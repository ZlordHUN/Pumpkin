#[allow(clippy::wildcard_imports)]
use super::*;
use pumpkin_protocol::java::server::play::{PlayResourcePackResult, SPlayResourcePack};
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

        if self
            .bedrock_skin_pack
            .load_full()
            .is_some_and(|pack| pack.id == packet.uuid)
        {
            if !matches!(
                result,
                PlayResourcePackResult::DownloadSuccess
                    | PlayResourcePackResult::Accepted
                    | PlayResourcePackResult::Downloaded
            ) {
                self.bedrock_skin_pack.store(None);
                let player_c = player.clone();
                player.spawn_task(async move {
                    player_c
                        .world()
                        .refresh_bedrock_players_for_java(&player_c)
                        .await;
                });
            }
            return;
        }

        let config = &server.advanced_config.resource_pack.java;
        let configured_id = uuid::Uuid::new_v3(&uuid::Uuid::NAMESPACE_DNS, config.url.as_bytes());
        let configured_response = packet.uuid == configured_id
            || (self.version.load() < JavaMinecraftVersion::V_1_20_3 && packet.uuid.is_nil());
        if config.enabled
            && config.force
            && configured_response
            && (result == PlayResourcePackResult::Declined
                || result == PlayResourcePackResult::DownloadFail)
        {
            self.try_kick(&TextComponent::text(
                "You must accept the resource pack to play on this server.",
            ));
        }
    }
}
