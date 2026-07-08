use std::borrow::Cow;
use std::sync::{Arc, Weak};

use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_protocol::codec::item_stack_seralizer::ItemStackSerializer;
use pumpkin_protocol::codec::var_int::VarInt;
use pumpkin_protocol::java::client::play::{CMerchantOffers, MerchantOffer};
use rand::prelude::IndexedRandom;

use crate::entity::player::Player;
use crate::entity::{
    Entity, EntityBase, EntityBaseFuture, NBTStorage,
    ai::goal::{
        look_around::RandomLookAroundGoal, look_at_entity::LookAtEntityGoal, swim::SwimGoal,
        wander_around::WanderAroundGoal,
    },
    mob::{Mob, MobEntity},
};

/// Trade definition for wandering trader.
struct WanderTrade {
    wants_item: &'static Item,
    wants_count: i32,
    gives_item: &'static Item,
    gives_count: i32,
    max_uses: i32,
}

/// The full pool of possible wandering trader trades (vanilla 1.21).
const WANDER_TRADES: &[WanderTrade] = &[
    WanderTrade {
        wants_item: &Item::EMERALD,
        wants_count: 1,
        gives_item: &Item::TUBE_CORAL_BLOCK,
        gives_count: 1,
        max_uses: 8,
    },
    WanderTrade {
        wants_item: &Item::EMERALD,
        wants_count: 1,
        gives_item: &Item::BRAIN_CORAL_BLOCK,
        gives_count: 1,
        max_uses: 8,
    },
    WanderTrade {
        wants_item: &Item::EMERALD,
        wants_count: 1,
        gives_item: &Item::BUBBLE_CORAL_BLOCK,
        gives_count: 1,
        max_uses: 8,
    },
    WanderTrade {
        wants_item: &Item::EMERALD,
        wants_count: 1,
        gives_item: &Item::FIRE_CORAL_BLOCK,
        gives_count: 1,
        max_uses: 8,
    },
    WanderTrade {
        wants_item: &Item::EMERALD,
        wants_count: 1,
        gives_item: &Item::HORN_CORAL_BLOCK,
        gives_count: 1,
        max_uses: 8,
    },
    WanderTrade {
        wants_item: &Item::EMERALD,
        wants_count: 1,
        gives_item: &Item::SEA_PICKLE,
        gives_count: 1,
        max_uses: 5,
    },
    WanderTrade {
        wants_item: &Item::EMERALD,
        wants_count: 4,
        gives_item: &Item::SLIME_BALL,
        gives_count: 1,
        max_uses: 5,
    },
    WanderTrade {
        wants_item: &Item::EMERALD,
        wants_count: 5,
        gives_item: &Item::GLOWSTONE_DUST,
        gives_count: 1,
        max_uses: 5,
    },
    WanderTrade {
        wants_item: &Item::EMERALD,
        wants_count: 2,
        gives_item: &Item::FERN,
        gives_count: 1,
        max_uses: 8,
    },
    WanderTrade {
        wants_item: &Item::EMERALD,
        wants_count: 3,
        gives_item: &Item::SUGAR_CANE,
        gives_count: 1,
        max_uses: 8,
    },
    WanderTrade {
        wants_item: &Item::EMERALD,
        wants_count: 5,
        gives_item: &Item::PUMPKIN,
        gives_count: 1,
        max_uses: 4,
    },
    WanderTrade {
        wants_item: &Item::EMERALD,
        wants_count: 1,
        gives_item: &Item::KELP,
        gives_count: 3,
        max_uses: 8,
    },
    WanderTrade {
        wants_item: &Item::EMERALD,
        wants_count: 3,
        gives_item: &Item::CACTUS,
        gives_count: 1,
        max_uses: 8,
    },
    WanderTrade {
        wants_item: &Item::EMERALD,
        wants_count: 4,
        gives_item: &Item::SAND,
        gives_count: 8,
        max_uses: 8,
    },
    WanderTrade {
        wants_item: &Item::EMERALD,
        wants_count: 5,
        gives_item: &Item::RED_SAND,
        gives_count: 4,
        max_uses: 6,
    },
    WanderTrade {
        wants_item: &Item::EMERALD,
        wants_count: 1,
        gives_item: &Item::VINE,
        gives_count: 1,
        max_uses: 8,
    },
    WanderTrade {
        wants_item: &Item::EMERALD,
        wants_count: 3,
        gives_item: &Item::LILY_PAD,
        gives_count: 1,
        max_uses: 7,
    },
    WanderTrade {
        wants_item: &Item::EMERALD,
        wants_count: 5,
        gives_item: &Item::SMALL_DRIPLEAF,
        gives_count: 1,
        max_uses: 8,
    },
    WanderTrade {
        wants_item: &Item::EMERALD,
        wants_count: 1,
        gives_item: &Item::MOSS_BLOCK,
        gives_count: 2,
        max_uses: 5,
    },
    WanderTrade {
        wants_item: &Item::EMERALD,
        wants_count: 1,
        gives_item: &Item::POINTED_DRIPSTONE,
        gives_count: 1,
        max_uses: 8,
    },
    WanderTrade {
        wants_item: &Item::EMERALD,
        wants_count: 2,
        gives_item: &Item::ROOTED_DIRT,
        gives_count: 1,
        max_uses: 5,
    },
    WanderTrade {
        wants_item: &Item::EMERALD,
        wants_count: 5,
        gives_item: &Item::NAUTILUS_SHELL,
        gives_count: 1,
        max_uses: 5,
    },
    WanderTrade {
        wants_item: &Item::EMERALD,
        wants_count: 4,
        gives_item: &Item::GUNPOWDER,
        gives_count: 1,
        max_uses: 5,
    },
    WanderTrade {
        wants_item: &Item::EMERALD,
        wants_count: 1,
        gives_item: &Item::BROWN_MUSHROOM,
        gives_count: 3,
        max_uses: 8,
    },
    WanderTrade {
        wants_item: &Item::EMERALD,
        wants_count: 1,
        gives_item: &Item::RED_MUSHROOM,
        gives_count: 3,
        max_uses: 8,
    },
    WanderTrade {
        wants_item: &Item::EMERALD,
        wants_count: 5,
        gives_item: &Item::PUFFERFISH_BUCKET,
        gives_count: 1,
        max_uses: 4,
    },
    WanderTrade {
        wants_item: &Item::EMERALD,
        wants_count: 5,
        gives_item: &Item::TROPICAL_FISH_BUCKET,
        gives_count: 1,
        max_uses: 4,
    },
    WanderTrade {
        wants_item: &Item::EMERALD,
        wants_count: 6,
        gives_item: &Item::PACKED_ICE,
        gives_count: 1,
        max_uses: 6,
    },
    WanderTrade {
        wants_item: &Item::EMERALD,
        wants_count: 1,
        gives_item: &Item::MANGROVE_PROPAGULE,
        gives_count: 1,
        max_uses: 8,
    },
    WanderTrade {
        wants_item: &Item::EMERALD,
        wants_count: 1,
        gives_item: &Item::BLUE_ICE,
        gives_count: 1,
        max_uses: 6,
    },
];

