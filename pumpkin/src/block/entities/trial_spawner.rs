//! Trial Spawner block entity. The core mechanic of trial chambers:
//! spawns mobs when a player approaches, awards loot when all mobs are
//! killed, then enters a cooldown period.
//!
//! State machine: INACTIVE → WAITING_FOR_PLAYERS → ACTIVE → WAITING_FOR_REWARD → COOLDOWN → INACTIVE

use crate::block::entities::BlockEntity;
use crate::world::World;
use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::world::WorldEvent;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_nbt::tag::NbtTag;
use pumpkin_util::math::boundingbox::{BoundingBox, EntityDimensions};
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use std::any::Any;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use uuid::Uuid;

/// Configuration for a trial spawner mob spawn entry.
#[derive(Clone)]
pub struct SpawnDataEntry {
    pub entity_type: &'static EntityType,
    pub nbt: Option<NbtCompound>,
    pub weight: i32,
}

/// Default spawn data for the trial spawner.
const DEFAULT_SPAWN_DATA: &[(&EntityType, i32)] = &[
    (&EntityType::ZOMBIE, 1),
    (&EntityType::HUSK, 1),
    (&EntityType::SLIME, 1),
];

pub struct TrialSpawnerBlockEntity {
    pub position: BlockPos,

    // === State ===
    /// Current state of the spawner (0=inactive, 1=waiting, 2=active, 3=reward, 4=cooldown).
    pub state: AtomicI32,
    /// Whether this is an ominous trial spawner.
    pub ominous: AtomicBool,

    // === Spawning ===
    /// Current delay before next spawn cycle (ticks).
    pub spawn_delay: AtomicI32,
    /// How many mobs to spawn per cycle.
    pub spawn_count: i32,
    /// Range around the spawner where mobs can appear.
    pub spawn_range: i32,
    /// The spawn data entries for this spawner.
    pub spawn_data: Vec<SpawnDataEntry>,
    /// Total mobs that need to be defeated this cycle.
    pub required_mobs: AtomicI32,
    /// Mobs currently alive that were spawned by this spawner.
    pub tracked_mobs: Arc<tokio::sync::Mutex<Vec<Uuid>>>,

    // === Player Tracking ===
    /// Players who completed this cycle and are on cooldown.
    pub cooldown_players: Arc<tokio::sync::Mutex<Vec<(Uuid, i32)>>>,

    // === Timing ===
    /// Ticks remaining in the current state.
    pub state_timer: AtomicI32,
    /// Total ticks for the cooldown period (30 min = 36000 ticks at 20 TPS).
    pub cooldown_length: i32,

    // === Rewards ===
    /// Whether rewards have been ejected this cycle.
    pub rewards_ejected: AtomicBool,

    // === Ominous ===
    /// Ominous spawn data for when the spawner is ominous.
    pub ominous_spawn_data: Vec<SpawnDataEntry>,
}

// State constants
const STATE_INACTIVE: i32 = 0;
const STATE_WAITING: i32 = 1;
const STATE_ACTIVE: i32 = 2;
const STATE_REWARD: i32 = 3;
const STATE_COOLDOWN: i32 = 4;

impl TrialSpawnerBlockEntity {
    pub const ID: &'static str = "minecraft:trial_spawner";

    pub const DEFAULT_SPAWN_DELAY: i32 = 40;
    pub const WAITING_PLAYER_TIME: i32 = 40;
    pub const REWARD_EJECT_TIME: i32 = 60;
    pub const COOLDOWN_LENGTH: i32 = 36000;
    pub const DEFAULT_SPAWN_RANGE: i32 = 4;
    pub const DEFAULT_SPAWN_COUNT: i32 = 3;
    pub const DETECTION_RANGE: f64 = 14.0;
    pub const REQUIRED_MOBS_PER_PLAYER: i32 = 6;

