//! Suspicious Sand and Suspicious Gravel block behavior.
//! Handles brush right-click interaction and block entity creation/removal.

use std::sync::Arc;

use pumpkin_data::Block;
use pumpkin_data::item::Item;

use crate::block::entities::brushable::BrushableBlockEntity;
use crate::block::registry::BlockActionResult;
use crate::block::{BlockBehaviour, BlockFuture, BlockMetadata, PlacedArgs, UseWithItemArgs};

/// Handles both `minecraft:suspicious_sand` and `minecraft:suspicious_gravel`.
pub struct SuspiciousSandBlock;

impl BlockMetadata for SuspiciousSandBlock {
    fn ids() -> Box<[pumpkin_data::BlockId]> {
        [Block::SUSPICIOUS_SAND.id, Block::SUSPICIOUS_GRAVEL.id].into()
    }
}

impl BlockBehaviour for SuspiciousSandBlock {
    fn placed<'a>(&'a self, args: PlacedArgs<'a>) -> BlockFuture<'a, ()> {
        Box::pin(async move {
            let is_sand = args.block == &Block::SUSPICIOUS_SAND;
            let entity = Arc::new(BrushableBlockEntity::new(*args.position, is_sand));
            // Generate random archaeology loot
            entity.generate_random_loot().await;
            args.world.add_block_entity(entity);
        })
    }

    fn broken<'a>(&'a self, args: crate::block::BrokenArgs<'a>) -> BlockFuture<'a, ()> {
        Box::pin(async move {
            args.world.remove_block_entity(args.position);
        })
    }

    fn use_with_item<'a>(
        &'a self,
        args: UseWithItemArgs<'a>,
    ) -> BlockFuture<'a, BlockActionResult> {
        Box::pin(async move {
            if args.item_stack.lock().await.item != &Item::BRUSH {
                return BlockActionResult::Pass;
            }

            if let Some(be) = args.world.get_block_entity(args.position) {
                if let Some(brushable) = be.as_any().downcast_ref::<BrushableBlockEntity>() {
                    brushable.brush(args.world);
                    return BlockActionResult::Success;
                }
            }

            BlockActionResult::Pass
        })
    }
}
