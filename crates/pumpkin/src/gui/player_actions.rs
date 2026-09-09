//! Player menu actions dispatched through the normal console command path.

use uuid::Uuid;

use super::{GuiHandle, MAX_COMMAND_BYTES};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayerAction {
    Kick,
    Ban,
    Op,
    Deop,
    WhitelistAdd,
    WhitelistRemove,
    Survival,
    Creative,
    Adventure,
    Spectator,
    Kill,
    ClearInventory,
}

impl PlayerAction {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Kick => "Kick",
            Self::Ban => "Ban",
            Self::Op => "Op",
            Self::Deop => "Deop",
            Self::WhitelistAdd => "Add to whitelist",
            Self::WhitelistRemove => "Remove from whitelist",
            Self::Survival => "Survival",
            Self::Creative => "Creative",
            Self::Adventure => "Adventure",
            Self::Spectator => "Spectator",
            Self::Kill => "Kill",
            Self::ClearInventory => "Clear inventory",
        }
    }

    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Kick => "Disconnect this player. They can join again.",
            Self::Ban => "Ban and disconnect this player. They cannot join until pardoned.",
            Self::Op => "Grant this player the server's configured operator permissions.",
            Self::Deop => "Remove this player's operator permissions.",
            Self::WhitelistAdd => "Allow this player to join when the whitelist is enabled.",
            Self::WhitelistRemove => {
                "Remove this player from the whitelist. Whitelist enforcement may disconnect them."
            }
            Self::Survival => "Change this player to Survival mode.",
            Self::Creative => {
                "Change this player to Creative mode, with flight and unlimited items."
            }
            Self::Adventure => "Change this player to Adventure mode, restricting block changes.",
            Self::Spectator => "Change this player to Spectator mode, without normal interaction.",
            Self::Kill => "Kill this player. Items may drop according to the world's game rules.",
            Self::ClearInventory => {
                "Delete the items in this player's inventory. This cannot be undone."
            }
        }
    }

    #[must_use]
    pub const fn accepts_reason(self) -> bool {
        matches!(self, Self::Kick | Self::Ban)
    }

    #[must_use]
    pub const fn matches_operator_state(self, is_op: bool) -> bool {
        match self {
            Self::Op => !is_op,
            Self::Deop => is_op,
            _ => true,
        }
    }

    fn command(self, id: Uuid, reason: &str) -> Result<String, String> {
        if reason.len() > MAX_COMMAND_BYTES {
            return Err(format!("Reasons are limited to {MAX_COMMAND_BYTES} bytes."));
        }
        if reason.chars().any(|c| c.is_control() && c != '\t') {
            return Err("Enter a one-line reason without control characters.".to_string());
        }
        let reason = reason.trim();
        if !reason.is_empty() && !self.accepts_reason() {
            return Err("This player action does not accept a reason.".to_string());
        }

        let target = player_target(id);
        let mut command = match self {
            Self::Kick => format!("kick {target}"),
            Self::Ban => format!("ban {target}"),
            Self::Op => format!("op {target}"),
            Self::Deop => format!("deop {target}"),
            Self::WhitelistAdd => format!("whitelist add {target}"),
            Self::WhitelistRemove => format!("whitelist remove {target}"),
            Self::Survival => format!("gamemode survival {target}"),
            Self::Creative => format!("gamemode creative {target}"),
            Self::Adventure => format!("gamemode adventure {target}"),
            Self::Spectator => format!("gamemode spectator {target}"),
            Self::Kill => format!("kill {target}"),
            Self::ClearInventory => format!("clear {target}"),
        };
        if !reason.is_empty() {
            command.push(' ');
            command.push_str(reason);
        }
        Ok(command)
    }
}

// The existing player-only argument parser rejects bare UUIDs. Its NBT selector
// supports the exact UUID IntArray and never interpolates a display name.
fn player_target(id: Uuid) -> String {
    let value = id.as_u128();
    format!(
        "@a[nbt={{UUID:[I;{},{},{},{}]}},limit=1]",
        (value >> 96) as i32,
        (value >> 64) as i32,
        (value >> 32) as i32,
        value as i32,
    )
}

