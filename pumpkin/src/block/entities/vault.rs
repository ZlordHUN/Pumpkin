//! Vault block entity for trial chambers. Awards loot to each player once
//! using a trial key. Tracks which players have already used the vault.
//! Has visual states: inactive, active, unlocking, ejecting.

use crate::block::entities::BlockEntity;
use crate::world::World;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_nbt::tag::NbtTag;
use pumpkin_util::math::position::BlockPos;
use std::any::Any;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use uuid::Uuid;

const STATE_INACTIVE: i32 = 0;
const STATE_ACTIVE: i32 = 1;
const STATE_UNLOCKING: i32 = 2;
const STATE_EJECTING: i32 = 3;

pub struct VaultBlockEntity {
    pub position: BlockPos,

    /// Current visual state of the vault.
    pub state: AtomicI32,
    /// Whether this is an ominous vault (uses ominous trial key).
    pub ominous: AtomicBool,
    /// Set of player UUIDs that have already used this vault.
    pub rewarded_players: Arc<tokio::sync::Mutex<Vec<Uuid>>>,
    /// Timer for the current state transition.
    pub state_timer: AtomicI32,

    /// Reward loot table to dispense (simplified to direct items for now).
    pub rewards: Arc<tokio::sync::Mutex<Vec<ItemStack>>>,
}

impl VaultBlockEntity {
    pub const ID: &'static str = "minecraft:vault";
    pub const UNLOCKING_TIME: i32 = 14; // ticks for unlock animation
    pub const EJECTING_TIME: i32 = 20; // ticks for item ejection
    pub const DETECTION_RANGE: f64 = 5.0;

    #[must_use]
    pub fn new(position: BlockPos) -> Self {
        Self {
            position,
            state: AtomicI32::new(STATE_INACTIVE),
            ominous: AtomicBool::new(false),
            rewarded_players: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            state_timer: AtomicI32::new(0),
            rewards: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        }
    }

    /// Attempts to unlock the vault with a trial key for the given player.
    /// Returns true if the unlock was successful (player hasn't used it before).
    pub async fn try_unlock(&self, player_uuid: Uuid, key_item: &Item) -> bool {
        // Check if the key matches the vault type
        let is_ominous = self.ominous.load(Ordering::Relaxed);
        let expected_key = if is_ominous {
            &Item::OMINOUS_TRIAL_KEY
        } else {
            &Item::TRIAL_KEY
        };

        if key_item != expected_key {
            return false;
        }

        // Check if this player has already used this vault
        let rewarded = self.rewarded_players.lock().await;
        if rewarded.contains(&player_uuid) {
            return false;
        }

        true
    }

    /// Marks a player as having used this vault and starts the ejection sequence.
    pub async fn mark_rewarded(&self, player_uuid: Uuid) {
        self.rewarded_players.lock().await.push(player_uuid);
        self.state.store(STATE_UNLOCKING, Ordering::Relaxed);
        self.state_timer
            .store(Self::UNLOCKING_TIME, Ordering::Relaxed);
    }

    /// Ejects loot for the player who just unlocked.
    async fn eject_loot(&self, world: &Arc<World>) {
        let rewards = self.rewards.lock().await;
        for item in rewards.iter() {
            world.drop_stack(&self.position, item.clone()).await;
        }

        // Also grant a default reward if the list is empty
        if rewards.is_empty() {
            // Default vault reward: emeralds, iron, diamonds, etc.
            let default_rewards: [(&Item, u8); 5] = [
                (&Item::EMERALD, 4),
                (&Item::IRON_INGOT, 3),
                (&Item::GOLDEN_APPLE, 1),
                (&Item::DIAMOND, 2),
                (&Item::IRON_NAUTILUS_ARMOR, 1),
            ];

            let idx = rand::random_range(0..default_rewards.len());
            let (item, count) = default_rewards[idx];
            world
                .drop_stack(&self.position, ItemStack::new(count, item))
                .await;
        }
    }

    /// Adds a reward to the vault's loot pool.
    pub async fn add_reward(&self, stack: ItemStack) {
        self.rewards.lock().await.push(stack);
    }
}