    #[must_use]
    pub fn new(position: BlockPos) -> Self {
        Self {
            position,
            state: AtomicI32::new(STATE_INACTIVE),
            ominous: AtomicBool::new(false),
            spawn_delay: AtomicI32::new(Self::DEFAULT_SPAWN_DELAY),
            spawn_count: Self::DEFAULT_SPAWN_COUNT,
            spawn_range: Self::DEFAULT_SPAWN_RANGE,
            spawn_data: DEFAULT_SPAWN_DATA
                .iter()
                .map(|&(et, w)| SpawnDataEntry {
                    entity_type: et,
                    nbt: None,
                    weight: w,
                })
                .collect(),
            required_mobs: AtomicI32::new(0),
            tracked_mobs: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            cooldown_players: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            state_timer: AtomicI32::new(0),
            cooldown_length: Self::COOLDOWN_LENGTH,
            rewards_ejected: AtomicBool::new(false),
            ominous_spawn_data: Vec::new(),
        }
    }

    fn active_spawn_data(&self) -> &[SpawnDataEntry] {
        if self.ominous.load(Ordering::Relaxed) && !self.ominous_spawn_data.is_empty() {
            &self.ominous_spawn_data
        } else {
            &self.spawn_data
        }
    }

    fn pick_random_spawn_entry(&self) -> Option<&SpawnDataEntry> {
        let data = self.active_spawn_data();
        if data.is_empty() {
            return None;
        }
        let total_weight: i32 = data.iter().map(|e| e.weight).sum();
        if total_weight <= 0 {
            return data.first();
        }
        let mut roll = rand::random_range(0..total_weight);
        for entry in data {
            roll -= entry.weight;
            if roll < 0 {
                return Some(entry);
            }
        }
        data.last()
    }

    async fn try_spawn_mob(&self, world: &Arc<World>) -> bool {
        let Some(entry) = self.pick_random_spawn_entry() else {
            return false;
        };
        let pos = self.position.0;
        let spawn_range = self.spawn_range as f64;

        for _ in 0..3 {
            let spawn_pos = Vector3::new(
                pos.x as f64 + (rand::random::<f64>() - 0.5) * spawn_range * 2.0 + 0.5,
                pos.y as f64 + rand::random::<f64>() * 3.0,
                pos.z as f64 + (rand::random::<f64>() - 0.5) * spawn_range * 2.0 + 0.5,
            );

            let dims = &EntityDimensions {
                width: entry.entity_type.dimension[0],
                height: entry.entity_type.dimension[1],
                eye_height: entry.entity_type.eye_height,
            };
            if !world.is_space_empty(BoundingBox::new_from_pos(
                spawn_pos.x,
                spawn_pos.y,
                spawn_pos.z,
                dims,
            )) {
                continue;
            }

            let entity = crate::entity::r#type::from_type(
                entry.entity_type,
                spawn_pos,
                world,
                Uuid::new_v4(),
            );
            let entity_uuid = entity.get_entity().entity_uuid;
            world.spawn_entity(entity).await;
            world.sync_world_event(WorldEvent::ParticlesMobblockSpawn, self.position, 0);

            self.tracked_mobs.lock().await.push(entity_uuid);
            return true;
        }
        false
    }

    async fn spawn_mob_wave(&self, world: &Arc<World>) -> i32 {
        let mut spawned = 0;
        for _ in 0..self.spawn_count {
            if self.try_spawn_mob(world).await {
                spawned += 1;
            }
        }
        spawned
    }

    fn has_player_nearby(&self, world: &Arc<World>) -> bool {
        // Use get_players_by_pos which returns players near a position
        let players = world.get_players_by_pos(self.position);
        let detection_sq = Self::DETECTION_RANGE * Self::DETECTION_RANGE;
        let pos = self.position.0;

        for player in &players {
            let player_pos = player.living_entity.entity.pos.load();
            let dx = player_pos.x - pos.x as f64;
            let dy = player_pos.y - pos.y as f64;
            let dz = player_pos.z - pos.z as f64;
            if dx * dx + dy * dy + dz * dz <= detection_sq {
                return true;
            }
        }
        false
    }

    fn player_count_nearby(&self, world: &Arc<World>) -> usize {
        let players = world.get_players_by_pos(self.position);
        let detection_sq = Self::DETECTION_RANGE * Self::DETECTION_RANGE;
        let pos = self.position.0;

        let mut count = 0;
        for player in &players {
            let player_pos = player.living_entity.entity.pos.load();
            let dx = player_pos.x - pos.x as f64;
            let dy = player_pos.y - pos.y as f64;
            let dz = player_pos.z - pos.z as f64;
            if dx * dx + dy * dy + dz * dz <= detection_sq {
                count += 1;
            }
        }
        count
    }

    async fn reset_for_new_cycle(&self) {
        self.state.store(STATE_INACTIVE, Ordering::Relaxed);
        self.required_mobs.store(0, Ordering::Relaxed);
        self.tracked_mobs.lock().await.clear();
        self.rewards_ejected.store(false, Ordering::Relaxed);
        self.state_timer.store(0, Ordering::Relaxed);
    }

    async fn eject_rewards(&self, world: &Arc<World>) {
        // Drop 1-2 trial keys (or ominous trial keys if ominous)
        let key_item = if self.ominous.load(Ordering::Relaxed) {
            &Item::OMINOUS_TRIAL_KEY
        } else {
            &Item::TRIAL_KEY
        };
        let key_count: u8 = rand::random_range(1..=2);
        world
            .drop_stack(&self.position, ItemStack::new(key_count, key_item))
            .await;

        // Drop bonus emeralds
        let emerald_count: u8 = rand::random_range(1..=6);
        world
            .drop_stack(
                &self.position,
                ItemStack::new(emerald_count, &Item::EMERALD),
            )
            .await;

        self.rewards_ejected.store(true, Ordering::Relaxed);
    }

    pub async fn on_mob_killed(&self, mob_uuid: Uuid, _world: &Arc<World>) {
        let mut tracked = self.tracked_mobs.lock().await;
        if let Some(pos) = tracked.iter().position(|u| *u == mob_uuid) {
            tracked.remove(pos);
            let remaining = self.required_mobs.fetch_sub(1, Ordering::Relaxed) - 1;
            if remaining <= 0 {
                self.state.store(STATE_REWARD, Ordering::Relaxed);
                self.state_timer
                    .store(Self::REWARD_EJECT_TIME, Ordering::Relaxed);
            }
        }
    }
}

