//! NBT Structure Template System
//!
//! This module provides functionality for loading and placing Minecraft structure
//! templates from `.nbt` files. This enables exact vanilla structure matching and
//! dramatically simplifies implementing structures like igloos, shipwrecks, villages, etc.
//!
//! # Architecture
//!
//! - [`StructureTemplate`]: Represents a loaded NBT template with size, palette, and blocks
//! - [`TemplatePiece`]: A structure piece that places blocks from a template
//! - [`Rotation`] and [`Mirror`]: Transform positions and block properties
//! - [`TemplateCache`]: Lazy-loading cache for embedded template files
//!
//! # Example Usage
//!
//! ```ignore
//! use pumpkin_world::generation::structure::template::{TemplateCache, TemplatePiece};
//! use pumpkin_data::Rotation;
//!
//! // Load a template from the cache
//! let template = TemplateCache::get("igloo/top").expect("Template not found");
//!
//! // Create a piece to place the template
//! let piece = TemplatePiece::new(template, rotation, mirror, position);
//! ```

mod block_state_resolver;
mod cache;
pub mod processor;
mod structure_template;
mod template_piece;

use pumpkin_data::Mirror;
use pumpkin_data::Rotation;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::math::vector3::Vector3;
use pumpkin_util::random::{RandomImpl, hash_block_pos, legacy_rand::LegacyRand};

use crate::ProtoChunk;

pub use block_state_resolver::BlockStateResolver;
pub use cache::{
    TemplateCache, get_pool_elements, get_processor_list_json, get_template,
    get_template_pool_json, global_cache,
};
pub use processor::StructureProcessor;
pub use pumpkin_data::{Mirror as BlockMirror, Rotation as BlockRotation};
pub use structure_template::{PaletteEntry, StructureTemplate, TemplateBlock, TemplateEntity};
pub use template_piece::TemplatePiece;

struct ProcessedBlock {
    pos: Vector3<i32>,
    state: &'static pumpkin_data::BlockState,
    block_entity_nbt: Option<NbtCompound>,
    block_name: String,
}

/// Places a template at a world origin with an un-rotated XZ offset.
///
/// All rotation is handled internally:
/// - The offset is rotated to position the template correctly
/// - Block positions within the template are rotated
/// - Directional block properties (facing, axis, etc.) are rotated
/// - Block entities are created from template NBT data
///
/// `origin` is the base world position (x, y, z).
/// `offset` is the un-rotated XZ offset from origin (`x_offset`, `z_offset`) - rotation is applied automatically.
#[allow(clippy::too_many_arguments)]
pub fn place_template(
    chunk: &mut ProtoChunk,
    template: &StructureTemplate,
    origin: Vector3<i32>,
    offset: (i32, i32),
    rotation: Rotation,
    skip_air: bool,
    apply_waterlogging: bool,
    processors: &[StructureProcessor],
    chunk_box: Option<&pumpkin_util::math::block_box::BlockBox>,
) {
    let (rotated_ox, rotated_oz) = rotation.rotate_offset(offset.0, offset.1);
    let world_x = origin.x + rotated_ox;
    let world_z = origin.z + rotated_oz;
    let mut processed_blocks = Vec::with_capacity(template.blocks.len());

    for block in &template.blocks {
        let palette_entry = &template.palette[block.state as usize];

        // Structure blocks are data markers and structure void preserves the existing block.
        if palette_entry.name == "minecraft:structure_void"
            || palette_entry.name == "minecraft:structure_block"
        {
            continue;
        }

        // Skip air blocks when using IGNORE_AIR processor (e.g. nether fossils)
        if skip_air && palette_entry.name == "minecraft:air" {
            continue;
        }

        let mut block_entity_nbt = block.nbt.clone();
        let mut placed_entry = palette_entry.clone();

        // Jigsaw blocks are replaced during template processing, before block entities are
        // collected. Keeping this in the placement pipeline avoids stale jigsaw entities.
        if palette_entry.name == "minecraft:jigsaw" {
            let final_state = block_entity_nbt
                .as_ref()
                .and_then(|nbt| nbt.get_string("final_state"))
                .unwrap_or("minecraft:air");
            placed_entry = PaletteEntry::from_string(final_state);
            block_entity_nbt = None;
        }

        // Resolve block state with rotation applied to directional properties
        let Some(mut state) =
            BlockStateResolver::resolve(&placed_entry, rotation, Mirror::default())
        else {
            continue;
        };

        // Rotate block position within template bounds
        let local_pos = rotation.transform_pos(block.pos, template.size);

        let mut world_pos = Vector3::new(
            world_x + local_pos.x,
            origin.y + local_pos.y,
            world_z + local_pos.z,
        );

        // Apply processors
        let mut should_place = true;
        for processor in processors {
            let Some((processed_pos, processed_state)) = processor.process(
                chunk,
                block.pos,
                world_pos,
                state,
                rotation,
                Mirror::default(),
            ) else {
                should_place = false;
                break;
            };
            world_pos = processed_pos;
            state = processed_state;
        }
        if !should_place {
            continue;
        }

        if let Some(bbox) = chunk_box
            && (world_pos.x < bbox.min.x
                || world_pos.x > bbox.max.x
                || world_pos.y < bbox.min.y
                || world_pos.y > bbox.max.y
                || world_pos.z < bbox.min.z
                || world_pos.z > bbox.max.z)
        {
            continue;
        }

        processed_blocks.push(ProcessedBlock {
            pos: world_pos,
            state,
            block_entity_nbt,
            block_name: placed_entry.name,
        });
    }

    for processed in processed_blocks {
        place_processed_block(chunk, processed, apply_waterlogging);
    }
}