impl BlockEntity for VaultBlockEntity {
    fn resource_location(&self) -> &'static str {
        Self::ID
    }

    fn get_position(&self) -> BlockPos {
        self.position
    }

    fn tick<'a>(&'a self, world: &'a Arc<World>) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            let state = self.state.load(Ordering::Relaxed);

            match state {
                STATE_INACTIVE => {
                    // Check for nearby players to show active state
                    let players = world.get_players_by_pos(self.position);
                    let detection_sq = Self::DETECTION_RANGE * Self::DETECTION_RANGE;
                    let pos = self.position.0;
                    let has_nearby = players.iter().any(|p| {
                        let ppos = p.living_entity.entity.pos.load();
                        let dx = ppos.x - pos.x as f64;
                        let dy = ppos.y - pos.y as f64;
                        let dz = ppos.z - pos.z as f64;
                        dx * dx + dy * dy + dz * dz <= detection_sq
                    });

                    if has_nearby {
                        self.state.store(STATE_ACTIVE, Ordering::Relaxed);
                    }
                }

                STATE_ACTIVE => {
                    // Stay active while players are near
                    let players = world.get_players_by_pos(self.position);
                    let detection_sq = Self::DETECTION_RANGE * Self::DETECTION_RANGE;
                    let pos = self.position.0;
                    let has_nearby = players.iter().any(|p| {
                        let ppos = p.living_entity.entity.pos.load();
                        let dx = ppos.x - pos.x as f64;
                        let dy = ppos.y - pos.y as f64;
                        let dz = ppos.z - pos.z as f64;
                        dx * dx + dy * dy + dz * dz <= detection_sq
                    });

                    if !has_nearby {
                        self.state.store(STATE_INACTIVE, Ordering::Relaxed);
                    }
                }

                STATE_UNLOCKING => {
                    let timer = self.state_timer.load(Ordering::Relaxed);
                    if timer > 0 {
                        self.state_timer.store(timer - 1, Ordering::Relaxed);
                    } else {
                        self.state.store(STATE_EJECTING, Ordering::Relaxed);
                        self.state_timer
                            .store(Self::EJECTING_TIME, Ordering::Relaxed);
                    }
                }

                STATE_EJECTING => {
                    let timer = self.state_timer.load(Ordering::Relaxed);
                    if timer > 0 {
                        self.state_timer.store(timer - 1, Ordering::Relaxed);
                    } else {
                        self.eject_loot(world).await;
                        self.state.store(STATE_INACTIVE, Ordering::Relaxed);
                    }
                }

                _ => {}
            }
        })
    }

    fn write_nbt<'a>(
        &'a self,
        nbt: &'a mut NbtCompound,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            nbt.put_string("id", self.resource_location().to_string());
            let pos = self.get_position();
            nbt.put_int("x", pos.0.x);
            nbt.put_int("y", pos.0.y);
            nbt.put_int("z", pos.0.z);
            nbt.put_bool("ominous", self.ominous.load(Ordering::Relaxed));

            // Write rewarded players as UUID int array
            let rewarded = self.rewarded_players.lock().await;
            let mut uuid_ints: Vec<i32> = Vec::with_capacity(rewarded.len() * 4);
            for uuid in rewarded.iter() {
                let bytes = uuid.as_u64_pair();
                uuid_ints.push((bytes.0 >> 32) as i32);
                uuid_ints.push(bytes.0 as i32);
                uuid_ints.push((bytes.1 >> 32) as i32);
                uuid_ints.push(bytes.1 as i32);
            }
            // Store as int array (vanilla uses "rewarded_players" list)
            let uuid_tags: Vec<NbtTag> = rewarded
                .iter()
                .map(|u| {
                    NbtTag::IntArray({
                        let bytes = u.as_u64_pair();
                        vec![
                            (bytes.0 >> 32) as i32,
                            bytes.0 as i32,
                            (bytes.1 >> 32) as i32,
                            bytes.1 as i32,
                        ]
                    })
                })
                .collect();
            nbt.put(
                "server_data",
                NbtTag::Compound({
                    let mut c = NbtCompound::new();
                    c.put("rewarded_players", NbtTag::List(uuid_tags));
                    c
                }),
            );
        })
    }

    fn from_nbt(nbt: &NbtCompound, position: BlockPos) -> Self
    where
        Self: Sized,
    {
        let ominous = nbt.get_bool("ominous").unwrap_or(false);

        let mut rewarded_players = Vec::new();
        if let Some(server_data) = nbt.get_compound("server_data") {
            if let Some(rewarded_list) = server_data.get_list("rewarded_players") {
                for tag in rewarded_list {
                    if let NbtTag::IntArray(arr) = tag {
                        if arr.len() >= 4 {
                            let uuid = Uuid::from_u64_pair(
                                ((arr[0] as u64) << 32) | (arr[1] as u32 as u64),
                                ((arr[2] as u64) << 32) | (arr[3] as u32 as u64),
                            );
                            rewarded_players.push(uuid);
                        }
                    }
                }
            }
        }

        Self {
            position,
            state: AtomicI32::new(STATE_INACTIVE),
            ominous: AtomicBool::new(ominous),
            rewarded_players: Arc::new(tokio::sync::Mutex::new(rewarded_players)),
            state_timer: AtomicI32::new(0),
            rewards: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
