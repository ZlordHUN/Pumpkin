//! Brushable block entity for suspicious sand and suspicious gravel.
//! Handles the brushing mechanic: right-clicking with a brush
//! increments a `dusted` counter (0→1→2→3), playing sounds and
//! spawning particles. When fully dusted (3), the contained item
//! is revealed and dropped, and the block converts to regular sand/gravel.

use crate::block::entities::BlockEntity;
use crate::world::World;
use crossbeam::atomic::AtomicCell;
use pumpkin_data::block_properties::BlockProperties;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_nbt::tag::NbtTag;
use pumpkin_util::math::position::BlockPos;
use pumpkin_world::world::BlockFlags;
use std::any::Any;
use std::pin::Pin;
use std::sync::Arc;

pub struct BrushableBlockEntity {
    pub position: BlockPos,
    /// 0 = untouched, 1-2 = partially brushed, 3 = fully brushed (item revealed)
    pub dusted: AtomicCell<u8>,
    /// Whether this is suspicious sand (true) or suspicious gravel (false).
    pub is_sand: AtomicCell<bool>,
    /// The item that will drop when fully brushed (set during worldgen).
    pub item: Arc<tokio::sync::Mutex<Option<ItemStack>>>,
    /// Ticks since the last brush stroke (for cooldown).
    pub brush_cooldown: AtomicCell<u8>,
}

impl BrushableBlockEntity {
    pub const ID: &'static str = "minecraft:brushable_block";

    #[must_use]
    pub fn new(position: BlockPos, is_sand: bool) -> Self {
        Self {
            position,
            dusted: AtomicCell::new(0),
            is_sand: AtomicCell::new(is_sand),
            item: Arc::new(tokio::sync::Mutex::new(None)),
            brush_cooldown: AtomicCell::new(0),
        }
    }

    /// Called on right-click with a brush. Returns true if the block was brushed.
    pub fn brush(&self, world: &Arc<World>) -> bool {
        let current = self.dusted.load();
        if current >= 3 {
            return false;
        }

        let cooldown = self.brush_cooldown.load();
        if cooldown > 0 {
            return false;
        }
        self.brush_cooldown.store(6);

        let new_dusted = (current + 1).min(3);
        self.dusted.store(new_dusted);

        let is_sand = self.is_sand.load();
        let pos = self.position;

        // Play brushing sound
        let sound = match (new_dusted, is_sand) {
            (3, true) => Sound::ItemBrushBrushingSandComplete,
            (3, false) => Sound::ItemBrushBrushingGravelComplete,
            (_, true) => Sound::ItemBrushBrushingSand,
            (_, false) => Sound::ItemBrushBrushingGravel,
        };

        world.play_sound_fine(
            sound,
            SoundCategory::Blocks,
            &pos.to_centered_f64(),
            1.0,
            1.0,
        );

        // Update block state to reflect new dusted level
        let new_state_id = get_dusted_state_id(is_sand, new_dusted);
        let world_clone = world.clone();
        let pos_clone = pos;
        tokio::spawn(async move {
            world_clone
                .set_block_state(&pos_clone, new_state_id, BlockFlags::NOTIFY_ALL)
                .await;
        });

        // If fully dusted, schedule item drop and block conversion
        if new_dusted >= 3 {
            let world_clone = world.clone();
            let pos_clone = pos;
            let item_arc = self.item.clone();
            let is_sand_val = is_sand;

            tokio::spawn(async move {
                // Drop the contained item if present
                let item_opt = item_arc.lock().await.take();
                if let Some(stack) = item_opt {
                    world_clone.drop_stack(&pos_clone, stack).await;
                }

                // Convert to regular sand/gravel
                let replacement = if is_sand_val {
                    pumpkin_data::Block::SAND
                } else {
                    pumpkin_data::Block::GRAVEL
                };
                world_clone
                    .set_block_state(
                        &pos_clone,
                        replacement.default_state.id,
                        BlockFlags::NOTIFY_ALL,
                    )
                    .await;
            });
        }

        true
    }

    /// Sets the item that will be revealed when fully brushed.
    pub async fn set_item(&self, stack: ItemStack) {
        *self.item.lock().await = Some(stack);
    }
}

