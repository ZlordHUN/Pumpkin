//! Trial Spawner block behavior. Handles creation/removal of the
//! TrialSpawnerBlockEntity on placement/break.

use std::sync::Arc;

use pumpkin_data::Block;

use crate::block::entities::trial_spawner::TrialSpawnerBlockEntity;
use crate::block::{BlockBehaviour, BlockFuture, BlockMetadata, PlacedArgs};

/// Handles `minecraft:trial_spawner`.
pub struct TrialSpawnerBlock;

impl BlockMetadata for TrialSpawnerBlock {
    fn ids() -> Box<[pumpkin_data::BlockId]> {
        [Block::TRIAL_SPAWNER.id].into()
    }
}

impl BlockBehaviour for TrialSpawnerBlock {
    fn placed<'a>(&'a self, args: PlacedArgs<'a>) -> BlockFuture<'a, ()> {
        Box::pin(async move {
            let entity = Arc::new(TrialSpawnerBlockEntity::new(*args.position));
            args.world.add_block_entity(entity);
        })
    }

    fn broken<'a>(&'a self, args: crate::block::BrokenArgs<'a>) -> BlockFuture<'a, ()> {
        Box::pin(async move {
            args.world.remove_block_entity(args.position);
        })
    }
}
