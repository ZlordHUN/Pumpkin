use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use pumpkin_protocol::{
    codec::var_int::VarInt,
    java::client::play::{Animation, CEntityAnimation},
};
use uuid::Uuid;

use crate::entity::player::Player;

const MIN_INTERVAL_TICKS: i32 = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JavaEmote {
    Wave,
    Clap,
    Point,
    FollowMe,
    Fallback,
}

impl JavaEmote {
    const fn from_id(id: Uuid) -> Self {
        match id.as_u128() {
            0x4c8a_e710_df2e_47cd_814d_cc7b_f21a_3d67 => Self::Wave,
            0x9a46_9a61_c83b_4ba9_b507_bdbe_6443_0582 => Self::Clap,
            0xce5c_0300_7f03_455d_aaf1_352e_4927_b54d => Self::Point,
            0x1742_8c4c_3813_4ea1_b3a9_d6a3_2f83_afca => Self::FollowMe,
            _ => Self::Fallback,
        }
    }

    /// Delays are relative to the preceding frame. Java does not expose the
    /// Bedrock skeleton, so these compact arm sequences approximate the four
    /// emotes bundled with the Bedrock client.
    const fn frames(self) -> &'static [(u64, Animation)] {
        use Animation::{SwingMainArm as Main, SwingOffhand as Off};

        match self {
            Self::Wave => &[(0, Main), (250, Main), (250, Main), (250, Main)],
            Self::Clap => &[
                (0, Main),
                (0, Off),
                (300, Main),
                (0, Off),
                (300, Main),
                (0, Off),
            ],
            Self::Point | Self::Fallback => &[(0, Main)],
            Self::FollowMe => &[(0, Main), (300, Main), (300, Main)],
        }
    }
}

pub fn broadcast(player: &Arc<Player>, emote_id: &str) {
    let Ok(emote_id) = Uuid::parse_str(emote_id) else {
        return;
    };
    let tick = player.tick_counter.load(Ordering::Relaxed);
    let previous_tick = player.last_java_emote_tick.swap(tick, Ordering::Relaxed);
    if tick.saturating_sub(previous_tick) < MIN_INTERVAL_TICKS {
        return;
    }

    let sequence = player
        .java_emote_sequence
        .fetch_add(1, Ordering::Relaxed)
        .wrapping_add(1);
    let entity_id = VarInt(player.entity_id());
    let player_id = player.gameprofile.id;
    let frames = JavaEmote::from_id(emote_id).frames();
    let world = player.world();

    world.broadcast_packet_except(&[player_id], &CEntityAnimation::new(entity_id, frames[0].1));

    if frames.len() == 1 {
        return;
    }

    let player = Arc::clone(player);
    let client = Arc::clone(&player.client);
    let _ = client.spawn_task(async move {
        for &(delay_ms, animation) in &frames[1..] {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            if player.java_emote_sequence.load(Ordering::Relaxed) != sequence
                || player.client.closed()
            {
                return;
            }
            world.broadcast_packet_except(
                &[player_id],
                &CEntityAnimation::new(entity_id, animation),
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_bedrock_builtin_emotes() {
        for (id, expected) in [
            ("4c8ae710-df2e-47cd-814d-cc7bf21a3d67", JavaEmote::Wave),
            ("9a469a61-c83b-4ba9-b507-bdbe64430582", JavaEmote::Clap),
            ("ce5c0300-7f03-455d-aaf1-352e4927b54d", JavaEmote::Point),
            ("17428c4c-3813-4ea1-b3a9-d6a32f83afca", JavaEmote::FollowMe),
        ] {
            assert_eq!(JavaEmote::from_id(Uuid::parse_str(id).unwrap()), expected);
        }
    }

    #[test]
    fn unknown_emotes_have_a_bounded_fallback() {
        assert_eq!(JavaEmote::from_id(Uuid::nil()), JavaEmote::Fallback);
        assert_eq!(JavaEmote::Fallback.frames().len(), 1);
        assert!(JavaEmote::Clap.frames().len() <= 8);
    }
}
