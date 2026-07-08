use std::sync::{Arc, Weak};

use pumpkin_data::entity::EntityType;

use crate::entity::{
    Entity, NBTStorage,
    ai::goal::{
        active_target::ActiveTargetGoal, look_around::RandomLookAroundGoal,
        look_at_entity::LookAtEntityGoal, melee_attack::MeleeAttackGoal, swim::SwimGoal,
        wander_around::WanderAroundGoal,
    },
    mob::{Mob, MobEntity},
};

/// Pillager entity - hostile illager found in pillager outposts and raids.
/// Wields a crossbow for ranged attacks and switches to melee when close.
pub struct PillagerEntity {
    pub mob_entity: MobEntity,
}

impl PillagerEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let pillager = Self { mob_entity };
        let mob_arc = Arc::new(pillager);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            let mut goal_selector = mob_arc.mob_entity.goals_selector.lock().unwrap();

            // Priority 0: Swim (survival)
            goal_selector.add_goal(0, Box::new(SwimGoal::default()));

            // Priority 1: Crossbow ranged attack
            goal_selector.add_goal(
                1,
                Box::new(
                    crate::entity::ai::goal::crossbow_attack::PillagerCrossbowAttackGoal::new(
                        Arc::downgrade(&mob_arc),
                    ),
                ),
            );

            // Priority 2: Melee for close targets
            goal_selector.add_goal(2, Box::new(MeleeAttackGoal::new(1.0, true)));

            // Priority 5: Wander when no target
            goal_selector.add_goal(5, Box::new(WanderAroundGoal::new(1.0)));

            // Priority 6: Look at players
            goal_selector.add_goal(
                6,
                LookAtEntityGoal::with_default(mob_weak.clone(), &EntityType::PLAYER, 8.0),
            );

            // Priority 7: Random glances
            goal_selector.add_goal(7, Box::new(RandomLookAroundGoal::default()));

            // Target selector
            let mut target_selector = mob_arc.mob_entity.target_selector.lock().unwrap();
            target_selector.add_goal(
                1,
                ActiveTargetGoal::with_default(&mob_arc.mob_entity, &EntityType::PLAYER, true),
            );
            target_selector.add_goal(
                2,
                ActiveTargetGoal::with_default(&mob_arc.mob_entity, &EntityType::VILLAGER, true),
            );
            target_selector.add_goal(
                3,
                ActiveTargetGoal::with_default(&mob_arc.mob_entity, &EntityType::IRON_GOLEM, true),
            );
        };

        mob_arc
    }
}

impl NBTStorage for PillagerEntity {}

impl Mob for PillagerEntity {
    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }
}
