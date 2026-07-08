//! Vault block behavior for trial chambers. Handles right-click interaction
//! with trial keys and manages the VaultBlockEntity.

use pumpkin_data::Block;
use std::sync::Arc;

use crate::block::entities::vault::VaultBlockEntity;
use crate::block::registry::BlockActionResult;
use crate::block::{BlockBehaviour, BlockFuture, BlockMetadata, PlacedArgs, UseWithItemArgs};
use pumpkin_data::item::Item;

/// Handles `minecraft:vault`.
pub struct VaultBlock;

impl BlockMetadata for VaultBlock {
    fn ids() -> Box<[pumpkin_data::BlockId]> {
        [Block::VAULT.id].into()
    }
}

impl BlockBehaviour for VaultBlock {
    fn placed<'a>(&'a self, args: PlacedArgs<'a>) -> BlockFuture<'a, ()> {
        Box::pin(async move {
            let entity = Arc::new(VaultBlockEntity::new(*args.position));
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
            let stack = args.item_stack.lock().await;

            // Check if the held item is a trial key
            let is_key = stack.item == &Item::TRIAL_KEY || stack.item == &Item::OMINOUS_TRIAL_KEY;

            if !is_key {
                return BlockActionResult::Pass;
            }

            if let Some(be) = args.world.get_block_entity(args.position) {
                if let Some(vault) = be.as_any().downcast_ref::<VaultBlockEntity>() {
                    let player_uuid = args.player.gameprofile.id;

                    if vault.try_unlock(player_uuid, stack.item).await {
                        vault.mark_rewarded(player_uuid).await;
                        // Consume one key from the stack
                        drop(stack);
                        let mut stack = args.item_stack.lock().await;
                        if stack.item_count > 1 {
                            stack.item_count -= 1;
                        } else {
                            *stack = pumpkin_data::item_stack::ItemStack::EMPTY.clone();
                        }
                        return BlockActionResult::Success;
                    }
                }
            }

            BlockActionResult::Pass
        })
    }
}
