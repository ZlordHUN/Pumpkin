//! Crossbow attack goal for pillagers. The pillager charges and fires
//! arrows at its target, with a reload delay between shots.

use pumpkin_util::math::vector3::Vector3;
use std::sync::{Arc, Weak};

use crate::entity::{
    Entity,
    ai::goal::{Controls, Goal, GoalFuture},
    mob::Mob,
    mob::pillager::PillagerEntity,
};

/// Pillager crossbow ranged attack goal.
/// Shoots arrows at the target with charge-up and reload delays.
pub struct PillagerCrossbowAttackGoal {
    pillager: Weak<PillagerEntity>,
    /// Timer until next action (ticks)
    attack_time: i32,
    /// Countdown for charge animation
    charge_time: i32,
    /// Whether the crossbow is currently charged/loaded
    charged: bool,
    /// Frames since target was last seen
    last_seen: i32,
}

impl PillagerCrossbowAttackGoal {
    #[must_use]
    pub const fn new(pillager: Weak<PillagerEntity>) -> Self {
        Self {
            pillager,
            attack_time: 0,
            charge_time: 0,
            charged: false,
            last_seen: 0,
        }
    }

    /// The range within which the pillager will use ranged attacks.
    /// In vanilla, follow range is typically 32 blocks.
    const fn get_follow_range_sq() -> f64 {
        1024.0 // 32^2
    }

    /// Minimum distance to switch to melee (vanilla: ~4 blocks).
    const fn get_melee_range_sq() -> f64 {
        16.0
    }

    /// Time to charge the crossbow before firing (ticks).
    const CHARGE_DURATION: i32 = 25; // ~1.25 seconds at 20 TPS
    /// Cooldown between shots.
    const RELOAD_COOLDOWN: i32 = 20; // 1 second reload
}

impl Goal for PillagerCrossbowAttackGoal {
    fn can_start<'a>(&'a mut self, _mob: &'a dyn Mob) -> GoalFuture<'a, bool> {
        Box::pin(async move {
            let Some(pillager) = self.pillager.upgrade() else {
                return false;
            };
            let target = pillager.mob_entity.target.lock().await.clone();
            target.is_some()
        })
    }

    fn should_continue<'a>(&'a self, _mob: &'a dyn Mob) -> GoalFuture<'a, bool> {
        Box::pin(async move {
            let Some(pillager) = self.pillager.upgrade() else {
                return false;
            };
            let target = pillager.mob_entity.target.lock().await.clone();
            target.is_some()
        })
    }

    fn start<'a>(&'a mut self, _mob: &'a dyn Mob) -> GoalFuture<'a, ()> {
        Box::pin(async move {
            self.attack_time = 0;
            self.charge_time = 0;
            self.charged = false;
            self.last_seen = 0;
        })
    }

    fn stop<'a>(&'a mut self, _mob: &'a dyn Mob) -> GoalFuture<'a, ()> {
        Box::pin(async move {
            self.charged = false;
            self.charge_time = 0;
        })
    }

    fn should_run_every_tick(&self) -> bool {
        true
    }

    fn tick<'a>(&'a mut self, _mob: &'a dyn Mob) -> GoalFuture<'a, ()> {
        Box::pin(async move {
            self.attack_time -= 1;

            let Some(pillager) = self.pillager.upgrade() else {
                return;
            };

            let target = pillager.mob_entity.target.lock().await.clone();
            let Some(target) = target else {
                return;
            };

            let ent = &pillager.mob_entity.living_entity.entity;
            let pillager_pos = ent.pos.load();
            let target_pos = target.get_entity().pos.load();

            let dx = target_pos.x - pillager_pos.x;
            let dy = target_pos.y - pillager_pos.y;
            let dz = target_pos.z - pillager_pos.z;
            let distance_sq = dx * dx + dy * dy + dz * dz;

            if distance_sq > Self::get_follow_range_sq() {
                // Too far - lose target tracking
                self.last_seen += 1;
                if self.last_seen >= 100 {
                    pillager.mob_entity.target.lock().await.take();
                }
                return;
            }

            self.last_seen = 0;

            // If target is very close, switch to melee
            if distance_sq < Self::get_melee_range_sq() {
                // The melee goal will take over if present
                return;
            }

            // Charge and fire logic
            if !self.charged {
                if self.charge_time <= 0 {
                    // Start charging
                    self.charge_time = Self::CHARGE_DURATION;
                }
                self.charge_time -= 1;

                if self.charge_time <= 0 {
                    // Charged! Fire on next available tick
                    self.charged = true;
                    self.attack_time = 0;
                }
            } else if self.attack_time <= 0 {
                // Fire the crossbow!
                self.charged = false;
                self.attack_time = Self::RELOAD_COOLDOWN;

                // Spawn an arrow projectile
                let world = ent.world.load();
                let uuid = uuid::Uuid::new_v4();
                let mut pos = pillager_pos;
                pos.y += ent.get_eye_height() - 0.1;

                let direction =
                    Vector3::new(dx, dy + target.get_entity().get_eye_height() - 1.5, dz)
                        .normalize();

                let base_entity = Entity::from_uuid(
                    uuid,
                    world.clone(),
                    pos,
                    &pumpkin_data::entity::EntityType::ARROW,
                );

                let arrow = crate::entity::projectile::arrow::ArrowEntity::new_shot(
                    base_entity,
                    ent,
                    crate::entity::projectile::arrow::ArrowPickup::Disallowed,
                );

                let speed = 1.6;
                arrow.entity.velocity.store(Vector3::new(
                    direction.x * speed,
                    direction.y * speed,
                    direction.z * speed,
                ));

                world.spawn_entity(Arc::new(arrow)).await;
            }

            // Look at target
            pillager.mob_entity.look_control.lock().unwrap().look_at(
                &*pillager,
                target_pos.x,
                target_pos.y + target.get_entity().get_eye_height(),
                target_pos.z,
            );
        })
    }

    fn controls(&self) -> Controls {
        Controls::MOVE | Controls::LOOK
    }
}