impl BlockEntity for TrialSpawnerBlockEntity {
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
                    if self.has_player_nearby(world) {
                        self.state.store(STATE_WAITING, Ordering::Relaxed);
                        self.state_timer
                            .store(Self::WAITING_PLAYER_TIME, Ordering::Relaxed);
                    }
                }

                STATE_WAITING => {
                    let timer = self.state_timer.load(Ordering::Relaxed);
                    if timer > 0 {
                        self.state_timer.store(timer - 1, Ordering::Relaxed);
                        if timer % 5 == 0 {
                            world.sync_world_event(
                                WorldEvent::ParticlesTrialSpawnerDetectPlayer,
                                self.position,
                                0,
                            );
                        }
                    } else {
                        let player_count = self.player_count_nearby(world).max(1) as i32;
                        self.required_mobs.store(
                            player_count * Self::REQUIRED_MOBS_PER_PLAYER,
                            Ordering::Relaxed,
                        );
                        self.state.store(STATE_ACTIVE, Ordering::Relaxed);
                        self.spawn_delay
                            .store(Self::DEFAULT_SPAWN_DELAY, Ordering::Relaxed);
                        self.rewards_ejected.store(false, Ordering::Relaxed);
                        world.sync_world_event(
                            WorldEvent::ParticlesTrialSpawnerSpawn,
                            self.position,
                            0,
                        );
                    }
                }

                STATE_ACTIVE => {
                    if !self.has_player_nearby(world) {
                        self.reset_for_new_cycle().await;
                        return;
                    }

                    let delay = self.spawn_delay.load(Ordering::Relaxed);
                    if delay > 0 {
                        self.spawn_delay.store(delay - 1, Ordering::Relaxed);
                    } else {
                        let spawned = self.spawn_mob_wave(world).await;
                        if spawned > 0 {
                            self.spawn_delay.store(
                                Self::DEFAULT_SPAWN_DELAY + rand::random_range(0..40),
                                Ordering::Relaxed,
                            );
                        }
                    }
                }

                STATE_REWARD => {
                    let timer = self.state_timer.load(Ordering::Relaxed);
                    if timer > 0 {
                        self.state_timer.store(timer - 1, Ordering::Relaxed);
                        if timer % 5 == 0 {
                            world.sync_world_event(
                                WorldEvent::AnimationTrialSpawnerEjectItem,
                                self.position,
                                0,
                            );
                        }
                    } else {
                        if !self.rewards_ejected.load(Ordering::Relaxed) {
                            self.eject_rewards(world).await;
                        }
                        self.state.store(STATE_COOLDOWN, Ordering::Relaxed);
                        self.state_timer
                            .store(self.cooldown_length, Ordering::Relaxed);
                    }
                }

                STATE_COOLDOWN => {
                    let timer = self.state_timer.load(Ordering::Relaxed);
                    if timer > 0 {
                        self.state_timer.store(timer - 1, Ordering::Relaxed);
                    } else {
                        self.cooldown_players.lock().await.clear();
                        self.reset_for_new_cycle().await;
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
            nbt.put_int("spawn_range", self.spawn_range);
            nbt.put_int("spawn_count", self.spawn_count);
            nbt.put_int("cooldown_length", self.cooldown_length);
        })
    }

    fn from_nbt(nbt: &NbtCompound, position: BlockPos) -> Self
    where
        Self: Sized,
    {
        let mut spawn_data = Vec::new();
        if let Some(potentials) = nbt.get_list("spawn_potentials") {
            for tag in potentials {
                if let NbtTag::Compound(spawn_entry) = tag {
                    if let Some(et) = spawn_entry
                        .get_compound("entity")
                        .and_then(|e| e.get_string("id"))
                        .and_then(|id| {
                            let name = id.strip_prefix("minecraft:").unwrap_or(id);
                            EntityType::from_name(name)
                        })
                    {
                        spawn_data.push(SpawnDataEntry {
                            entity_type: et,
                            nbt: spawn_entry.get_compound("entity").cloned(),
                            weight: spawn_entry.get_int("weight").unwrap_or(1),
                        });
                    }
                }
            }
        }
        if spawn_data.is_empty() {
            spawn_data = DEFAULT_SPAWN_DATA
                .iter()
                .map(|&(et, w)| SpawnDataEntry {
                    entity_type: et,
                    nbt: None,
                    weight: w,
                })
                .collect();
        }

        let ominous = nbt.get_bool("ominous").unwrap_or(false);

        let mut ominous_spawn_data = Vec::new();
        if let Some(potentials) = nbt.get_list("ominous_spawn_potentials") {
            for tag in potentials {
                if let NbtTag::Compound(spawn_entry) = tag {
                    if let Some(et) = spawn_entry
                        .get_compound("entity")
                        .and_then(|e| e.get_string("id"))
                        .and_then(|id| {
                            let name = id.strip_prefix("minecraft:").unwrap_or(id);
                            EntityType::from_name(name)
                        })
                    {
                        ominous_spawn_data.push(SpawnDataEntry {
                            entity_type: et,
                            nbt: spawn_entry.get_compound("entity").cloned(),
                            weight: spawn_entry.get_int("weight").unwrap_or(1),
                        });
                    }
                }
            }
        }

        Self {
            position,
            state: AtomicI32::new(STATE_INACTIVE),
            ominous: AtomicBool::new(ominous),
            spawn_delay: AtomicI32::new(Self::DEFAULT_SPAWN_DELAY),
            spawn_count: nbt
                .get_int("spawn_count")
                .unwrap_or(Self::DEFAULT_SPAWN_COUNT),
            spawn_range: nbt
                .get_int("spawn_range")
                .unwrap_or(Self::DEFAULT_SPAWN_RANGE),
            spawn_data,
            required_mobs: AtomicI32::new(0),
            tracked_mobs: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            cooldown_players: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            state_timer: AtomicI32::new(0),
            cooldown_length: nbt
                .get_int("cooldown_length")
                .unwrap_or(Self::COOLDOWN_LENGTH),
            rewards_ejected: AtomicBool::new(false),
            ominous_spawn_data,
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
