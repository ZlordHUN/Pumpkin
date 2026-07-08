//! Pillager Raid system. Manages raid lifecycle: triggering, wave
//! spawning, bossbar display, victory/loss conditions, and Hero of the
//! Village rewards.
//!
//! Matches vanilla `Raid` behaviour as closely as Pumpkin's current API
//! allows.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use pumpkin_data::Block;
use pumpkin_data::effect::StatusEffect;
use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::potion::Effect;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::world::WorldEvent;
use pumpkin_util::math::boundingbox::BoundingBox;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use pumpkin_util::text::TextComponent;
use rand::RngExt;
use std::sync::atomic::Ordering::Relaxed;
use tracing::{debug, warn};
use uuid::Uuid;

use super::World;
use super::bossbar::{Bossbar, BossbarColor};
use crate::block::blocks::redstone::bell::ring_bell;
use crate::entity::EntityBase;
use crate::entity::r#type::from_type;

// ── Raid constants (match vanilla 1.21) ────────────────────────────────────

/// Maximum number of waves in a raid.
const MAX_WAVES: u8 = 7;

/// Ticks between waves (30 seconds).
const WAVE_DELAY_TICKS: u32 = 600;

/// Ticks before raid expires if no raiders found (20 minutes).
const RAID_TIMEOUT_TICKS: u32 = 24_000;

/// Ticks before victory celebration expires and Hero is awarded.
const CELEBRATION_TICKS: u32 = 600;

/// How often (in ticks) we check for players with Raid Omen near villages.
pub const RAID_OMEN_CHECK_INTERVAL: u32 = 20;

/// Radius for raid spawn attempts around the village center.
const SPAWN_RADIUS: i32 = 64;

/// Min distance raiders spawn from the center.
const SPAWN_MIN_DIST: i32 = 24;

/// Raider tracking — entity IDs belonging to each raid.
pub type RaidEntitySet = HashSet<i32>;

/// ── Raid status ───────────────────────────────────────────────────────────
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RaidStatus {
    /// Raid is ongoing.
    Ongoing,
    /// All waves defeated, celebration timer running.
    Victory,
    /// Raid was lost (all villagers killed or timeout).
    Loss,
}

/// ── Raid ──────────────────────────────────────────────────────────────────
pub struct Raid {
    /// Unique raid identifier.
    pub id: i32,
    /// Village center (block position).
    pub center: BlockPos,
    /// World this raid belongs to.
    pub world: Option<Arc<World>>,
    /// Total waves in this raid (based on bad omen level + difficulty).
    pub total_waves: u8,
    /// Current wave index (0-based).
    pub current_wave: u8,
    /// Ticks until the next wave spawns.
    pub wave_countdown: u32,
    /// Number of raiders still alive in the current wave.
    pub raiders_alive: u32,
    /// Total raider count this raid.
    pub total_raiders_spawned: u32,
    /// Raid status.
    pub status: RaidStatus,
    /// The bossbar displayed to players within range.
    pub bossbar: Bossbar,
    /// Timer tracking overall raid duration (times out eventually).
    pub ticks_active: u32,
    /// Celebration timer after victory.
    pub celebration_ticks: u32,
    /// Bad omen level that triggered this raid.
    pub bad_omen_level: u8,
    /// Set of entity IDs belonging to this raid (for death tracking).
    pub raider_entities: RaidEntitySet,
    /// Tracks whether the victory bonus has been awarded.
    pub victory_awarded: bool,
}

impl Raid {
    #[must_use]
    pub fn new(id: i32, center: BlockPos, bad_omen_level: u8, world: Option<Arc<World>>) -> Self {
        let total_waves = Self::get_wave_count(bad_omen_level);
        let title = TextComponent::text("Raid");
        let mut bossbar = Bossbar::new(title);
        bossbar.color = BossbarColor::Red;
        bossbar.health = 0.0;

        Self {
            id,
            center,
            world,
            total_waves,
            current_wave: 0,
            wave_countdown: WAVE_DELAY_TICKS,
            raiders_alive: 0,
            total_raiders_spawned: 0,
            status: RaidStatus::Ongoing,
            bossbar,
            ticks_active: 0,
            celebration_ticks: 0,
            bad_omen_level,
            raider_entities: RaidEntitySet::new(),
            victory_awarded: false,
        }
    }