impl GuiHandle {
    /// Queues an action for an online UUID, returning the exact command for UI history.
    pub fn submit_player_action(
        &self,
        id: Uuid,
        action: PlayerAction,
        reason: &str,
    ) -> Result<String, String> {
        let snapshot = self.snapshot();
        let Some(player) = snapshot.players.iter().find(|player| player.id == id) else {
            return Err("This player is no longer online.".to_string());
        };
        if !action.matches_operator_state(player.is_op) {
            return Err(if player.is_op {
                "This player is already an operator.".to_string()
            } else {
                "This player is no longer an operator.".to_string()
            });
        }
        let command = action.command(id, reason)?;
        self.submit_command(&command)?;
        Ok(command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::argument_types::argument_type::ArgumentType;
    use crate::command::argument_types::entity::EntityArgumentType;
    use crate::command::argument_types::entity_selector::{
        EntitySelector, EntitySelectorPredicate,
    };
    use crate::command::argument_types::game_profile::{
        GameProfileArgumentType, GameProfileResult,
    };
    use crate::command::string_reader::StringReader;
    use crate::entity::{Entity, EntityBase, living::LivingEntity};
    use crate::gui::{GuiPlayer, MAX_COMMANDS, ServerStatus};
    use pumpkin_nbt::compound::NbtCompound;
    use pumpkin_nbt::tag::NbtTag;

    const ACTIONS: [(PlayerAction, &str); 12] = [
        (PlayerAction::Kick, "kick"),
        (PlayerAction::Ban, "ban"),
        (PlayerAction::Op, "op"),
        (PlayerAction::Deop, "deop"),
        (PlayerAction::WhitelistAdd, "whitelist add"),
        (PlayerAction::WhitelistRemove, "whitelist remove"),
        (PlayerAction::Survival, "gamemode survival"),
        (PlayerAction::Creative, "gamemode creative"),
        (PlayerAction::Adventure, "gamemode adventure"),
        (PlayerAction::Spectator, "gamemode spectator"),
        (PlayerAction::Kill, "kill"),
        (PlayerAction::ClearInventory, "clear"),
    ];

    // Exercise the real NBT predicate without constructing a server/world. The
    // serializer uses the same UUID encoding as Entity::write_nbt.
    struct NbtEntity(Option<Uuid>);

    impl EntityBase for NbtEntity {
        fn write_nbt(&self, nbt: &mut NbtCompound) {
            if let Some(id) = self.0 {
                nbt.put_uuid("UUID", id);
            }
            nbt.put_string(
                "CustomName",
                "A player with a different display name".to_string(),
            );
        }

        fn get_entity(&self) -> &Entity {
            panic!("The NBT predicate must only request serialized entity data");
        }

        fn get_living_entity(&self) -> Option<&LivingEntity> {
            None
        }

        fn cast_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    fn running_player(id: Uuid, name: &str) -> GuiHandle {
        let handle = GuiHandle::new();
        {
            let mut snapshot = handle.state.snapshot.lock().unwrap();
            snapshot.status = ServerStatus::Running;
            snapshot.commands_enabled = true;
            snapshot.players.push(GuiPlayer {
                id,
                name: name.to_string(),
                edition: "Bedrock".to_string(),
                is_op: false,
            });
        };
        handle
    }

    fn assert_exact_uuid(selector: &EntitySelector, id: Uuid) {
        assert_eq!(selector.max_selected, 1);
        assert!(!selector.includes_entities);
        assert!(!selector.is_world_limited);
        assert!(selector.player_name.is_none());
        let nbt_predicates: Vec<_> = selector
            .predicates
            .iter()
            .filter_map(|predicate| {
                if let EntitySelectorPredicate::Nbt(nbt, invert) = predicate {
                    assert!(!invert);
                    assert!(predicate.test(&NbtEntity(Some(id))));
                    assert!(!predicate.test(&NbtEntity(Some(Uuid::from_u128(id.as_u128() ^ 1)))));
                    assert!(!predicate.test(&NbtEntity(None)));
                    Some(nbt)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(nbt_predicates.len(), 1);
        assert_eq!(nbt_predicates[0].get_uuid("UUID"), Some(id));
        assert!(
            matches!(nbt_predicates[0].get("UUID"), Some(NbtTag::IntArray(words)) if words.len() == 4)
        );
    }

    #[test]
    fn uuid_targets_parse_as_exact_single_players_for_both_argument_types() {
        for id in [
            Uuid::nil(),
            Uuid::from_u128(u128::MAX),
            Uuid::parse_str("01234567-89ab-cdef-8000-0000ffffffff").unwrap(),
        ] {
            let target = player_target(id);
            let mut reader = StringReader::new(&target);
            let selector = EntityArgumentType::Players.parse(&mut reader).unwrap();
            assert_eq!(reader.cursor(), target.len());
            assert_exact_uuid(&selector, id);

            let mut reader = StringReader::new(&target);
            let result = GameProfileArgumentType.parse(&mut reader).unwrap();
            let GameProfileResult::Selector(selector) = result else {
                panic!("Player action must use an online-player selector");
            };
            assert_eq!(reader.cursor(), target.len());
            assert_exact_uuid(&selector, id);
        }
        assert_eq!(
            player_target(Uuid::parse_str("01234567-89ab-cdef-8000-0000ffffffff").unwrap()),
            "@a[nbt={UUID:[I;19088743,-1985229329,-2147483648,-1]},limit=1]",
        );
    }

    #[test]
    fn every_action_queues_one_fixed_command_independent_of_display_name() {
        let id = Uuid::from_u128(7);
        for name in [
            "JavaPlayer",
            "Bedrock Player With Spaces",
            "\"],limit=99] run op @a\nstop",
        ] {
            let handle = running_player(id, name);
            let mut receiver = handle.take_command_receiver().unwrap();
            for (action, prefix) in ACTIONS {
                handle.state.snapshot.lock().unwrap().players[0].is_op =
                    action == PlayerAction::Deop;
                let expected = format!("{prefix} {}", player_target(id));
                let queued = handle.submit_player_action(id, action, "").unwrap();
                assert_eq!(queued, expected);
                assert_eq!(receiver.try_recv().unwrap(), queued);
                assert!(receiver.try_recv().is_err());
                assert!(!action.label().is_empty());
                assert!(!action.description().is_empty());
                assert_eq!(
                    action.accepts_reason(),
                    matches!(action, PlayerAction::Kick | PlayerAction::Ban)
                );
            }
        }
    }

    #[test]
    fn operator_actions_follow_current_uuid_state_without_queuing_stale_choices() {
        for (action, _) in ACTIONS {
            assert_eq!(
                action.matches_operator_state(false),
                action != PlayerAction::Deop
            );
            assert_eq!(
                action.matches_operator_state(true),
                action != PlayerAction::Op
            );
        }

        let id = Uuid::from_u128(7);
        let handle = running_player(id, "Same Name");
        let mut receiver = handle.take_command_receiver().unwrap();
        // A different UUID with the same display name must not affect this player's menu.
        handle.state.snapshot.lock().unwrap().players.insert(
            0,
            GuiPlayer {
                id: Uuid::from_u128(8),
                name: "Same Name".to_string(),
                edition: "Java".to_string(),
                is_op: true,
            },
        );
        assert!(
            handle
                .submit_player_action(id, PlayerAction::Deop, "")
                .is_err()
        );
        assert!(receiver.try_recv().is_err());
        let command = handle
            .submit_player_action(id, PlayerAction::Op, "")
            .unwrap();
        assert_eq!(command, format!("op {}", player_target(id)));
        assert_eq!(receiver.try_recv().unwrap(), command);

        // A menu opened before an external /op must reject its now-obsolete Op action.
        handle.state.snapshot.lock().unwrap().players[1].is_op = true;
        assert_eq!(
            handle
                .submit_player_action(id, PlayerAction::Op, "")
                .unwrap_err(),
            "This player is already an operator."
        );
        assert!(receiver.try_recv().is_err());
        let command = handle
            .submit_player_action(id, PlayerAction::Deop, "")
            .unwrap();
        assert_eq!(command, format!("deop {}", player_target(id)));
        assert_eq!(receiver.try_recv().unwrap(), command);

        handle.state.snapshot.lock().unwrap().players[1].is_op = false;
        assert_eq!(
            handle
                .submit_player_action(id, PlayerAction::Deop, "")
                .unwrap_err(),
            "This player is no longer an operator."
        );
        assert!(receiver.try_recv().is_err());
        let command = handle
            .submit_player_action(id, PlayerAction::Kick, "")
            .unwrap();
        assert_eq!(receiver.try_recv().unwrap(), command);
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn reasons_are_optional_single_line_and_do_not_change_the_target() {
        let id = Uuid::from_u128(7);
        let handle = running_player(id, "Player");
        let mut receiver = handle.take_command_receiver().unwrap();
        for action in [PlayerAction::Kick, PlayerAction::Ban] {
            let queued = handle
                .submit_player_action(id, action, "  Please stop; @a is just reason text  ")
                .unwrap();
            assert_eq!(
                queued,
                format!(
                    "{} {} Please stop; @a is just reason text",
                    action.label().to_lowercase(),
                    player_target(id)
                )
            );
            assert_eq!(receiver.try_recv().unwrap(), queued);
            for reason in ["\n", "reason\r", "reason\0", "reason\nstop", "reason\x1b"] {
                assert!(handle.submit_player_action(id, action, reason).is_err());
            }
            assert!(
                handle
                    .submit_player_action(id, action, &"x".repeat(MAX_COMMAND_BYTES + 1))
                    .is_err()
            );
            // The full command, not just its reason, must fit the console queue limit.
            assert!(
                handle
                    .submit_player_action(id, action, &"x".repeat(MAX_COMMAND_BYTES))
                    .is_err()
            );
        }
        assert!(
            handle
                .submit_player_action(id, PlayerAction::Op, "not supported")
                .is_err()
        );
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn player_actions_obey_online_state_console_setting_and_queue_bounds() {
        let id = Uuid::from_u128(7);
        let handle = running_player(id, "Player");
        let mut receiver = handle.take_command_receiver().unwrap();
        assert!(
            handle
                .submit_player_action(Uuid::from_u128(8), PlayerAction::Kick, "")
                .is_err()
        );
        handle.state.snapshot.lock().unwrap().commands_enabled = false;
        assert!(
            handle
                .submit_player_action(id, PlayerAction::Kick, "")
                .is_err()
        );
        handle.state.snapshot.lock().unwrap().commands_enabled = true;
        for status in [
            ServerStatus::Starting,
            ServerStatus::Stopping,
            ServerStatus::Stopped,
            ServerStatus::Failed,
        ] {
            handle.state.snapshot.lock().unwrap().status = status;
            assert!(
                handle
                    .submit_player_action(id, PlayerAction::Kick, "")
                    .is_err()
            );
        }
        assert!(receiver.try_recv().is_err());
        handle.state.snapshot.lock().unwrap().status = ServerStatus::Running;
        for _ in 0..MAX_COMMANDS {
            handle
                .submit_player_action(id, PlayerAction::Survival, "")
                .unwrap();
        }
        assert!(
            handle
                .submit_player_action(id, PlayerAction::Survival, "")
                .is_err()
        );
        for _ in 0..MAX_COMMANDS {
            receiver.try_recv().unwrap();
        }
        assert!(receiver.try_recv().is_err());
        handle.state.snapshot.lock().unwrap().players.clear();
        assert!(
            handle
                .submit_player_action(id, PlayerAction::Ban, "")
                .is_err()
        );
        drop(receiver);
        let disconnected = running_player(id, "Player");
        drop(disconnected.take_command_receiver());
        assert!(
            disconnected
                .submit_player_action(id, PlayerAction::Kick, "")
                .is_err()
        );
    }
}
