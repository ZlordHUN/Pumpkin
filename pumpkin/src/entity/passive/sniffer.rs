use std::sync::{Arc, Weak};

use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;

use crate::entity::{
    Entity, EntityBase, EntityBaseFuture, NBTStorage,
    ai::goal::{
        breed::BreedGoal, look_around::RandomLookAroundGoal, look_at_entity::LookAtEntityGoal,
        swim::SwimGoal, tempt::TemptGoal, wander_around::WanderAroundGoal,
    },
    mob::{Mob, MobEntity},
    player::Player,
};

/// A sniffer is tempted by and breeds with torchflower seeds.
const SNIFFER_FOOD: &[&Item] = &[&Item::TORCHFLOWER_SEEDS];

pub struct SnifferEntity {
    pub mob_entity: MobEntity,
}

impl SnifferEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let sniffer = Self { mob_entity };
        let mob_arc = Arc::new(sniffer);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            let mut goal_selector = mob_arc.mob_entity.goals_selector.lock().unwrap();

            goal_selector.add_goal(0, Box::new(SwimGoal::default()));
            goal_selector.add_goal(1, BreedGoal::new(1.0));
            goal_selector.add_goal(2, Box::new(TemptGoal::new(1.2, SNIFFER_FOOD)));
            goal_selector.add_goal(3, Box::new(WanderAroundGoal::new(1.0)));
            goal_selector.add_goal(
                4,
                LookAtEntityGoal::with_default(mob_weak, &EntityType::PLAYER, 6.0),
            );
            goal_selector.add_goal(5, Box::new(RandomLookAroundGoal::default()));
        };

        mob_arc
    }
}

impl NBTStorage for SnifferEntity {}

impl Mob for SnifferEntity {
    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn mob_interact<'a>(
        &'a self,
        player: &'a Arc<Player>,
        item_stack: &'a mut ItemStack,
    ) -> EntityBaseFuture<'a, bool> {
        Box::pin(async move {
            let is_food = SNIFFER_FOOD.iter().any(|i| *i == item_stack.item);

            if is_food && self.is_breeding_ready() && !self.is_in_love() {
                item_stack.decrement_unless_creative(player.gamemode.load(), 1);
                self.mob_entity
                    .set_love_ticks(600, Some(player.gameprofile.id));

                // Spawn heart particles
                let entity = &self.mob_entity.living_entity.entity;
                entity
                    .world
                    .load()
                    .send_entity_status(entity, pumpkin_data::entity::EntityStatus::InLoveHearts);
                return true;
            }

            false
        })
    }
}