    /// Determines the number of waves based on bad omen level.
    #[must_use]
    pub const fn get_wave_count(bad_omen_level: u8) -> u8 {
        match bad_omen_level {
            0..=1 => 3,
            2 => 5,
            3 => 6,
            _ => MAX_WAVES,
        }
    }

    /// Returns the bossbar health as a fraction (remaining raiders / total).
    #[must_use]
    pub fn bossbar_progress(&self) -> f32 {
        let total_waves = f32::from(self.total_waves);
        let current = f32::from(self.current_wave);
        if total_waves == 0.0 {
            return 1.0;
        }
        let wave_alive = if self.current_wave < self.total_waves {
            self.get_raiders_in_wave(self.current_wave)
        } else {
            0
        };
        let wave_fraction = if wave_alive > 0 {
            let living = self.raiders_alive as f32 / wave_alive as f32;
            1.0 - living
        } else {
            1.0
        };
        (current + wave_fraction) / total_waves
    }

    /// Returns the total number of raiders expected in a wave.
    #[must_use]
    pub const fn get_raiders_in_wave(&self, wave: u8) -> u32 {
        match wave {
            0..=2 => 4, // Waves 1-3: 4 raiders each
            3..=4 => 5, // Waves 4-5: 5 raiders each
            5 => 6,     // Wave 6: 6 raiders
            _ => 7,     // Wave 7+: 7 raiders
        }
    }

    /// Returns the wave composition (entity types to spawn).
    #[must_use]
    pub fn get_wave_members(&self) -> Vec<(&'static EntityType, u32)> {
        let bonus = self.bad_omen_level.saturating_sub(1) as u32;

        match self.current_wave {
            0 => vec![(&EntityType::PILLAGER, 4 + bonus)],
            1 => vec![
                (&EntityType::PILLAGER, 2 + bonus),
                (&EntityType::VINDICATOR, 2),
            ],
            2 => vec![
                (&EntityType::PILLAGER, 3 + bonus),
                (&EntityType::VINDICATOR, 1),
            ],
            3 => vec![
                (&EntityType::PILLAGER, 2),
                (&EntityType::VINDICATOR, 2),
                (&EntityType::WITCH, 1 + bonus),
            ],
            4 => vec![
                (&EntityType::PILLAGER, 3),
                (&EntityType::VINDICATOR, 1),
                (&EntityType::EVOKER, 1 + bonus.min(1)),
            ],
            5 => vec![
                (&EntityType::RAVAGER, 1),
                (&EntityType::PILLAGER, 2 + bonus),
                (&EntityType::VINDICATOR, 1),
                (&EntityType::WITCH, 1),
            ],
            _ => vec![
                (&EntityType::RAVAGER, 1 + bonus.min(1)),
                (&EntityType::EVOKER, 1),
                (&EntityType::VINDICATOR, 2),
                (&EntityType::PILLAGER, 2),
                (&EntityType::WITCH, 1),
            ],
        }
    }

    /// Returns `true` if the raid is finished (either victory or loss).
    #[must_use]
    pub const fn is_finished(&self) -> bool {
        matches!(self.status, RaidStatus::Victory | RaidStatus::Loss)
    }

    /// Returns `true` if the current wave has been defeated.
    #[must_use]
    pub const fn is_current_wave_defeated(&self) -> bool {
        self.raiders_alive == 0
    }

    /// Returns `true` if the given entity type is a raider mob.
    #[must_use]
    pub const fn is_raider(entity_type: &EntityType) -> bool {
        // Compare by ID to avoid structural pattern matching issues
        let id = entity_type.id;
        id == EntityType::PILLAGER.id
            || id == EntityType::VINDICATOR.id
            || id == EntityType::EVOKER.id
            || id == EntityType::RAVAGER.id
            || id == EntityType::WITCH.id
            || id == EntityType::VEX.id
            || id == EntityType::ILLUSIONER.id
    }
}

/// ── Raid Manager ──────────────────────────────────────────────────────────
pub struct RaidManager {
    /// All active raids, keyed by their unique ID.
    pub raids: HashMap<i32, Raid>,
    /// Auto-incrementing ID counter.
    next_id: i32,
    /// Tick counter for periodic tasks.
    tick_counter: u32,
}