fn place_processed_block(
    chunk: &mut ProtoChunk,
    processed: ProcessedBlock,
    apply_waterlogging: bool,
) {
    let ProcessedBlock {
        pos,
        mut state,
        block_entity_nbt,
        block_name,
    } = processed;

    if apply_waterlogging
        && chunk.get_block_state(&pos).to_block_id() == pumpkin_data::Block::WATER.id
        && let Some(waterlogged_state) = state.with_waterlogged()
    {
        state = waterlogged_state;
    }

    chunk.set_block_state(pos.x, pos.y, pos.z, state);

    let block_entity_id = get_block_entity_id(&block_name);
    if block_entity_nbt.is_none() && block_entity_id.is_none() {
        return;
    }

    let mut placed_nbt = NbtCompound::new();
    placed_nbt.put_string("id", block_entity_id.unwrap_or(&block_name).to_string());
    placed_nbt.put_int("x", pos.x);
    placed_nbt.put_int("y", pos.y);
    placed_nbt.put_int("z", pos.z);

    if let Some(template_nbt) = block_entity_nbt {
        for (key, value) in template_nbt.child_tags {
            if key.as_ref() != "x"
                && key.as_ref() != "y"
                && key.as_ref() != "z"
                && key.as_ref() != "id"
            {
                placed_nbt.child_tags.insert(key, value);
            }
        }
    }

    if placed_nbt.get_string("LootTable").is_some()
        && placed_nbt.get_long("LootTableSeed").is_none()
    {
        let mut random = LegacyRand::from_seed(hash_block_pos(pos.x, pos.y, pos.z) as u64);
        placed_nbt.put_long("LootTableSeed", random.next_i64());
    }

    chunk.add_block_entity(placed_nbt);
}