impl BlockEntity for BrushableBlockEntity {
    fn write_nbt<'a>(
        &'a self,
        nbt: &'a mut NbtCompound,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            nbt.put_int("dusted", i32::from(self.dusted.load()));
            if let Some(ref item) = *self.item.lock().await {
                let mut item_nbt = NbtCompound::new();
                item_nbt.put_string("id", item.item.registry_key.to_string());
                item_nbt.put_byte("Count", item.item_count as i8);
                nbt.put("item", NbtTag::Compound(item_nbt));
            }
        })
    }

    fn from_nbt(nbt: &NbtCompound, position: BlockPos) -> Self
    where
        Self: Sized,
    {
        let dusted = nbt.get_int("dusted").map_or(0, |d| d as u8);
        let item = nbt
            .get_compound("item")
            .and_then(ItemStack::read_item_stack)
            .map(|s| tokio::sync::Mutex::new(Some(s)))
            .unwrap_or_else(|| tokio::sync::Mutex::new(None));

        // Default to sand; will be corrected when the block entity is first ticked or placed
        let is_sand = true;

        Self {
            position,
            dusted: AtomicCell::new(dusted),
            is_sand: AtomicCell::new(is_sand),
            item: Arc::new(item),
            brush_cooldown: AtomicCell::new(0),
        }
    }

    fn tick<'a>(&'a self, _world: &'a Arc<World>) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            let cd = self.brush_cooldown.load();
            if cd > 0 {
                self.brush_cooldown.store(cd - 1);
            }
        })
    }

    fn resource_location(&self) -> &'static str {
        Self::ID
    }

    fn get_position(&self) -> BlockPos {
        self.position
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Returns the block state ID for the given dusted level.
#[must_use]
fn get_dusted_state_id(is_sand: bool, dusted: u8) -> pumpkin_data::BlockStateId {
    use pumpkin_data::block_properties::SuspiciousSandLikeProperties;

    let block = if is_sand {
        &pumpkin_data::Block::SUSPICIOUS_SAND
    } else {
        &pumpkin_data::Block::SUSPICIOUS_GRAVEL
    };

    let props = SuspiciousSandLikeProperties {
        r#dusted: dusted.min(3),
    };
    props.to_state_id(block)
}

/// Vanilla archaeology loot pool for suspicious sand (desert pyramid / trail ruins).
/// Contains pottery sherds, sniffer egg, armor trims, and other rare items.
static SUSPICIOUS_SAND_LOOT: &[(&Item, u8, u8, i32)] = &[
    // (item reference, min_count, max_count, weight)
    (&Item::ANGLER_POTTERY_SHERD, 1, 1, 2),
    (&Item::ARCHER_POTTERY_SHERD, 1, 1, 2),
    (&Item::ARMS_UP_POTTERY_SHERD, 1, 1, 2),
    (&Item::BLADE_POTTERY_SHERD, 1, 1, 2),
    (&Item::BREWER_POTTERY_SHERD, 1, 1, 2),
    (&Item::BURN_POTTERY_SHERD, 1, 1, 2),
    (&Item::DANGER_POTTERY_SHERD, 1, 1, 2),
    (&Item::EXPLORER_POTTERY_SHERD, 1, 1, 2),
    (&Item::FRIEND_POTTERY_SHERD, 1, 1, 2),
    (&Item::HEART_POTTERY_SHERD, 1, 1, 2),
    (&Item::HEARTBREAK_POTTERY_SHERD, 1, 1, 2),
    (&Item::HOWL_POTTERY_SHERD, 1, 1, 2),
    (&Item::MINER_POTTERY_SHERD, 1, 1, 2),
    (&Item::MOURNER_POTTERY_SHERD, 1, 1, 2),
    (&Item::PLENTY_POTTERY_SHERD, 1, 1, 2),
    (&Item::PRIZE_POTTERY_SHERD, 1, 1, 2),
    (&Item::SHEAF_POTTERY_SHERD, 1, 1, 2),
    // Rare items
    (&Item::SNIFFER_EGG, 1, 1, 1),
    (&Item::DUNE_ARMOR_TRIM_SMITHING_TEMPLATE, 1, 1, 1),
    (&Item::COAST_ARMOR_TRIM_SMITHING_TEMPLATE, 1, 1, 1),
    (&Item::EYE_ARMOR_TRIM_SMITHING_TEMPLATE, 1, 1, 1),
    (&Item::RIB_ARMOR_TRIM_SMITHING_TEMPLATE, 1, 1, 1),
    (&Item::SENTRY_ARMOR_TRIM_SMITHING_TEMPLATE, 1, 1, 1),
    (&Item::SNOUT_ARMOR_TRIM_SMITHING_TEMPLATE, 1, 1, 1),
    (&Item::SPIRE_ARMOR_TRIM_SMITHING_TEMPLATE, 1, 1, 1),
    (&Item::TIDE_ARMOR_TRIM_SMITHING_TEMPLATE, 1, 1, 1),
    (&Item::VEX_ARMOR_TRIM_SMITHING_TEMPLATE, 1, 1, 1),
    (&Item::WARD_ARMOR_TRIM_SMITHING_TEMPLATE, 1, 1, 1),
    (&Item::WILD_ARMOR_TRIM_SMITHING_TEMPLATE, 1, 1, 1),
    // Common archaeology loot
    (&Item::EMERALD, 1, 1, 3),
    (&Item::DIAMOND, 1, 1, 2),
    (&Item::GOLD_NUGGET, 1, 3, 3),
    (&Item::IRON_NUGGET, 1, 3, 3),
    (&Item::TNT, 1, 1, 2),
    (&Item::GUNPOWDER, 1, 3, 3),
    (&Item::WHEAT, 1, 2, 2),
    (&Item::COAL, 1, 2, 2),
    (&Item::STICK, 1, 3, 2),
    (&Item::STRING, 1, 3, 2),
    (&Item::LEAD, 1, 1, 1),
    (&Item::CANDLE, 1, 2, 1),
    (&Item::GLASS_BOTTLE, 1, 1, 1),
];

impl BrushableBlockEntity {
    /// Generates and assigns a random archaeology loot item to this block.
    /// Called when suspicious sand/gravel is placed by world generation.
    pub async fn generate_random_loot(&self) {
        use rand::RngExt;

        let result = {
            let mut rng = rand::rng();
            let total_weight: i32 = SUSPICIOUS_SAND_LOOT.iter().map(|(_, _, _, w)| w).sum();
            let mut roll = rng.random_range(0..total_weight);
            let mut result = None;

            for &(item, min, max, weight) in SUSPICIOUS_SAND_LOOT {
                roll -= weight;
                if roll < 0 {
                    let count = rng.random_range(min as i32..=max as i32) as u8;
                    result = Some((item, count.max(1)));
                    break;
                }
            }
            result
        };

        if let Some((item, count)) = result {
            let stack = ItemStack::new(count, item);
            self.set_item(stack).await;
        }
    }
}