impl RaidManager {
    #[must_use]
    pub fn new() -> Self {
        Self {
            raids: HashMap::new(),
            next_id: 0,
            tick_counter: 0,
        }
    }

    /// Attempts to start a raid at a village center for a player with
    /// Raid Omen effect. Returns the raid ID if successful.
    pub fn try_start_raid(
        &mut self,
        center: Vector3<f64>,
        bad_omen_level: u8,
        world: Option<Arc<World>>,
    ) -> Option<i32> {
        let center_block = BlockPos::new(center.x as i32, center.y as i32, center.z as i32);
        let center_exists = self
            .raids
            .values()
            .any(|r| r.center == center_block && !r.is_finished());
        if center_exists {
            return None;
        }

        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);

        let raid = Raid::new(id, center_block, bad_omen_level, world.clone());
        debug!("Raid #{id} started at {center_block:?} with bad omen level {bad_omen_level}");
        self.raids.insert(id, raid);

        // Ring all bells in the village to alert villagers
        if let Some(ref w) = world {
            ring_village_bells(w, &center_block);
        }

        Some(id)
    }

    /// Returns `true` if any unfinished raid exists at the given center.
    #[must_use]
    pub fn has_active_raid_at(&self, center: &BlockPos) -> bool {
        self.raids
            .values()
            .any(|r| r.center == *center && !r.is_finished())
    }

    /// Returns the raid at the given center if one exists and is active.
    #[must_use]
    pub fn get_raid_at(&self, center: &BlockPos) -> Option<&Raid> {
        self.raids
            .values()
            .find(|r| r.center == *center && !r.is_finished())
    }

    /// Called when a raider entity is killed. Finds and updates the
    /// appropriate raid.
    pub fn on_raider_killed(&mut self, entity_id: i32) {
        let mut affected_raid_id = None;
        for raid in self.raids.values_mut() {
            if raid.raider_entities.remove(&entity_id) {
                raid.raiders_alive = raid.raiders_alive.saturating_sub(1);
                affected_raid_id = Some(raid.id);
                break;
            }
        }

        if let Some(rid) = affected_raid_id {
            debug!(
                "Raider entity {entity_id} killed. Raid #{rid}: {} raiders remaining",
                self.raids[&rid].raiders_alive
            );
        }
    }

    /// Checks if an entity belongs to any active raid.
    #[must_use]
    pub fn is_raider(&self, entity_id: i32) -> bool {
        self.raids
            .values()
            .any(|r| r.raider_entities.contains(&entity_id))
    }

    /// Advances all active raids by one tick. Returns spawns, victories, and losses.
    pub fn tick(&mut self, world_seed: u64, world_age: i64) -> RaidTickResult {
        self.tick_counter = self.tick_counter.wrapping_add(1);

        let mut spawns: Vec<(Vector3<f64>, &'static EntityType, i32)> = Vec::new();
        let mut finished_ids: Vec<i32> = Vec::new();
        let mut victory_ids: Vec<i32> = Vec::new();
        let mut loss_ids: Vec<i32> = Vec::new();

        for raid in self.raids.values_mut() {
            if raid.status == RaidStatus::Victory {
                raid.celebration_ticks = raid.celebration_ticks.saturating_sub(1);
                raid.bossbar.health = 1.0;
                if raid.celebration_ticks == 0 {
                    if !raid.victory_awarded {
                        victory_ids.push(raid.id);
                        raid.victory_awarded = true;
                    }
                    finished_ids.push(raid.id);
                }
                continue;
            }

            if raid.status == RaidStatus::Loss {
                loss_ids.push(raid.id);
                finished_ids.push(raid.id);
                continue;
            }

            raid.ticks_active = raid.ticks_active.saturating_add(1);

            if raid.ticks_active >= RAID_TIMEOUT_TICKS {
                raid.status = RaidStatus::Loss;
                loss_ids.push(raid.id);
                finished_ids.push(raid.id);
                warn!(
                    "Raid #{} timed out after {} ticks",
                    raid.id, RAID_TIMEOUT_TICKS
                );
                continue;
            }

            raid.bossbar.health = raid.bossbar_progress();
            raid.bossbar.title =
                TextComponent::text(format!("Raid - Wave {}", raid.current_wave + 1));

            if raid.wave_countdown > 0 {
                raid.wave_countdown = raid.wave_countdown.saturating_sub(1);
                continue;
            }

            if raid.is_current_wave_defeated() {
                raid.current_wave = raid.current_wave.saturating_add(1);
                if raid.current_wave >= raid.total_waves {
                    raid.status = RaidStatus::Victory;
                    raid.celebration_ticks = CELEBRATION_TICKS;
                    raid.bossbar.health = 1.0;
                    raid.bossbar.title = TextComponent::text("Raid - Victory!");
                    debug!("Raid #{}: all waves defeated!", raid.id);
                    continue;
                }
                raid.wave_countdown = WAVE_DELAY_TICKS;
                continue;
            }

            if raid.raiders_alive == 0 {
                let members = raid.get_wave_members();
                for (entity_type, count) in members {
                    for _ in 0..count {
                        let pos = Self::find_spawn_position(
                            raid.center,
                            world_seed,
                            world_age + i64::from(raid.raiders_alive),
                        );
                        spawns.push((pos, entity_type, raid.id));
                        raid.raiders_alive = raid.raiders_alive.saturating_add(1);
                        raid.total_raiders_spawned = raid.total_raiders_spawned.saturating_add(1);
                    }
                }
            }
        }

        for id in &finished_ids {
            self.raids.remove(id);
        }

        RaidTickResult {
            spawns,
            victory_ids,
            loss_ids,
        }
    }

    /// Finds a valid spawn position around the center.
    fn find_spawn_position(center: BlockPos, world_seed: u64, world_age: i64) -> Vector3<f64> {
        let hash = world_seed
            .wrapping_mul(31)
            .wrapping_add(world_age as u64)
            .wrapping_add(center.0.x as u64)
            .wrapping_mul(6364136223846793005u64);

        let angle = (hash as f64 % 360.0).to_radians();
        let dist = SPAWN_MIN_DIST as f64
            + ((hash.wrapping_shr(8) as f64).abs() % ((SPAWN_RADIUS - SPAWN_MIN_DIST) as f64));

        let x = center.0.x as f64 + angle.cos() * dist;
        let z = center.0.z as f64 + angle.sin() * dist;

        Vector3::new(x, center.0.y as f64, z)
    }
}

