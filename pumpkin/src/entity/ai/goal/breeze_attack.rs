//! Breeze wind charge attack goal. Shoots wind charges at the target player
//! in bursts of 3 shots per cycle, with a cooldown between cycles.

use pumpkin_protocol::java::client::play::CWorldEvent;
use pumpkin_util::math::vector3::Vector3;
use std::sync::{Arc, Weak};

use crate::entity::{
    Entity,
    ai::goal::{Controls, Goal, GoalFuture},
    mob::Mob,
    mob::breeze::BreezeEntity,
};

pub struct BreezeShootWindChargeGoal {
    breeze: Weak<BreezeEntity>,
    attack_time: i32,
    cooldown_until: i32,
    shots_fired: i32,
}

impl BreezeShootWindChargeGoal {
    #[must_use]
    pub const fn new(breeze: Weak<BreezeEntity>) -> Self {
        Self {
            breeze,
            attack_time: 0,
            cooldown_until: 0,
            shots_fired: 0,
        }
    }

    const SHOTS_PER_BURST: i32 = 3;
    const SHOT_DELAY: i32 = 4;
    const CYCLE_COOLDOWN: i32 = 20;
}

impl Goal for BreezeShootWindChargeGoal {
    fn can_start<'a>(&'a mut self, _mob: &'a dyn Mob) -> GoalFuture<'a, bool> {
        Box::pin(async move {
            if self.cooldown_until > 0 {
                return false;
            }
            let Some(breeze) = self.breeze.upgrade() else {
                return false;
            };
            let target = breeze.mob_entity.target.lock().await.clone();
            target.is_some()
        })
    }

    fn should_continue<'a>(&'a self, _mob: &'a dyn Mob) -> GoalFuture<'a, bool> {
        Box::pin(async move {
            let Some(breeze) = self.breeze.upgrade() else {
                return false;
            };
            let target = breeze.mob_entity.target.lock().await.clone();
            target.is_some()
        })
    }

    fn start<'a>(&'a mut self, _mob: &'a dyn Mob) -> GoalFuture<'a, ()> {
        Box::pin(async move {
            self.attack_time = 0;
            self.shots_fired = 0;
        })
    }

    fn stop<'a>(&'a mut self, _mob: &'a dyn Mob) -> GoalFuture<'a, ()> {
        Box::pin(async move {
            self.cooldown_until = Self::CYCLE_COOLDOWN;
            self.shots_fired = 0;
        })
    }

    fn should_run_every_tick(&self) -> bool {
        true
    }

    fn tick<'a>(&'a mut self, _mob: &'a dyn Mob) -> GoalFuture<'a, ()> {
        Box::pin(async move {
            if self.cooldown_until > 0 {
                self.cooldown_until -= 1;
                return;
            }

            self.attack_time -= 1;

            let Some(breeze) = self.breeze.upgrade() else {
                return;
            };

            let target = breeze.mob_entity.target.lock().await.clone();
            let Some(target) = target else {
                return;
            };

            let ent = &breeze.mob_entity.living_entity.entity;
            let breeze_pos = ent.pos.load();
            let target_pos = target.get_entity().pos.load();

            let dx = target_pos.x - breeze_pos.x;
            let dy = target_pos.y - breeze_pos.y + 0.5;
            let dz = target_pos.z - breeze_pos.z;

            if self.attack_time <= 0 {
                if self.shots_fired < Self::SHOTS_PER_BURST {
                    self.attack_time = Self::SHOT_DELAY;
                    self.shots_fired += 1;

                    let direction = Vector3::new(dx, dy, dz).normalize();

                    // Spawn a breeze wind charge
                    let world = ent.world.load();
                    let uuid = uuid::Uuid::new_v4();
                    let mut pos = breeze_pos;
                    pos.y += ent.get_eye_height() - 0.1;

                    let base_entity = Entity::from_uuid(
                        uuid,
                        world.clone(),
                        pos,
                        &pumpkin_data::entity::EntityType::BREEZE_WIND_CHARGE,
                    );

                    let thrown = crate::entity::projectile::ThrownItemEntity::new(
                        base_entity,
                        ent,
                        crate::entity::projectile::wind_charge::WIND_CHARGE_GRAVITY,
                    );

                    let wind_charge =
                        crate::entity::projectile::wind_charge::WindChargeEntity::new_breeze(
                            thrown,
                        );

                    let speed = 1.5;
                    wind_charge
                        .thrown_item_entity
                        .entity
                        .velocity
                        .store(Vector3::new(
                            direction.x * speed,
                            direction.y * speed,
                            direction.z * speed,
                        ));

                    world.spawn_entity(Arc::new(wind_charge)).await;

                    // Play shoot sound
                    let chunk_pos = ent.chunk_pos.load();
                    world.broadcast_to_chunk(
                        chunk_pos,
                        &CWorldEvent::new(1018, ent.block_pos.load(), 0, false),
                    );
                } else {
                    self.cooldown_until = Self::CYCLE_COOLDOWN;
                    self.shots_fired = 0;
                }
            }

            // Look at target
            breeze.mob_entity.look_control.lock().unwrap().look_at(
                &*breeze,
                target_pos.x,
                target_pos.y,
                target_pos.z,
            );
        })
    }

    fn controls(&self) -> Controls {
        Controls::MOVE | Controls::LOOK
    }
}
