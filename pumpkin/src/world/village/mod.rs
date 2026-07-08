//! Village system — tracks groups of villagers, beds, bells, and job sites as
//! "villages" and drives mechanics like iron golem spawning, cat spawning,
//! zombie sieges, and raid eligibility.
//!
//! Vanilla parity notes:
//! - Villages are defined by at least 1 claimed bed + 1 villager (vanilla uses
//!   a Point-of-Interest manager; we aggregate from villager entity data).
//! - Iron golem spawning: timer-based 600-tick cooldown, 16×6×16 search area,
//!   valid block checks (solid floor, air body rows, no liquid).
//! - Cat spawning: 1 cat per 4 beds, max 10.
//! - Zombie sieges: midnight only, ≥10 beds, ≥20 villagers, 10% chance.
//!   Spawns 10-20 zombies outside but near the village over several ticks.
//!   Cooldown of 10 in-game days after a completed siege.

use pumpkin_data::Block;
use pumpkin_data::BlockStateId;
use pumpkin_data::block_properties::blocks_movement;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use pumpkin_util::random::RandomImpl;
use pumpkin_util::random::xoroshiro128::Xoroshiro;
use std::collections::HashSet;
use tracing::debug;

// ── Constants ──────────────────────────────────────────────────────────

/// Golem spawn cooldown: 600 ticks = 30 seconds.
const GOLEM_SPAWN_DELAY: i32 = 600;
/// Horizontal search range for golem spawn position.
const GOLEM_SEARCH_H_RANGE: i32 = 16;
/// Vertical search range for golem spawn position.
const GOLEM_SEARCH_V_RANGE: i32 = 6;

/// Siege cooldown: 10 in-game days after a siege ends before another can start.
const SIEGE_COOLDOWN_TICKS: i64 = 24000 * 10;
/// Min zombies spawned per siege wave.
/// Max zombies a single siege can spawn.
const SIEGE_MAX_ZOMBIES: i32 = 20;
/// Zombies per tick during an active siege.
const SIEGE_SPAWN_RATE: i32 = 3;
/// Horizontal search radius for siege zombie positions (outside village).
const SIEGE_SEARCH_RADIUS: i32 = 32;
/// Minimum distance from center for siege spawns (to avoid spawning inside).
const SIEGE_INNER_RADIUS: i32 = 10;
/// Vanilla chance (1 in 10) per midnight that a siege triggers.
const SIEGE_CHANCE_DENOMINATOR: i32 = 10;

// ── Village ────────────────────────────────────────────────────────────

/// Tracks a single village — a coherent group of villagers, beds,
/// job-site blocks, bells, and optionally iron golems.
#[derive(Debug, Clone)]
pub struct Village {
    pub center: Vector3<f64>,
    pub radius: i32,
    pub beds: HashSet<BlockPos>,
    pub bells: HashSet<BlockPos>,
    pub population: i32,
    pub golem_count: i32,
    pub cat_count: i32,
    pub golem_spawn_timer: i32,
    /// World-age tick when the next siege may start (cooldown after previous).
    pub siege_cooldown_end: i64,
    /// Whether a zombie siege is currently active for this village.
    pub siege_active: bool,
    /// How many zombies have been spawned so far in the current siege.
    pub siege_spawned_count: i32,
}

impl Village {
    #[must_use]
    pub fn max_golems(&self) -> i32 {
        (self.population / 10).max(0)
    }

    #[must_use]
    pub fn can_spawn_golem(&self) -> bool {
        self.population >= 10 && self.golem_count < self.max_golems()
    }

    #[must_use]
    pub fn max_cats(&self) -> i32 {
        if self.population < 1 {
            return 0;
        }
        (self.beds.len() as i32 / 4).min(10)
    }

    /// Vanilla: at least 10 beds + 20 villagers, not already in a siege,
    /// and cooldown has expired.
    #[must_use]
    pub fn can_start_siege(&self, world_age: i64) -> bool {
        self.beds.len() >= 10
            && self.population >= 20
            && !self.siege_active
            && world_age >= self.siege_cooldown_end
    }
}