/// Returns the block entity ID for blocks that require one, or None if not needed.
fn get_block_entity_id(block_name: &str) -> Option<&'static str> {
    match block_name {
        "minecraft:furnace" => Some("minecraft:furnace"),
        "minecraft:chest" => Some("minecraft:chest"),
        "minecraft:trapped_chest" => Some("minecraft:trapped_chest"),
        "minecraft:barrel" => Some("minecraft:barrel"),
        "minecraft:hopper" => Some("minecraft:hopper"),
        "minecraft:dropper" => Some("minecraft:dropper"),
        "minecraft:dispenser" => Some("minecraft:dispenser"),
        "minecraft:brewing_stand" => Some("minecraft:brewing_stand"),
        "minecraft:blast_furnace" => Some("minecraft:blast_furnace"),
        "minecraft:smoker" => Some("minecraft:smoker"),
        "minecraft:shulker_box" => Some("minecraft:shulker_box"),
        "minecraft:bed" => Some("minecraft:bed"),
        "minecraft:sign"
        | "minecraft:oak_sign"
        | "minecraft:spruce_sign"
        | "minecraft:birch_sign"
        | "minecraft:jungle_sign"
        | "minecraft:acacia_sign"
        | "minecraft:dark_oak_sign"
        | "minecraft:mangrove_sign"
        | "minecraft:cherry_sign"
        | "minecraft:bamboo_sign"
        | "minecraft:crimson_sign"
        | "minecraft:warped_sign" => Some("minecraft:sign"),
        "minecraft:hanging_sign" => Some("minecraft:hanging_sign"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pumpkin_data::{Block, BlockState, dimension::Dimension};
    use pumpkin_util::{HeightMap, world_seed::Seed};

    fn empty_chunk() -> ProtoChunk {
        let generator = crate::generation::get_world_gen(Seed(0), Dimension::OVERWORLD);
        ProtoChunk::new(0, 0, &generator)
    }

    fn single_block_template(state: PaletteEntry) -> StructureTemplate {
        StructureTemplate {
            size: Vector3::new(1, 1, 1),
            palette: vec![state],
            blocks: vec![TemplateBlock {
                pos: Vector3::new(0, 0, 0),
                state: 0,
                nbt: None,
            }],
            entities: Vec::new(),
        }
    }

    #[test]
    fn terrain_matching_street_replaces_water_with_planks() {
        let mut chunk = empty_chunk();
        chunk.set_block_state(4, 62, 4, Block::WATER.default_state);
        let template = single_block_template(PaletteEntry::new("minecraft:dirt_path".to_string()));
        let street_processors = processor::load_processor_list("minecraft:street_plains");
        let mut processors = vec![StructureProcessor::gravity(HeightMap::WorldSurfaceWg, -1)];
        processors.extend(street_processors.iter().cloned());

        place_template(
            &mut chunk,
            &template,
            Vector3::new(4, 100, 4),
            (0, 0),
            Rotation::None,
            false,
            true,
            &processors,
            None,
        );

        assert_eq!(
            chunk.get_block_state(&Vector3::new(4, 62, 4)).to_block_id(),
            Block::OAK_PLANKS.id
        );
        assert_eq!(
            chunk
                .get_block_state(&Vector3::new(4, 100, 4))
                .to_block_id(),
            Block::AIR.id
        );
    }

    #[test]
    fn gravity_uses_the_pre_placement_heightmap_for_the_whole_template() {
        let mut chunk = empty_chunk();
        chunk.set_block_state(4, 62, 4, Block::WATER.default_state);
        let template = StructureTemplate {
            size: Vector3::new(1, 3, 1),
            palette: vec![PaletteEntry::new("minecraft:stone".to_string())],
            blocks: vec![
                TemplateBlock {
                    pos: Vector3::new(0, 1, 0),
                    state: 0,
                    nbt: None,
                },
                TemplateBlock {
                    pos: Vector3::new(0, 2, 0),
                    state: 0,
                    nbt: None,
                },
            ],
            entities: Vec::new(),
        };
        let processors = [StructureProcessor::gravity(HeightMap::WorldSurfaceWg, -1)];

        place_template(
            &mut chunk,
            &template,
            Vector3::new(4, 100, 4),
            (0, 0),
            Rotation::None,
            false,
            true,
            &processors,
            None,
        );

        assert_eq!(
            chunk.get_block_state(&Vector3::new(4, 63, 4)).to_block_id(),
            Block::STONE.id
        );
        assert_eq!(
            chunk.get_block_state(&Vector3::new(4, 64, 4)).to_block_id(),
            Block::STONE.id
        );
        assert_eq!(
            chunk.get_block_state(&Vector3::new(4, 65, 4)).to_block_id(),
            Block::AIR.id
        );
    }

    #[test]
    fn village_rules_run_before_waterlogging() {
        let mut chunk = empty_chunk();
        chunk.set_block_state(4, 62, 4, Block::WATER.default_state);
        let template = single_block_template(PaletteEntry::with_properties(
            "minecraft:glass_pane".to_string(),
            vec![
                ("east".to_string(), "false".to_string()),
                ("north".to_string(), "true".to_string()),
                ("south".to_string(), "true".to_string()),
                ("waterlogged".to_string(), "false".to_string()),
                ("west".to_string(), "false".to_string()),
            ],
        ));
        let processors = processor::load_processor_list("minecraft:zombie_plains");

        place_template(
            &mut chunk,
            &template,
            Vector3::new(4, 62, 4),
            (0, 0),
            Rotation::None,
            false,
            true,
            &processors,
            None,
        );

        let state = BlockState::from_id(chunk.get_block_state(&Vector3::new(4, 62, 4)));
        assert_eq!(state.id.to_block_id(), Block::BROWN_STAINED_GLASS_PANE.id);
        assert!(state.is_waterlogged());
    }
}
