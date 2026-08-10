#[allow(clippy::wildcard_imports)]
use super::*;
use pumpkin_protocol::java::{
    client::play::CRemoveResourcePack,
    server::play::{PlayResourcePackResult, SPlayResourcePack},
};
use pumpkin_util::{text::TextComponent, version::JavaMinecraftVersion};

use crate::plugin::api::events::player::player_resource_pack_status::PlayerResourcePackStatusEvent;

impl JavaClient {
    pub fn handle_play_resource_pack_response(
        &self,
        server: &Arc<Server>,
        player: &Arc<Player>,
        packet: &SPlayResourcePack,
    ) {
        let result = packet.response_result();
        debug!(
            "Player {} resource pack response for {}: {:?}",
            player.gameprofile.name, packet.uuid, result
        );

        let mut event = PlayerResourcePackStatusEvent::new(
            player.clone(),
            packet.uuid.to_string(),
            format!("{result:?}"),
        );
        server.plugin_manager.fire_blocking(server, &mut event);

        let config = &server.advanced_config.resource_pack.java;
        let configured_id = uuid::Uuid::new_v3(&uuid::Uuid::NAMESPACE_DNS, config.url.as_bytes());
        let configured_response = packet.uuid == configured_id
            || (self.version.load() < JavaMinecraftVersion::V_1_20_3 && packet.uuid.is_nil());
        if config.enabled && configured_response {
            if config.force
                && matches!(
                    result,
                    PlayResourcePackResult::Declined | PlayResourcePackResult::DownloadFail
                )
            {
                self.try_kick(&TextComponent::text(
                    "You must accept the resource pack to play on this server.",
                ));
            }
            return;
        }

        let player_c = player.clone();
        let server_c = server.clone();
        let id = packet.uuid;
        player.spawn_task(async move {
            if let Some(client) = player_c.client.java() {
                client
                    .handle_bedrock_skin_pack_response(&server_c, &player_c, id, result)
                    .await;
            }
        });
    }

    async fn handle_bedrock_skin_pack_response(
        &self,
        server: &Server,
        player: &Player,
        id: uuid::Uuid,
        result: PlayResourcePackResult,
    ) {
        let mut pending = self.pending_bedrock_skin_pack.lock().await;
        if self.is_closed() {
            return;
        }
        let in_progress = matches!(
            result,
            PlayResourcePackResult::Accepted | PlayResourcePackResult::Downloaded
        );
        if pending.as_ref().is_some_and(|pack| pack.id == id) {
            if in_progress {
                return;
            }
            // Take only this session's outstanding offer, never an arbitrary pack
            // from the global registry or a duplicate response to an older offer.
            let pack = pending.take().expect("matching pending skin pack");
            if result == PlayResourcePackResult::DownloadSuccess {
                let previous = self.bedrock_skin_pack.swap(Some(pack));
                if let Some(previous) = previous
                    && previous.id != id
                {
                    self.enqueue_client_packet(&CRemoveResourcePack::new(Some(&previous.id)))
                        .await;
                }
                player
                    .world()
                    .refresh_bedrock_players_for_java(player)
                    .await;
            }
        } else if self
            .bedrock_skin_pack
            .load_full()
            .is_some_and(|pack| pack.id == id)
            && !in_progress
            && result != PlayResourcePackResult::DownloadSuccess
        {
            self.bedrock_skin_pack.store(None);
            player
                .world()
                .refresh_bedrock_players_for_java(player)
                .await;
            return;
        } else {
            return;
        }

        // Coalesce changes that arrived during download into one latest offer.
        drop(pending);
        if let Some(latest) = server.bedrock_skin_packs.current().await
            && latest.id != id
        {
            self.push_bedrock_skin_pack(server, latest).await;
        }
    }
}