// ── Villager snapshot ──────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct VillagerSnapshot {
    pub position: Vector3<f64>,
    pub home_pos: Option<BlockPos>,
    pub is_adult: bool,
}

// ── Village Manager ────────────────────────────────────────────────────

pub struct VillageManager {
    pub villages: Vec<Village>,
    tick_counter: u32,
    interval: u32,
}

impl VillageManager {
    #[must_use]
    pub fn new(interval: u32) -> Self {
        Self {
            villages: Vec::new(),
            tick_counter: 0,
            interval,
        }
    }

    pub fn should_rebuild(&mut self) -> bool {
        self.tick_counter = self.tick_counter.wrapping_add(1);
        if self.tick_counter % self.interval == 0 {
            self.tick_counter = 0;
            true
        } else {
            false
        }
    }

    // ── Rebuild ───────────────────────────────────────────────────────

    pub fn rebuild(&mut self, villagers: &[VillagerSnapshot], bell_positions: &[BlockPos]) {
        let old_state: Vec<Option<VillageState>> = self
            .villages
            .iter()
            .map(|v| {
                Some(VillageState {
                    golem_count: v.golem_count,
                    cat_count: v.cat_count,
                    golem_spawn_timer: v.golem_spawn_timer,
                    siege_active: v.siege_active,
                    siege_spawned_count: v.siege_spawned_count,
                    siege_cooldown_end: v.siege_cooldown_end,
                })
            })
            .collect();

        self.villages.clear();
        if villagers.is_empty() {
            return;
        }

        // Step 1: cluster beds
        let mut clusters: Vec<Cluster> = Vec::new();
        for snap in villagers {
            if snap.is_adult && let Some(home) = snap.home_pos {
                let bed_center = home.to_f64();
                let mut assigned = false;
                for cluster in &mut clusters {
                    if cluster.bed_centroid.squared_distance_to_vec(&bed_center) < 1024.0 {
                        cluster.add_bed(home);
                        assigned = true;
                        break;
                    }
                }
                if !assigned {
                    let mut c = Cluster::default();
                    c.add_bed(home);
                    clusters.push(c);
                }
            }
        }

        // Step 2: merge nearby clusters
        let mut merged = true;
        while merged {
            merged = false;
            let mut i = 0;
            while i < clusters.len() {
                let mut j = i + 1;
                while j < clusters.len() {
                    if clusters[i].bed_centroid.squared_distance_to_vec(&clusters[j].bed_centroid) < 1024.0 {
                        let other = clusters.remove(j);
                        clusters[i].merge(&other);
                        merged = true;
                    } else {
                        j += 1;
                    }
                }
                i += 1;
            }
        }

        // Step 3: count villagers
        for snap in villagers {
            let pos = &snap.position;
            let mut best_i: Option<usize> = None;
            let mut best_dist = f64::MAX;
            for (ci, cluster) in clusters.iter().enumerate() {
                let d = cluster.bed_centroid.squared_distance_to_vec(pos);
                if d < best_dist { best_dist = d; best_i = Some(ci); }
            }
            if let Some(ci) = best_i { clusters[ci].population += 1; }
        }

        // Step 4: assign bells
        for &bell in bell_positions {
            let bell_f = bell.to_f64();
            let mut best_i = 0;
            let mut best_dist = f64::MAX;
            for (ci, cluster) in clusters.iter().enumerate() {
                let d = cluster.bed_centroid.squared_distance_to_vec(&bell_f);
                if d < best_dist { best_dist = d; best_i = ci; }
            }
            if !clusters.is_empty() { clusters[best_i].bells.insert(bell); }
        }

        // Step 5: build villages
        for (_i, cluster) in clusters.iter().enumerate() {
            if cluster.beds.is_empty() || cluster.population == 0 { continue; }

            let center = if cluster.bells.is_empty() {
                cluster.bed_centroid
            } else {
                let n = cluster.bells.len() as f64;
                let sum = cluster.bells.iter().map(|b| b.to_f64()).fold(Vector3::new(0.0,0.0,0.0), |a,b| a+b);
                div_vec3_f64(sum, n)
            };

            let max_bed_dist = cluster.beds.iter().map(|b| {
                let bf = b.to_f64();
                (center.x-bf.x).powi(2)+(center.y-bf.y).powi(2)+(center.z-bf.z).powi(2)
            }).fold(0.0_f64, f64::max).sqrt();
            let radius = (32_i32).max((max_bed_dist + 32.0).ceil() as i32);

            let os = old_state.iter().filter_map(|opt| *opt).next().unwrap_or(VillageState {
                golem_count: 0, cat_count: 0, golem_spawn_timer: GOLEM_SPAWN_DELAY,
                siege_active: false, siege_spawned_count: 0, siege_cooldown_end: 0,
            });

            self.villages.push(Village {
                center, radius,
                beds: cluster.beds.clone(),
                bells: cluster.bells.clone(),
                population: cluster.population,
                golem_count: os.golem_count,
                cat_count: os.cat_count,
                golem_spawn_timer: os.golem_spawn_timer.max(1),
                siege_cooldown_end: os.siege_cooldown_end,
                siege_active: os.siege_active,
                siege_spawned_count: os.siege_spawned_count,
            });
        }

        debug!("VillageManager rebuilt: {} villages ({} beds, {} bells)",
            self.villages.len(),
            self.villages.iter().map(|v| v.beds.len()).sum::<usize>(),
            bell_positions.len(),
        );
    }