impl Default for RaidManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of a single tick of the raid manager.
pub struct RaidTickResult {
    /// Entities to spawn this tick.
    pub spawns: Vec<(Vector3<f64>, &'static EntityType, i32)>,
    /// Raid IDs that achieved victory this tick.
    pub victory_ids: Vec<i32>,
    /// Raid IDs that were lost this tick.
    pub loss_ids: Vec<i32>,
}

/// ── World extension methods ──────────────────────────────────────────────
impl World {
    /// Gets or initializes the raid manager for this world.
    pub async fn get_raid_manager(&self) -> tokio::sync::MutexGuard<'_, RaidManager> {
        self.raid_manager.lock().await
    }

    /// Ticks the raid system. Called from `tick_environment`.
    pub async fn tick_raids(self: &Arc<Self>, world_seed: u64, world_age: i64) {
        let result = {
            let mut rm = self.raid_manager.lock().await;
            rm.tick(world_seed, world_age)
        };

        // Spawn raider entities
        for (pos, entity_type, raid_id) in &result.spawns {
            let entity = from_type(entity_type, *pos, self, Uuid::new_v4());
            let entity_id = entity.get_entity().entity_id;
            self.spawn_entity(entity).await;

            // Track this entity in the raid
            let mut rm = self.raid_manager.lock().await;
            if let Some(raid) = rm.raids.get_mut(raid_id) {
                raid.raider_entities.insert(entity_id);
            }
        }

        // Play raid horn sound on wave start
        {
            let rm = self.raid_manager.lock().await;
            for raid in rm.raids.values() {
                if raid.wave_countdown == WAVE_DELAY_TICKS.saturating_sub(1) {
                    let block_pos =
                        BlockPos::new(raid.center.0.x, raid.center.0.y, raid.center.0.z);
                    // Global raid horn world event + positional sound
                    self.sync_world_event(WorldEvent::SoundRaidHorn, block_pos, 0);
                    let pos = Vector3::new(
                        f64::from(raid.center.0.x),
                        f64::from(raid.center.0.y),
                        f64::from(raid.center.0.z),
                    );
                    self.play_sound_fine(
                        Sound::EventRaidHorn,
                        SoundCategory::Hostile,
                        &pos,
                        1.0,
                        1.0,
                    );
                }
            }
        }

        // Award Hero of the Village on victory
        for raid_id in &result.victory_ids {
            self.award_hero_of_the_village(*raid_id, world_age).await;
        }

        // Broadcast raid loss messages
        for raid_id in &result.loss_ids {
            debug!("Raid #{raid_id}: defeated (loss).");
        }
    }

