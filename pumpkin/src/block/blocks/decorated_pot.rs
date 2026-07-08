//! Decorated Pot block behavior. Handles creation/removal of the
//! DecoratedPotBlockEntity on placement/break, reading NBT from the
//! placing item, and right-click insertion of pottery sherds.

use std::sync::Arc;

use pumpkin_macros::pumpkin_block;

use crate::block::entities::decorated_pot::{DecoratedPotBlockEntity, PotDecorations};
use crate::block::registry::BlockActionResult;
use crate::block::{
    BlockBehaviour, BlockFuture, BrokenArgs, PlacedArgs, PlayerPlacedArgs, UseWithItemArgs,
};
use pumpkin_data::tag;
use pumpkin_data::tag::Taggable;

/// Handles `minecraft:decorated_pot`.
#[pumpkin_block("minecraft:decorated_pot")]
pub struct DecoratedPotBlock;

impl BlockBehaviour for DecoratedPotBlock {
    fn placed<'a>(&'a self, args: PlacedArgs<'a>) -> BlockFuture<'a, ()> {
        Box::pin(async move {
            let entity = Arc::new(DecoratedPotBlockEntity::new(*args.position));
            args.world.add_block_entity(entity);
        })
    }

    fn player_placed<'a>(&'a self, args: PlayerPlacedArgs<'a>) -> BlockFuture<'a, ()> {
        Box::pin(async move {
            // Read the BlockEntityData NBT from the placed item to initialize decorations
            let held_stack = args.player.inventory().held_item();
            let held_stack = held_stack.lock().await;

            let mut decorations = PotDecorations::default();

            if let Some(block_entity_data) = held_stack
                .get_data_component::<pumpkin_data::data_component_impl::BlockEntityDataImpl>(
            ) {
                if let Some(sherd_list) = block_entity_data.nbt.get_list("sherds") {
                    for (i, tag) in sherd_list.iter().enumerate() {
                        if let Some(s) = tag.extract_string() {
                            match i {
                                0 => decorations.back = Some(s.to_string()),
                                1 => decorations.left = Some(s.to_string()),
                                2 => decorations.right = Some(s.to_string()),
                                3 => decorations.front = Some(s.to_string()),
                                _ => {}
                            }
                        }
                    }
                }
            }

            let entity = Arc::new(DecoratedPotBlockEntity {
                position: *args.position,
                decorations: tokio::sync::Mutex::new(decorations),
                stored_item: tokio::sync::Mutex::new(None),
            });
            args.world.add_block_entity(entity);
        })
    }

    fn broken<'a>(&'a self, args: BrokenArgs<'a>) -> BlockFuture<'a, ()> {
        Box::pin(async move {
            args.world.remove_block_entity(args.position);
        })
    }

    fn use_with_item<'a>(
        &'a self,
        args: UseWithItemArgs<'a>,
    ) -> BlockFuture<'a, BlockActionResult> {
        Box::pin(async move {
            let stack = args.item_stack.lock().await;
            // Check if the held item is a valid decorated pot ingredient (sherd or brick)
            if !stack
                .item
                .has_tag(&tag::Item::MINECRAFT_DECORATED_POT_INGREDIENTS)
            {
                return BlockActionResult::Pass;
            }

            let sherd_id = stack.item.registry_key.to_string();

            if let Some(be) = args.world.get_block_entity(args.position) {
                if let Some(pot) = be.as_any().downcast_ref::<DecoratedPotBlockEntity>() {
                    let mut decorations = pot.decorations.lock().await;

                    // Insert into the first empty slot
                    if decorations.back.is_none() {
                        decorations.back = Some(sherd_id);
                    } else if decorations.left.is_none() {
                        decorations.left = Some(sherd_id);
                    } else if decorations.right.is_none() {
                        decorations.right = Some(sherd_id);
                    } else if decorations.front.is_none() {
                        decorations.front = Some(sherd_id);
                    } else {
                        return BlockActionResult::Pass;
                    }

                    return BlockActionResult::Success;
                }
            }

            BlockActionResult::Pass
        })
    }
}