    #[must_use]
    pub fn get_village_at(&self, pos: &Vector3<f64>) -> Option<&Village> {
        self.villages.iter().find(|v| v.center.squared_distance_to_vec(pos) <= (v.radius*v.radius) as f64)
    }

    // ── Golem spawning ─────────────────────────────────────────────────

    #[must_use]
    pub fn tick_golem_spawn_timers(&mut self) -> Vec<(usize, Vector3<f64>)> {
        let mut expired = Vec::new();
        for (vi, v) in self.villages.iter_mut().enumerate() {
            if !v.can_spawn_golem() { continue; }
            v.golem_spawn_timer -= 1;
            if v.golem_spawn_timer <= 0 {
                expired.push((vi, v.center));
                v.golem_spawn_timer = GOLEM_SPAWN_DELAY;
            }
        }
        expired
    }

    #[must_use]
    pub fn find_golem_spawn_position(
        center: Vector3<f64>,
        world_seed: u64, world_age: i64,
        get_state: &impl Fn(BlockPos) -> Option<BlockStateId>,
    ) -> Option<Vector3<f64>> {
        let cx = center.x.floor() as i32;
        let cy = center.y.floor() as i32;
        let cz = center.z.floor() as i32;
        let base_seed = world_seed ^ (world_age as u64);

        for attempt in 0..3 {
            let mut rng = Xoroshiro::from_seed(base_seed.wrapping_add((attempt*17) as u64));
            let dx = rng.next_bounded_i32(GOLEM_SEARCH_H_RANGE*2+1) - GOLEM_SEARCH_H_RANGE;
            let dz = rng.next_bounded_i32(GOLEM_SEARCH_H_RANGE*2+1) - GOLEM_SEARCH_H_RANGE;
            let dy = rng.next_bounded_i32(GOLEM_SEARCH_V_RANGE*2+1) - GOLEM_SEARCH_V_RANGE;
            let bx = cx+dx; let by = cy+dy; let bz = cz+dz;

            let Some(fid) = get_state(BlockPos::new(bx,by-1,bz)) else { continue };
            let Some(b0) = get_state(BlockPos::new(bx,by,bz)) else { continue };
            let Some(b1) = get_state(BlockPos::new(bx,by+1,bz)) else { continue };

            let fs = pumpkin_data::BlockState::from_id(fid);
            let b0s = pumpkin_data::BlockState::from_id(b0);
            let b1s = pumpkin_data::BlockState::from_id(b1);
            let fb = pumpkin_data::Block::from_state_id(fid);

            if !blocks_movement(fs, fb.id) { continue; }
            if !b0s.is_air() || b0s.is_liquid() { continue; }
            if !b1s.is_air() || b1s.is_liquid() { continue; }

            let hid = get_state(BlockPos::new(bx,by+2,bz));
            if let Some(h) = hid {
                let hs = pumpkin_data::BlockState::from_id(h);
                if !hs.is_air() || hs.is_liquid() { continue; }
            }

            return Some(Vector3::new(bx as f64+0.5, by as f64, bz as f64+0.5));
        }
        None
    }