    /// Checks if any player near a village has Raid Omen and starts a raid.
    pub async fn check_raid_triggers(self: &Arc<Self>) {
        let players = self.players.load();

        let mut triggers: Vec<(Vector3<f64>, u8)> = Vec::new();

        for player in players.iter() {
            // Check if player has RAID_OMEN effect
            let living = &player.living_entity;
            let has_raid_omen = living
                .active_effects
                .lock()
                .await
                .contains_key(&StatusEffect::RAID_OMEN);

            if !has_raid_omen {
                continue;
            }

            // Get the player's position
            let entity = player.get_entity();
            let player_pos = entity.pos.load();

            // Check if player is near a village
            let player_vec = Vector3::new(player_pos.x, player_pos.y, player_pos.z);
            let vm = self.village_manager.lock().await;
            if let Some(village) = vm.get_village_at(&player_vec) {
                let center = village.center;
                let rm = self.raid_manager.lock().await;

                let center_block = BlockPos::new(center.x as i32, center.y as i32, center.z as i32);
                if !rm.has_active_raid_at(&center_block) {
                    // Get bad omen level from the effect
                    let bad_omen_level = living
                        .active_effects
                        .lock()
                        .await
                        .get(&StatusEffect::RAID_OMEN)
                        .map_or(1, |e| (e.amplifier + 1).min(5));

                    triggers.push((center, bad_omen_level));

                    // Remove the RAID_OMEN effect
                    drop(living.active_effects.lock().await);
                    player.remove_effect(&StatusEffect::RAID_OMEN).await;
                }
            }
        }

        // Start raids outside of any lock
        for (center, bad_omen_level) in triggers {
            let mut rm = self.raid_manager.lock().await;
            rm.try_start_raid(center, bad_omen_level, Some(self.clone()));
        }
    }

    /// Awards the Hero of the Village effect to nearby players.
    async fn award_hero_of_the_village(&self, raid_id: i32, _world_age: i64) {
        let (center, bad_omen_level) = {
            let rm = self.raid_manager.lock().await;
            let Some(raid) = rm.raids.get(&raid_id) else {
                return;
            };
            (raid.center, raid.bad_omen_level)
        };

        debug!("Raid #{raid_id}: awarding Hero of the Village (level {bad_omen_level})");

        let block_center = BlockPos::new(center.0.x, center.0.y, center.0.z);

        let effect = Effect {
            effect_type: &StatusEffect::HERO_OF_THE_VILLAGE,
            // Duration: 60 minutes (ticks) / 40 minutes for level 1
            duration: 48000 * i32::from(bad_omen_level),
            // Amplifier: 0-indexed, so level 1 = amp 0
            amplifier: bad_omen_level.saturating_sub(1),
            ambient: true,
            show_particles: true,
            show_icon: true,
            blend: false,
        };

        let players = self.players.load();
        for player in players.iter() {
            let entity = player.get_entity();
            let pos = entity.pos.load();
            let player_pos = Vector3::new(pos.x, pos.y, pos.z);
            let center_pos = Vector3::new(
                f64::from(block_center.0.x),
                f64::from(block_center.0.y),
                f64::from(block_center.0.z),
            );

            // Award to players within a generous range of the village center
            if player_pos.squared_distance_to_vec(&center_pos) < (SPAWN_RADIUS as f64).powi(2) {
                player.add_effect(effect.clone()).await;
            }
        }
    }

