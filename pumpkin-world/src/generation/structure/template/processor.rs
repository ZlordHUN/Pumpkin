use pumpkin_data::{
    Block, BlockId, BlockState, Mirror, Rotation,
    tag::{self, RegistryKey},
};
use pumpkin_util::{
    HeightMap,
    math::vector3::Vector3,
    random::{RandomImpl, hash_block_pos, legacy_rand::LegacyRand},
};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    sync::{Arc, LazyLock},
};

use crate::ProtoChunk;

use super::{BlockStateResolver, PaletteEntry};

#[derive(Clone)]
pub enum StructureProcessor {
    BlockRot { integrity: f32, blocks: BlockTag },
    Gravity { heightmap: HeightMap, offset: i32 },
    Rules(Vec<ProcessorRule>),
    ProtectedBlocks(BlockTag),
}

#[derive(Clone)]
pub struct ProcessorRule {
    input_predicate: RulePredicate,
    location_predicate: RulePredicate,
    output_state: PaletteEntry,
}

#[derive(Clone)]
enum RulePredicate {
    AlwaysTrue,
    Block(BlockId),
    BlockState(PaletteEntry),
    RandomBlock { block: BlockId, probability: f32 },
    Tag(BlockTag),
}

#[derive(Clone, Copy)]
pub struct BlockTag(&'static [u16]);

impl RulePredicate {
    fn test(
        &self,
        state: &BlockState,
        random: &mut LegacyRand,
        rotation: Rotation,
        mirror: Mirror,
    ) -> bool {
        let block = state.id.to_block_id();
        match self {
            Self::AlwaysTrue => true,
            Self::Block(expected) => block == *expected,
            Self::BlockState(expected) => BlockStateResolver::resolve(expected, rotation, mirror)
                .is_some_and(|expected| expected.id == state.id),
            Self::RandomBlock {
                block: expected,
                probability,
            } => block == *expected && random.next_f32() < *probability,
            Self::Tag(tag) => tag.contains(block),
        }
    }
}

impl BlockTag {
    fn from_name(name: &str) -> Option<Self> {
        let name = name.strip_prefix('#').unwrap_or(name);
        tag::get_tag_ids(RegistryKey::Block, name).map(Self)
    }

    fn contains(self, block_id: BlockId) -> bool {
        self.0.contains(&block_id.as_u16())
    }
}

impl StructureProcessor {
    #[must_use]
    pub const fn gravity(heightmap: HeightMap, offset: i32) -> Self {
        Self::Gravity { heightmap, offset }
    }

    #[must_use]
    pub fn process(
        &self,
        chunk: &ProtoChunk,
        template_pos: Vector3<i32>,
        pos: Vector3<i32>,
        state: &'static BlockState,
        rotation: Rotation,
        mirror: Mirror,
    ) -> Option<(Vector3<i32>, &'static BlockState)> {
        let input_block = state.id.to_block_id();
        match self {
            Self::BlockRot { integrity, blocks } => {
                if !blocks.contains(input_block) {
                    return Some((pos, state));
                }
                let mut random = LegacyRand::from_seed(hash_block_pos(pos.x, pos.y, pos.z) as u64);
                (random.next_f32() <= *integrity).then_some((pos, state))
            }
            Self::Gravity { heightmap, offset } => {
                let y = chunk.get_top_y(heightmap, pos.x, pos.z) + offset + template_pos.y;
                Some((Vector3::new(pos.x, y, pos.z), state))
            }
            Self::Rules(rules) => {
                let mut random = LegacyRand::from_seed(hash_block_pos(pos.x, pos.y, pos.z) as u64);
                let location_state = BlockState::from_id(chunk.get_block_state(&pos));
                for rule in rules {
                    if rule
                        .input_predicate
                        .test(state, &mut random, rotation, mirror)
                        && rule.location_predicate.test(
                            location_state,
                            &mut random,
                            Rotation::None,
                            Mirror::None,
                        )
                    {
                        return Some((
                            pos,
                            BlockStateResolver::resolve(&rule.output_state, rotation, mirror)
                                .unwrap_or(state),
                        ));
                    }
                }
                Some((pos, state))
            }
            Self::ProtectedBlocks(blocks) => {
                let existing = chunk.get_block_state(&pos).to_block_id();
                (!blocks.contains(existing)).then_some((pos, state))
            }
        }
    }
}

#[derive(Deserialize)]
struct RawProcessorList {
    processors: Vec<RawProcessor>,
}

#[derive(Deserialize)]
#[serde(tag = "processor_type")]
enum RawProcessor {
    #[serde(rename = "minecraft:block_rot")]
    BlockRot {
        integrity: f32,
        rottable_blocks: String,
    },
    #[serde(rename = "minecraft:rule")]
    Rule { rules: Vec<RawRule> },
    #[serde(rename = "minecraft:protected_blocks")]
    ProtectedBlocks { value: String },
}

#[derive(Deserialize)]
struct RawRule {
    input_predicate: RawRulePredicate,
    location_predicate: RawRulePredicate,
    output_state: RawBlockState,
}

#[derive(Deserialize)]
#[serde(tag = "predicate_type")]
enum RawRulePredicate {
    #[serde(rename = "minecraft:always_true")]
    AlwaysTrue,
    #[serde(rename = "minecraft:block_match")]
    Block { block: String },
    #[serde(rename = "minecraft:blockstate_match")]
    BlockState { block_state: RawBlockState },
    #[serde(rename = "minecraft:random_block_match")]
    RandomBlock { block: String, probability: f32 },
    #[serde(rename = "minecraft:tag_match")]
    Tag { tag: String },
}

#[derive(Deserialize)]
struct RawBlockState {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Properties", default)]
    properties: BTreeMap<String, String>,
}

impl RawRulePredicate {
    fn into_predicate(self) -> Option<RulePredicate> {
        match self {
            Self::AlwaysTrue => Some(RulePredicate::AlwaysTrue),
            Self::Block { block } => block_id(&block).map(RulePredicate::Block),
            Self::BlockState { block_state } => {
                let state = block_state.into_palette_entry();
                BlockStateResolver::resolve_simple(&state)?;
                Some(RulePredicate::BlockState(state))
            }
            Self::RandomBlock { block, probability } => {
                block_id(&block).map(|block| RulePredicate::RandomBlock { block, probability })
            }
            Self::Tag { tag } => BlockTag::from_name(&tag).map(RulePredicate::Tag),
        }
    }
}

impl RawBlockState {
    fn into_palette_entry(self) -> PaletteEntry {
        PaletteEntry::with_properties(self.name, self.properties.into_iter().collect())
    }
}

impl ProcessorRule {
    fn from_raw(raw: RawRule) -> Option<Self> {
        let output_state = raw.output_state.into_palette_entry();
        BlockStateResolver::resolve_simple(&output_state)?;
        Some(Self {
            input_predicate: raw.input_predicate.into_predicate()?,
            location_predicate: raw.location_predicate.into_predicate()?,
            output_state,
        })
    }
}

fn block_id(name: &str) -> Option<BlockId> {
    let name = name.strip_prefix("minecraft:").unwrap_or(name);
    Block::from_name(name)
        .or_else(|| Block::from_registry_key(name))
        .map(|block| block.id)
}

#[must_use]
pub fn load_processor_list(name: &str) -> Arc<[StructureProcessor]> {
    static CACHE: LazyLock<dashmap::DashMap<String, Arc<[StructureProcessor]>>> =
        LazyLock::new(dashmap::DashMap::new);

    if let Some(processors) = CACHE.get(name) {
        return Arc::clone(&processors);
    }

    let Some(json) = super::cache::get_processor_list_json(name) else {
        tracing::warn!("Unknown structure processor list: {name}");
        return Arc::from([]);
    };
    let raw: RawProcessorList = match serde_json::from_str(json) {
        Ok(raw) => raw,
        Err(error) => {
            tracing::error!("Failed to parse structure processor list {name}: {error}");
            return Arc::from([]);
        }
    };

    let processors = raw
        .processors
        .into_iter()
        .filter_map(|processor| match processor {
            RawProcessor::BlockRot {
                integrity,
                rottable_blocks,
            } => BlockTag::from_name(&rottable_blocks)
                .map(|blocks| StructureProcessor::BlockRot { integrity, blocks }),
            RawProcessor::ProtectedBlocks { value } => {
                BlockTag::from_name(&value).map(StructureProcessor::ProtectedBlocks)
            }
            RawProcessor::Rule { rules } => Some(StructureProcessor::Rules(
                rules
                    .into_iter()
                    .filter_map(ProcessorRule::from_raw)
                    .collect(),
            )),
        })
        .collect::<Arc<[_]>>();
    CACHE.insert(name.to_owned(), Arc::clone(&processors));
    processors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ancient_city_processor_lists() {
        assert_eq!(
            load_processor_list("minecraft:ancient_city_generic_degradation").len(),
            3
        );
        assert_eq!(
            load_processor_list("minecraft:ancient_city_start_degradation").len(),
            2
        );
        assert_eq!(
            load_processor_list("minecraft:ancient_city_walls_degradation").len(),
            3
        );
    }

    #[test]
    fn parses_village_processor_lists() {
        let expected = [
            ("minecraft:mossify_10_percent", 1),
            ("minecraft:mossify_20_percent", 1),
            ("minecraft:mossify_70_percent", 1),
            ("minecraft:street_plains", 4),
            ("minecraft:street_savanna", 4),
            ("minecraft:street_snowy_or_taiga", 5),
            ("minecraft:zombie_desert", 10),
            ("minecraft:zombie_plains", 17),
            ("minecraft:zombie_savanna", 14),
            ("minecraft:zombie_snowy", 13),
            ("minecraft:zombie_taiga", 12),
        ];

        for (name, expected_rules) in expected {
            let processors = load_processor_list(name);
            let [StructureProcessor::Rules(rules)] = processors.as_ref() else {
                panic!("{name} must contain one rule processor");
            };
            assert_eq!(rules.len(), expected_rules, "{name}");
        }
    }

    #[test]
    fn preserves_village_rule_predicates_and_state_properties() {
        let processors = load_processor_list("minecraft:zombie_plains");
        let [StructureProcessor::Rules(rules)] = processors.as_ref() else {
            panic!("zombie_plains must contain one rule processor");
        };

        assert!(
            rules
                .iter()
                .any(|rule| matches!(&rule.input_predicate, RulePredicate::Tag(_)))
        );
        assert!(
            rules
                .iter()
                .any(|rule| matches!(&rule.input_predicate, RulePredicate::BlockState(_)))
        );
        assert!(
            rules
                .iter()
                .any(|rule| !rule.output_state.properties.is_empty())
        );
    }
}