    pub fn on_golem_spawned(&mut self, idx: usize) {
        if let Some(v) = self.villages.get_mut(idx) { v.golem_count += 1; }
    }

    // ── Zombie sieges ──────────────────────────────────────────────────

    /// Checks all villages for siege eligibility at midnight.
    /// Returns a list of `(village_index, spawn_positions)` for villages
    /// where a siege should begin this tick.
    ///
    /// Vanilla: at midnight (time 18000–18019), each eligible village has
    /// a 1-in-10 chance of triggering a siege. Once triggered, zombies spawn
    /// in a ring around the village center (between `SIEGE_INNER_RADIUS` and
    /// `SIEGE_SEARCH_RADIUS` blocks).
    #[must_use]
    pub fn tick_sieges(
        &mut self,
        is_night: bool,
        is_midnight: bool,
        world_age: i64,
        world_seed: u64,
        get_state: &impl Fn(BlockPos) -> Option<BlockStateId>,
    ) -> Vec<(usize, Vec<Vector3<f64>>)> {
        let mut results: Vec<(usize, Vec<Vector3<f64>>)> = Vec::new();

        for (vi, village) in self.villages.iter_mut().enumerate() {
            // ── Try to start a siege ───────────────────────────────
            if !village.siege_active
                && is_midnight
                && village.can_start_siege(world_age)
            {
                let mut rng =
                    Xoroshiro::from_seed(world_seed ^ (world_age as u64).wrapping_add(vi as u64));
                if rng.next_bounded_i32(SIEGE_CHANCE_DENOMINATOR) == 0 {
                    // Start siege!
                    village.siege_active = true;
                    village.siege_spawned_count = 0;
                    debug!(
                        "Zombie siege started for village {} (center {:?})",
                        vi, village.center
                    );
                }
            }

            // ── Continue active siege ──────────────────────────────
            if village.siege_active {
                if !is_night || village.siege_spawned_count >= SIEGE_MAX_ZOMBIES {
                    // End siege: either dawn broke or cap reached.
                    village.siege_active = false;
                    village.siege_cooldown_end = world_age + SIEGE_COOLDOWN_TICKS;
                    debug!("Zombie siege ended for village {}", vi);
                    continue;
                }

                // Spawn a batch of zombies this tick.
                let remaining = SIEGE_MAX_ZOMBIES - village.siege_spawned_count;
                let to_spawn = SIEGE_SPAWN_RATE.min(remaining);
                let mut positions = Vec::with_capacity(to_spawn as usize);

                let mut rng = Xoroshiro::from_seed(
                    world_seed
                        ^ (world_age as u64)
                            .wrapping_add(vi as u64)
                            .wrapping_add(village.siege_spawned_count as u64),
                );

                for _ in 0..to_spawn {
                    if let Some(pos) =
                        Self::find_siege_spawn_position(village.center, &mut rng, get_state)
                    {
                        positions.push(pos);
                        village.siege_spawned_count += 1;
                    }
                }

                if !positions.is_empty() {
                    results.push((vi, positions));
                }
            }
        }

        results
    }