pub struct WanderingTraderEntity {
    pub mob_entity: MobEntity,
    pub offers: tokio::sync::Mutex<Vec<MerchantOffer>>,
    /// Ticks until the trader despawns (48000 = 40 minutes).
    pub despawn_timer: std::sync::atomic::AtomicI32,
}

impl WanderingTraderEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let trader = Self {
            mob_entity,
            offers: tokio::sync::Mutex::new(Vec::new()),
            despawn_timer: std::sync::atomic::AtomicI32::new(48000),
        };
        let mob_arc = Arc::new(trader);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            let mut goal_selector = mob_arc.mob_entity.goals_selector.lock().unwrap();

            goal_selector.add_goal(0, Box::new(SwimGoal::default()));
            goal_selector.add_goal(1, Box::new(WanderAroundGoal::new(0.6)));
            goal_selector.add_goal(
                2,
                LookAtEntityGoal::with_default(mob_weak, &EntityType::PLAYER, 8.0),
            );
            goal_selector.add_goal(3, Box::new(RandomLookAroundGoal::default()));
        }

        mob_arc
    }

    /// Generates 6 random trades from the pool on first interaction.
    async fn generate_trades(&self) {
        let mut offers = self.offers.lock().await;
        if !offers.is_empty() {
            return;
        }

        let mut rng = rand::rng();
        let chosen = WANDER_TRADES.sample(&mut rng, 6);

        for trade in chosen {
            offers.push(MerchantOffer {
                base_cost_a: ItemStackSerializer(Cow::Owned(ItemStack::new(
                    trade.wants_count as u8,
                    trade.wants_item,
                ))),
                output: ItemStackSerializer(Cow::Owned(ItemStack::new(
                    trade.gives_count as u8,
                    trade.gives_item,
                ))),
                cost_b: None,
                is_disabled: false,
                uses: 0,
                max_uses: trade.max_uses,
                xp: 1,
                special_price: 0,
                price_multiplier: 1.0,
                demand: 0,
            });
        }
    }
}

impl NBTStorage for WanderingTraderEntity {}

impl Mob for WanderingTraderEntity {
    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn mob_interact<'a>(
        &'a self,
        player: &'a Arc<Player>,
        _item_stack: &'a mut ItemStack,
    ) -> EntityBaseFuture<'a, bool> {
        Box::pin(async move {
            self.open_trading_screen(player).await;
            true
        })
    }

    fn mob_tick<'a>(&'a self, _caller: &'a Arc<dyn EntityBase>) -> EntityBaseFuture<'a, ()> {
        Box::pin(async move {
            let age = self
                .mob_entity
                .living_entity
                .entity
                .age
                .load(std::sync::atomic::Ordering::Relaxed);
            if age % 20 != 0 {
                return;
            }

            // Despawn timer
            let timer = self
                .despawn_timer
                .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            if timer <= 0 {
                let entity = &self.mob_entity.living_entity.entity;
                entity
                    .removal_reason
                    .store(Some(crate::entity::RemovalReason::Discarded));
                entity.remove().await;
            }
        })
    }
}

impl WanderingTraderEntity {
    async fn open_trading_screen(&self, player: &Arc<Player>) {
        self.generate_trades().await;

        let offers = self.offers.lock().await.clone();
        if offers.is_empty() {
            return;
        }

        player
            .client
            .enqueue_packet(&CMerchantOffers::new(
                VarInt(0),
                offers,
                VarInt(1),
                VarInt(0),
                false,
                false,
            ))
            .await;
    }
}
