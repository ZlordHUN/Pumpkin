//! Pumpkin server-side compatibility plugin for the Distant Horizons (DH) mod.
//!
//! Implements the DH network handshake on the `distant_horizons:msg` PLAY channel so a
//! DH client recognises Pumpkin as a DH-capable server (client state advances to FULL).
//!
//! v1 scope: handshake only (channel framing + `LevelInit`/`SessionConfig` exchange).
//! LOD data serving (`FullDataSourceRequest` handling) is a follow-up.

mod codec;
mod messages;

use std::time::{SystemTime, UNIX_EPOCH};

use pumpkin_plugin_api::{
    Server,
    events::{EventData, EventHandler, EventPriority, PlayerCustomPayloadEvent},
    Context, Plugin, PluginMetadata,
};
use tracing::{info, warn};

use codec::Reader;
use messages::{
    CHANNEL, LEVEL_INIT_ID, PROTOCOL_VERSION, REQUEST_LEVEL_INIT_ID, SESSION_CONFIG_ID,
    ServerSessionConfig, decode_request_level_init, encode_level_init_body,
    encode_session_config_body, frame, skip_session_config,
};

struct DistantHorizonsPlugin;

impl Plugin for DistantHorizonsPlugin {
    fn new() -> Self {
        DistantHorizonsPlugin
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: "Distant Horizons".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            authors: vec!["ZlordHUN".into()],
            description: "Server-side Distant Horizons (LOD) compatibility".into(),
            dependencies: vec![],
            permissions: vec![],
        }
    }

    fn on_load(&mut self, context: Context) -> pumpkin_plugin_api::Result<()> {
        context.register_event_handler(PayloadHandler, EventPriority::Normal, false)?;
        info!(
            "Distant Horizons plugin loaded; handshake active on channel {CHANNEL} (protocol {PROTOCOL_VERSION})"
        );
        Ok(())
    }
}

/// Handles incoming DH payloads from clients.
struct PayloadHandler;

impl EventHandler<PlayerCustomPayloadEvent> for PayloadHandler {
    fn handle(
        &self,
        _server: Server,
        event: EventData<PlayerCustomPayloadEvent>,
    ) -> EventData<PlayerCustomPayloadEvent> {
        if event.channel != CHANNEL {
            return event;
        }
        handle_payload(&event.player, &event.data);
        event
    }
}

fn handle_payload(player: &pumpkin_plugin_api::player::Player, data: &[u8]) {
    let mut reader = Reader::new(data);

    let Some(version) = reader.read_u16() else {
        warn!("received truncated DH payload");
        return;
    };
    if version != PROTOCOL_VERSION {
        warn!(
            "DH protocol version mismatch: client sent {version}, expected {PROTOCOL_VERSION}"
        );
        return;
    }

    let Some(message_id) = reader.read_u16() else {
        warn!("DH payload missing message id");
        return;
    };

    match message_id {
        REQUEST_LEVEL_INIT_ID => {
            let Some(dimension) = decode_request_level_init(&mut reader) else {
                warn!("malformed RequestLevelInit from client");
                return;
            };
            // serverKey/levelKey just namespace the client's cached LOD; reuse the dimension.
            let body = encode_level_init_body(&dimension, "pumpkin", &dimension, now_millis());
            send(player, LEVEL_INIT_ID, &body);
            info!("sent LevelInit for dimension {dimension}");
        }
        SESSION_CONFIG_ID => {
            // Client sent its config constraints; reply with the server's session config,
            // which is what advances the client to FULL DH support.
            let _ = skip_session_config(&mut reader);
            let body = encode_session_config_body(&ServerSessionConfig::default());
            send(player, SESSION_CONFIG_ID, &body);
            info!("sent server SessionConfig (client should now reach FULL support)");
        }
        other => {
            info!("ignoring DH client message id {other} (not handled in v1)");
        }
    }
}

fn send(player: &pumpkin_plugin_api::player::Player, message_id: u16, body: &[u8]) {
    // Custom payloads are Java-edition only; reach the java-player view from the player.
    if let Some(java) = player.as_java() {
        java.send_custom_payload(CHANNEL, &frame(message_id, body));
    } else {
        warn!("cannot send DH payload: player is not a Java edition client");
    }
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

pumpkin_plugin_api::register_plugin!(DistantHorizonsPlugin);