    /// Finds a valid zombie spawn position for a siege attempt.
    /// Vanilla: searches in a ring between inner and outer radii around
    /// the village center. The zombie needs a solid block below and 2 blocks
    /// of air above. Light level is ignored during sieges.
    #[must_use]
    fn find_siege_spawn_position(
        center: Vector3<f64>,
        rng: &mut Xoroshiro,
        get_state: &impl Fn(BlockPos) -> Option<BlockStateId>,
    ) -> Option<Vector3<f64>> {
        let cx = center.x.floor() as i32;
        let cy = center.y.floor() as i32;
        let cz = center.z.floor() as i32;

        // Up to 10 attempts to find a valid position.
        for _ in 0..10 {
            // Pick a random point in the ring.
            let dx = rng.next_bounded_i32(SIEGE_SEARCH_RADIUS * 2) - SIEGE_SEARCH_RADIUS;
            let dz = rng.next_bounded_i32(SIEGE_SEARCH_RADIUS * 2) - SIEGE_SEARCH_RADIUS;

            // Reject if too close to center (inside inner radius).
            if dx * dx + dz * dz < SIEGE_INNER_RADIUS * SIEGE_INNER_RADIUS {
                continue;
            }

            let dy = rng.next_bounded_i32(6) - 3; // -3..=3 vertical offset
            let bx = cx + dx;
            let by = cy + dy;
            let bz = cz + dz;

            let Some(fid) = get_state(BlockPos::new(bx, by - 1, bz)) else {
                continue;
            };
            let Some(b0) = get_state(BlockPos::new(bx, by, bz)) else {
                continue;
            };
            let Some(b1) = get_state(BlockPos::new(bx, by + 1, bz)) else {
                continue;
            };

            let fs = pumpkin_data::BlockState::from_id(fid);
            let b0s = pumpkin_data::BlockState::from_id(b0);
            let b1s = pumpkin_data::BlockState::from_id(b1);

            // Floor must be solid.
            if !fs.is_solid() { continue; }
            // Body rows must be air (zombies are 2 blocks tall).
            if !b0s.is_air() || b0s.is_liquid() { continue; }
            if !b1s.is_air() || b1s.is_liquid() { continue; }

            return Some(Vector3::new(bx as f64 + 0.5, by as f64, bz as f64 + 0.5));
        }

        None
    }

    // ── Cat spawning ────────────────────────────────────────────────────

    #[must_use]
    pub fn try_spawn_cats(
        &mut self, world_seed: u64, world_age: i64,
    ) -> Vec<(usize, Vector3<f64>)> {
        let mut spawn_requests = Vec::new();
        let base_seed = world_seed.wrapping_add(1).wrapping_mul(6364136223846793005) ^ (world_age as u64);

        for (vi, village) in self.villages.iter_mut().enumerate() {
            if village.cat_count >= village.max_cats() { continue; }
            let mut rng = Xoroshiro::from_seed(base_seed.wrapping_add(vi as u64));
            if rng.next_bounded_i32(200) != 0 { continue; }
            let ox = (rng.next_bounded_i32(village.radius) - village.radius/2) as f64;
            let oz = (rng.next_bounded_i32(village.radius) - village.radius/2) as f64;
            spawn_requests.push((vi, Vector3::new(village.center.x+ox, village.center.y, village.center.z+oz)));
            village.cat_count += 1;
        }
        spawn_requests
    }
}

impl Default for VillageManager {
    fn default() -> Self { Self::new(20) }
}

// ── Helpers ────────────────────────────────────────────────────────────

fn div_vec3_f64(v: Vector3<f64>, d: f64) -> Vector3<f64> {
    Vector3::new(v.x / d, v.y / d, v.z / d)
}

#[derive(Clone, Copy)]
struct VillageState {
    golem_count: i32,
    cat_count: i32,
    golem_spawn_timer: i32,
    siege_active: bool,
    siege_spawned_count: i32,
    siege_cooldown_end: i64,
}

#[derive(Default, Clone)]
struct Cluster {
    beds: HashSet<BlockPos>,
    bells: HashSet<BlockPos>,
    bed_centroid: Vector3<f64>,
    population: i32,
}

impl Cluster {
    fn add_bed(&mut self, pos: BlockPos) {
        let n = self.beds.len() as f64;
        let bf = pos.to_f64();
        self.bed_centroid = div_vec3_f64(self.bed_centroid * n + bf, n + 1.0);
        self.beds.insert(pos);
    }
    fn merge(&mut self, other: &Self) {
        let total = (self.beds.len() + other.beds.len()) as f64;
        self.bed_centroid = self.bed_centroid * (self.beds.len() as f64 / total)
            + other.bed_centroid * (other.beds.len() as f64 / total);
        self.beds.extend(&other.beds);
        self.bells.extend(&other.bells);
        self.population += other.population;
    }
}

#[must_use]
pub fn is_bell(block: &Block) -> bool { block.id == Block::BELL.id }
