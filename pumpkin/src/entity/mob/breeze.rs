use std::sync::{Arc, Weak};

use pumpkin_data::entity::EntityType;

use crate::entity::{
    Entity, NBTStorage,
    ai::goal::{
        active_target::ActiveTargetGoal, look_around::RandomLookAroundGoal,
        look_at_entity::LookAtEntityGoal, swim::SwimGoal, wander_around::WanderAroundGoal,
    },
    mob::{Mob, MobEntity},
};

/// The Breeze is a hostile mob found in trial chambers.
/// It attacks by shooting wind charges at players and leaps to evade.
pub struct BreezeEntity {
    pub mob_entity: MobEntity,
}

impl BreezeEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let breeze = Self { mob_entity };
        let mob_arc = Arc::new(breeze);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            let mut goal_selector = mob_arc.mob_entity.goals_selector.lock().unwrap();

            // Priority 0: Swim (survival)
            goal_selector.add_goal(0, Box::new(SwimGoal::default()));

            // Priority 2: Shoot wind charges at target
            goal_selector.add_goal(
                2,
                Box::new(
                    crate::entity::ai::goal::breeze_attack::BreezeShootWindChargeGoal::new(
                        Arc::downgrade(&mob_arc),
                    ),
                ),
            );

            // Priority 5: Wander around when not attacking
            goal_selector.add_goal(5, Box::new(WanderAroundGoal::new(1.0)));

            // Priority 6: Look at players
            goal_selector.add_goal(
                6,
                LookAtEntityGoal::with_default(mob_weak.clone(), &EntityType::PLAYER, 8.0),
            );

            // Priority 7: Random glances
            goal_selector.add_goal(7, Box::new(RandomLookAroundGoal::default()));

            // Target selector: actively target players
            let mut target_selector = mob_arc.mob_entity.target_selector.lock().unwrap();
            target_selector.add_goal(
                1,
                ActiveTargetGoal::with_default(&mob_arc.mob_entity, &EntityType::PLAYER, true),
            );
        };

        mob_arc
    }
}

impl NBTStorage for BreezeEntity {}

impl Mob for BreezeEntity {
    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }
}