    /// Called when any entity dies. Checks if it was a raider and notifies
    /// the raid manager. Also handles ominous banner drops for raid captains.
    pub async fn on_entity_death(self: &Arc<Self>, entity_id: i32) {
        let mut rm = self.raid_manager.lock().await;
        if rm.is_raider(entity_id) {
            rm.on_raider_killed(entity_id);

            // Drop an ominous banner if this was a raid captain (pillager with banner)
            if let Some(entity) = self.get_entity_by_id(entity_id) {
                let e = entity.get_entity();
                if e.entity_type == &EntityType::PILLAGER {
                    let pos = e.pos.load();
                    let block_pos = BlockPos::new(
                        pos.x.floor() as i32,
                        pos.y.floor() as i32,
                        pos.z.floor() as i32,
                    );
                    let banner = ItemStack::new(1, &Item::WHITE_BANNER);
                    self.drop_stack(&block_pos, banner).await;
                }
            }
        }
    }

    /// Attempts to spawn a wandering trader near a random player.
    pub async fn tick_wandering_trader_spawning(self: &Arc<Self>, _world_age: i64) {
        let delay = self.wandering_trader_spawn_delay.load(Relaxed);
        if delay > 0 {
            self.wandering_trader_spawn_delay.store(delay - 1, Relaxed);
            return;
        }

        // Reset delay
        self.wandering_trader_spawn_delay.store(24000, Relaxed);

        // Must have at least one player
        let players = self.players.load();
        if players.is_empty() {
            return;
        }

        // Pick a random player
        let (_player_pos, spawn_pos) = {
            let mut rng = rand::rng();

            let idx = rng.random_range(0..players.len());
            let player = &players[idx];
            let player_pos = player.get_entity().pos.load();

            // Spawn chance: 25% base, increases each failed attempt, caps at 75%
            let chance = self.wandering_trader_spawn_chance.load(Relaxed).min(75);
            if rng.random_range(0..100) >= chance {
                self.wandering_trader_spawn_chance
                    .store(chance + 1, Relaxed);
                return;
            }

            // Reset chance on success
            self.wandering_trader_spawn_chance.store(25, Relaxed);

            // Find a spawn position within 48 blocks of the player
            let mut spawn_pos = None;
            for _ in 0..10 {
                let dx = rng.random_range(-48i32..=48);
                let dz = rng.random_range(-48i32..=48);
                let x = player_pos.x as i32 + dx;
                let z = player_pos.z as i32 + dz;

                let top_y = self.get_top_y();
                let bottom_y = self.get_bottom_y();
                let mut surface_y = None;
                for y in (bottom_y..=top_y).rev() {
                    let pos = BlockPos::new(x, y, z);
                    let block = self.get_block(&pos);
                    if block.id != Block::AIR.id {
                        surface_y = Some(y);
                        break;
                    }
                }

                if let Some(y) = surface_y {
                    let vec_pos = Vector3::new(x as f64, (y + 1) as f64, z as f64);
                    let bb = BoundingBox::new(
                        Vector3::new(vec_pos.x - 0.3, vec_pos.y, vec_pos.z - 0.3),
                        Vector3::new(vec_pos.x + 0.3, vec_pos.y + 1.8, vec_pos.z + 0.3),
                    );
                    if self.is_space_empty(bb) {
                        spawn_pos = Some(vec_pos);
                        break;
                    }
                }
            }
            (player_pos, spawn_pos)
        };

        if let Some(pos) = spawn_pos {
            let entity = from_type(&EntityType::WANDERING_TRADER, pos, self, Uuid::new_v4());
            self.spawn_entity(entity).await;

            // Spawn 2 trader llamas nearby
            for offset in [-2.0f64, 2.0f64] {
                let llama_pos = Vector3::new(pos.x + offset, pos.y, pos.z);
                let llama = from_type(&EntityType::TRADER_LLAMA, llama_pos, self, Uuid::new_v4());
                self.spawn_entity(llama).await;
            }

            debug!("Spawned wandering trader + 2 llamas near player at {pos:?}");
        }
    }
}

/// Rings all bells near a village center to alert villagers of a raid.
fn ring_village_bells(world: &Arc<World>, center: &BlockPos) {
    let search_radius = 48;
    let y_range = 8;

    for dy in -y_range..=y_range {
        for dx in (-search_radius..=search_radius).step_by(4) {
            for dz in (-search_radius..=search_radius).step_by(4) {
                let pos = BlockPos::new(center.0.x + dx, center.0.y + dy, center.0.z + dz);
                let block = world.get_block(&pos);
                if block.id == Block::BELL.id {
                    ring_bell(pos, world, None);
                    debug!("Ringing bell at {pos:?} for raid start");
                }
            }
        }
    }
}
