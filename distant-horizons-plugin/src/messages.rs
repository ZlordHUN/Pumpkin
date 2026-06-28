//! Distant Horizons network protocol constants and message (de)serialization.
//!
//! Reference: `distant-horizons-main/.../coreSubProjects/core/.../network/messages/`
//! and `coreapi/ModInfo.java`. All values are transmitted big-endian.

use crate::codec::{Reader, Writer};

/// DH network protocol version (`ModInfo.PROTOCOL_VERSION`). Must prefix every payload.
pub const PROTOCOL_VERSION: u16 = 15;

/// The single Minecraft custom-payload channel DH registers in the PLAY phase
/// (`RESOURCE_NAMESPACE` + ":" + `WRAPPER_PACKET_PATH`).
pub const CHANNEL: &str = "distant_horizons:msg";

// Message IDs, assigned by `MessageRegistry` in registration order (id = index + 1).
pub const LEVEL_INIT_ID: u16 = 2;
pub const REQUEST_LEVEL_INIT_ID: u16 = 3;
pub const SESSION_CONFIG_ID: u16 = 4;

/// Wraps a message body in the outer DH frame: `[protocol version: u16][message id: u16][body]`.
pub fn frame(message_id: u16, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
    out.extend_from_slice(&message_id.to_be_bytes());
    out.extend_from_slice(body);
    out
}

/// Server capabilities advertised to the client via `SessionConfigMessage`.
///
/// Field order MUST match DH's `SessionConfig` insertion order (it is serialized as a
/// fixed-length collection with no count prefix).
///
/// The derived `Default` (all `false`/`0`) is the v1 milestone config: it completes the
/// DH handshake so a client recognises Pumpkin as a DH-capable server, while distant
/// generation, real-time updates and on-load sync stay disabled until LOD data serving
/// is implemented.
#[derive(Default)]
pub struct ServerSessionConfig {
    pub enable_distant_generation: bool,
    pub max_generation_request_distance: i32,
    pub generation_center_chunk_x: i32,
    pub generation_center_chunk_z: i32,
    pub generation_max_chunk_radius: i32,
    pub generation_request_rate_limit: i32,
    pub enable_real_time_updates: bool,
    pub real_time_update_distance_radius_in_chunks: i32,
    pub synchronize_on_load: bool,
    pub max_sync_on_load_request_distance: i32,
    pub sync_on_load_rate_limit: i32,
    pub player_bandwidth_limit: i32,
}

/// Decodes the client's `RequestLevelInitMessage` (message id 3): a single dimension string.
pub fn decode_request_level_init(reader: &mut Reader) -> Option<String> {
    reader.read_string()
}

/// Encodes a `LevelInitMessage` body (message id 2): dimension, server key, level key, server time.
pub fn encode_level_init_body(
    dimension: &str,
    server_key: &str,
    level_key: &str,
    server_time: i64,
) -> Vec<u8> {
    let mut writer = Writer::new();
    writer.write_string(dimension);
    writer.write_string(server_key);
    writer.write_string(level_key);
    writer.write_i64(server_time);
    writer.into_vec()
}

/// Encodes a `SessionConfigMessage` body (message id 4) from the server's config.
pub fn encode_session_config_body(config: &ServerSessionConfig) -> Vec<u8> {
    let mut writer = Writer::new();
    writer.write_bool(config.enable_distant_generation);
    writer.write_i32(config.max_generation_request_distance);
    writer.write_i32(config.generation_center_chunk_x);
    writer.write_i32(config.generation_center_chunk_z);
    writer.write_i32(config.generation_max_chunk_radius);
    writer.write_i32(config.generation_request_rate_limit);
    writer.write_bool(config.enable_real_time_updates);
    writer.write_i32(config.real_time_update_distance_radius_in_chunks);
    writer.write_bool(config.synchronize_on_load);
    writer.write_i32(config.max_sync_on_load_request_distance);
    writer.write_i32(config.sync_on_load_rate_limit);
    writer.write_i32(config.player_bandwidth_limit);
    writer.into_vec()
}

/// Decodes and discards a client `SessionConfigMessage` body (the server currently ignores
/// the client's constraints and replies with its own config).
pub fn skip_session_config(reader: &mut Reader) -> Option<()> {
    // 3 booleans + 9 i32s, in the fixed SessionConfig order.
    reader.read_bool()?;
    reader.read_i32()?;
    reader.read_i32()?;
    reader.read_i32()?;
    reader.read_i32()?;
    reader.read_i32()?;
    reader.read_bool()?;
    reader.read_i32()?;
    reader.read_bool()?;
    reader.read_i32()?;
    reader.read_i32()?;
    reader.read_i32()?;
    Some(())
}
