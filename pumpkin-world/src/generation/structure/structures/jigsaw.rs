use super::jigsaw_placement::{
    DimensionPadding, JigsawPlacement, LiquidSettings, MaxDistance, PoolAliasLookup,
};
use crate::generation::structure::structures::{
    StructureGenerator, StructureGeneratorContext, StructurePieceBase, StructurePosition,
};
use crate::generation::structure::template::{
    BlockMirror, BlockRotation, PaletteEntry, StructureProcessor, StructureTemplate,
};
use pumpkin_util::HeightMap;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::random::RandomImpl;
use serde::Deserialize;
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JigsawProjection {
    Rigid,
    TerrainMatching,
}

#[derive(Clone)]
pub struct TemplatePool {
    pub id: String,
    pub fallback: String,
    pub elements: Vec<PoolElement>,
}

#[derive(Clone)]
pub struct PoolElement {
    pub weight: u32,
    pub projection: JigsawProjection,
    pub kind: PoolElementKind,
}

#[derive(Clone)]
pub enum PoolElementKind {
    Empty,
    Single {
        template: String,
        processors: ProcessorListRef,
        legacy: bool,
    },
    List(Vec<Self>),
    Feature(pumpkin_data::placed_feature::PlacedFeature),
}

#[derive(Clone, Default)]
pub enum ProcessorListRef {
    Named(String),
    #[default]
    Empty,
}

#[derive(Deserialize)]
struct RawTemplatePool {
    fallback: String,
    elements: Vec<RawWeightedPoolElement>,
}

#[derive(Deserialize)]
struct RawWeightedPoolElement {
    element: RawPoolElement,
    weight: u32,
}

#[derive(Deserialize)]
#[serde(tag = "element_type")]
enum RawPoolElement {
    #[serde(rename = "minecraft:empty_pool_element")]
    Empty,
    #[serde(rename = "minecraft:single_pool_element")]
    Single {
        location: String,
        processors: RawProcessorList,
        projection: RawProjection,
    },
    #[serde(rename = "minecraft:legacy_single_pool_element")]
    LegacySingle {
        location: String,
        processors: RawProcessorList,
        projection: RawProjection,
    },
    #[serde(rename = "minecraft:list_pool_element")]
    List {
        elements: Vec<Self>,
        projection: RawProjection,
    },
    #[serde(rename = "minecraft:feature_pool_element")]
    Feature {
        feature: String,
        projection: RawProjection,
    },
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawProcessorList {
    Named(String),
    Inline { processors: Vec<serde_json::Value> },
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RawProjection {
    Rigid,
    TerrainMatching,
}

impl From<RawProjection> for JigsawProjection {
    fn from(value: RawProjection) -> Self {
        match value {
            RawProjection::Rigid => Self::Rigid,
            RawProjection::TerrainMatching => Self::TerrainMatching,
        }
    }
}

impl RawPoolElement {
    fn single(
        location: String,
        processors: RawProcessorList,
        projection: RawProjection,
        legacy: bool,
    ) -> (PoolElementKind, JigsawProjection) {
        let processors = match processors {
            RawProcessorList::Named(name) => ProcessorListRef::Named(name),
            RawProcessorList::Inline { processors } => {
                debug_assert!(processors.is_empty());
                ProcessorListRef::Empty
            }
        };
        (
            PoolElementKind::Single {
                template: location,
                processors,
                legacy,
            },
            projection.into(),
        )
    }

    fn into_element(self) -> Option<(PoolElementKind, JigsawProjection)> {
        match self {
            Self::Empty => Some((PoolElementKind::Empty, JigsawProjection::Rigid)),
            Self::Single {
                location,
                processors,
                projection,
            } => Some(Self::single(location, processors, projection, false)),
            Self::LegacySingle {
                location,
                processors,
                projection,
            } => Some(Self::single(location, processors, projection, true)),
            Self::List {
                elements,
                projection,
            } => {
                let projection = projection.into();
                let elements = elements
                    .into_iter()
                    .filter_map(|element| element.into_element().map(|(kind, _)| kind))
                    .collect();
                Some((PoolElementKind::List(elements), projection))
            }
            Self::Feature {
                feature,
                projection,
            } => {
                let feature = feature.strip_prefix("minecraft:").unwrap_or(&feature);
                pumpkin_data::placed_feature::PlacedFeature::from_name(feature)
                    .map(|feature| (PoolElementKind::Feature(feature), projection.into()))
            }
        }
    }
}

impl PoolElement {
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        matches!(self.kind, PoolElementKind::Empty)
    }

    #[must_use]
    pub const fn ground_level_delta(&self) -> i32 {
        1
    }

    #[must_use]
    pub fn first_template(&self) -> Option<Arc<StructureTemplate>> {
        fn find(kind: &PoolElementKind) -> Option<Arc<StructureTemplate>> {
            match kind {
                PoolElementKind::Single { template, .. } => {
                    crate::generation::structure::template::get_template(template)
                }
                PoolElementKind::List(elements) => elements.iter().find_map(find),
                PoolElementKind::Empty | PoolElementKind::Feature(_) => None,
            }
        }

        find(&self.kind)
    }

    pub fn for_each_template(
        &self,
        mut consumer: impl FnMut(&str, &ProcessorListRef, bool, Arc<StructureTemplate>),
    ) {
        fn visit(
            kind: &PoolElementKind,
            consumer: &mut impl FnMut(&str, &ProcessorListRef, bool, Arc<StructureTemplate>),
        ) {
            match kind {
                PoolElementKind::Single {
                    template,
                    processors,
                    legacy,
                } => {
                    if let Some(structure_template) =
                        crate::generation::structure::template::get_template(template)
                    {
                        consumer(template, processors, *legacy, structure_template);
                    }
                }
                PoolElementKind::List(elements) => {
                    for element in elements {
                        visit(element, consumer);
                    }
                }
                PoolElementKind::Empty | PoolElementKind::Feature(_) => {}
            }
        }

        visit(&self.kind, &mut consumer);
    }

    #[must_use]
    pub const fn feature(&self) -> Option<pumpkin_data::placed_feature::PlacedFeature> {
        match self.kind {
            PoolElementKind::Feature(feature) => Some(feature),
            _ => None,
        }
    }
}

impl TemplatePool {
    pub fn get_random_element(
        &self,
        random: &mut pumpkin_util::random::RandomGenerator,
    ) -> &PoolElement {
        let total_weight: u32 = self.elements.iter().map(|e| e.weight).sum();
        if total_weight == 0 {
            return &self.elements[0];
        }
        let mut r = random.next_bounded_i32(total_weight as i32) as u32;
        for element in &self.elements {
            if r < element.weight {
                return element;
            }
            r -= element.weight;
        }
        &self.elements[0]
    }

    /// Discovers a pool from the filesystem/embedded assets.
    #[must_use]
    pub fn discover(id: &str) -> Option<Self> {
        static CACHE: std::sync::LazyLock<dashmap::DashMap<String, TemplatePool>> =
            std::sync::LazyLock::new(dashmap::DashMap::new);

        if let Some(pool) = CACHE.get(id) {
            return Some(pool.clone());
        }

        let pool = if id == "minecraft:empty" || id == "empty" {
            Self {
                id: "minecraft:empty".to_string(),
                fallback: "minecraft:empty".to_string(),
                elements: Vec::new(),
            }
        } else if let Some(json) =
            crate::generation::structure::template::get_template_pool_json(id)
        {
            let raw: RawTemplatePool = match serde_json::from_str(json) {
                Ok(pool) => pool,
                Err(error) => {
                    tracing::error!("Failed to parse template pool {id}: {error}");
                    return None;
                }
            };
            let elements = raw
                .elements
                .into_iter()
                .filter_map(|weighted| {
                    weighted
                        .element
                        .into_element()
                        .map(|(kind, projection)| PoolElement {
                            weight: weighted.weight,
                            projection,
                            kind,
                        })
                })
                .collect();
            Self {
                id: id.to_string(),
                fallback: raw.fallback,
                elements,
            }
        } else {
            let elements = crate::generation::structure::template::get_pool_elements(id)?;
            let projection = if id.contains("streets") {
                JigsawProjection::TerrainMatching
            } else {
                JigsawProjection::Rigid
            };

            Self {
                id: id.to_string(),
                fallback: "minecraft:empty".to_string(),
                elements: elements
                    .iter()
                    .map(|e| PoolElement {
                        weight: 1,
                        projection,
                        kind: PoolElementKind::Single {
                            template: (*e).to_string(),
                            processors: ProcessorListRef::Empty,
                            legacy: false,
                        },
                    })
                    .collect(),
            }
        };
        CACHE.insert(id.to_owned(), pool.clone());
        Some(pool)
    }

    #[must_use]
    pub fn get_shuffled_elements(
        &self,
        random: &mut pumpkin_util::random::RandomGenerator,
    ) -> Vec<PoolElement> {
        let mut elements = self
            .elements
            .iter()
            .flat_map(|element| std::iter::repeat_n(element.clone(), element.weight as usize))
            .collect::<Vec<_>>();
        for index in (1..elements.len()).rev() {
            let other = random.next_bounded_i32(index as i32 + 1) as usize;
            elements.swap(index, other);
        }
        elements
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JigsawJointType {
    Rollable,
    Aligned,
}

impl JigsawJointType {
    #[allow(clippy::should_implement_trait)]
    #[must_use]
    pub fn from_str(s: &str) -> Self {
        match s {
            "aligned" => Self::Aligned,
            _ => Self::Rollable,
        }
    }
}

#[derive(Clone)]
pub struct JigsawBlock {
    pub pos: BlockPos,
    pub name: String,
    pub target: String,
    pub pool: String,
    pub final_state: String,
    pub joint: JigsawJointType,
    pub facing: pumpkin_util::BlockDirection,
    pub up: pumpkin_util::BlockDirection,
    pub selection_priority: i32,
    pub placement_priority: i32,
}

impl JigsawBlock {
    #[must_use]
    pub fn from_template_block(
        block: &crate::generation::structure::template::TemplateBlock,
        palette: &PaletteEntry,
    ) -> Option<Self> {
        if palette.name != "minecraft:jigsaw" {
            return None;
        }

        let nbt = block.nbt.as_ref()?;

        // Resolve facing from properties
        let facing_str = palette
            .properties
            .iter()
            .find(|(k, _)| *k == "orientation")
            .map_or_else(|| "north_up".to_string(), |(_, v)| v.clone());

        let mut parts = facing_str.split('_');
        let facing_part = parts.next().unwrap_or("north");
        let up_part = parts.next().unwrap_or("up");

        let facing = match facing_part {
            "south" => pumpkin_util::BlockDirection::South,
            "east" => pumpkin_util::BlockDirection::East,
            "west" => pumpkin_util::BlockDirection::West,
            "up" => pumpkin_util::BlockDirection::Up,
            "down" => pumpkin_util::BlockDirection::Down,
            _ => pumpkin_util::BlockDirection::North,
        };

        let up = match up_part {
            "north" => pumpkin_util::BlockDirection::North,
            "south" => pumpkin_util::BlockDirection::South,
            "east" => pumpkin_util::BlockDirection::East,
            "west" => pumpkin_util::BlockDirection::West,
            "down" => pumpkin_util::BlockDirection::Down,
            _ => pumpkin_util::BlockDirection::Up,
        };

        Some(Self {
            pos: BlockPos(block.pos),
            name: nbt.get_string("name").unwrap_or_default().to_string(),
            target: nbt.get_string("target").unwrap_or_default().to_string(),
            pool: nbt.get_string("pool").unwrap_or_default().to_string(),
            final_state: nbt
                .get_string("final_state")
                .unwrap_or_default()
                .to_string(),
            joint: JigsawJointType::from_str(nbt.get_string("joint").unwrap_or_default()),
            facing,
            up,
            selection_priority: nbt.get_int("selection_priority").unwrap_or(0),
            placement_priority: nbt.get_int("placement_priority").unwrap_or(0),
        })
    }

    #[must_use]
    pub fn can_attach(
        source: &Self,
        target_facing: pumpkin_util::BlockDirection,
        target_name: &str,
    ) -> bool {
        source.facing.opposite() == target_facing && source.target == target_name
    }
}

#[derive(Clone)]
pub struct JigsawJunction {
    pub source_x: i32,
    pub source_ground_y: i32,
    pub source_z: i32,
    pub delta_y: i32,
    pub projection: JigsawProjection,
}

pub struct PoolElementStructurePiece {
    pub piece: crate::generation::structure::structures::StructurePiece,
    pub element: PoolElement,
    pub pos: BlockPos,
    pub rotation: BlockRotation,
    pub mirror: BlockMirror,
    pub jigsaw_blocks: Vec<JigsawBlock>,
    pub junctions: Vec<JigsawJunction>,
    pub ground_level_delta: i32,
    pub liquid_settings: LiquidSettings,
    pub projection: JigsawProjection,
}

impl StructurePieceBase for PoolElementStructurePiece {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn get_structure_piece(&self) -> &crate::generation::structure::structures::StructurePiece {
        &self.piece
    }

    fn get_structure_piece_mut(
        &mut self,
    ) -> &mut crate::generation::structure::structures::StructurePiece {
        &mut self.piece
    }

    fn place(
        &mut self,
        chunk: &mut crate::ProtoChunk,
        _block_registry: &dyn crate::world::WorldPortalExt,
        random: &mut pumpkin_util::random::RandomGenerator,
        _seed: i64,
        chunk_box: &pumpkin_util::math::block_box::BlockBox,
    ) {
        let origin =
            pumpkin_util::math::vector3::Vector3::new(self.pos.0.x, self.pos.0.y, self.pos.0.z);

        self.element
            .for_each_template(|_name, processor_list, legacy, template| {
                let configured_processors = match processor_list {
                    ProcessorListRef::Named(name) => {
                        crate::generation::structure::template::processor::load_processor_list(name)
                    }
                    ProcessorListRef::Empty => Arc::from([]),
                };
                let mut processors = Vec::with_capacity(configured_processors.len() + 1);
                if self.projection == JigsawProjection::TerrainMatching {
                    processors.push(StructureProcessor::gravity(HeightMap::WorldSurfaceWg, -1));
                }
                processors.extend(configured_processors.iter().cloned());
                crate::generation::structure::template::place_template(
                    chunk,
                    &template,
                    origin,
                    (0, 0),
                    self.rotation,
                    legacy,
                    self.liquid_settings == LiquidSettings::ApplyWaterlog,
                    &processors,
                    Some(chunk_box),
                );
            });

        if let Some(feature) = self.element.feature()
            && let Some(placed_feature) =
                crate::generation::feature::placed_features::PLACED_FEATURES.get(&feature)
        {
            placed_feature.generate_in_proto_chunk(chunk, feature, random, self.pos);
        }
    }
}

impl PoolElementStructurePiece {
    pub fn add_junction(&mut self, junction: JigsawJunction) {
        self.junctions.push(junction);
    }
}

pub struct JigsawGenerator {
    pub start_pool: String,
    pub size: i32,
    pub start_jigsaw_name: Option<String>,
    pub use_expansion_hack: bool,
}

impl JigsawGenerator {
    #[must_use]
    pub fn new(start_pool: &str, size: i32) -> Self {
        Self {
            start_pool: start_pool.to_string(),
            size,
            start_jigsaw_name: None,
            use_expansion_hack: false,
        }
    }

    #[must_use]
    pub fn with_start_jigsaw(mut self, name: &str) -> Self {
        self.start_jigsaw_name = Some(name.to_string());
        self
    }

    #[must_use]
    pub const fn with_expansion_hack(mut self, use_hack: bool) -> Self {
        self.use_expansion_hack = use_hack;
        self
    }
}

impl StructureGenerator for JigsawGenerator {
    fn get_structure_position(
        &self,
        context: StructureGeneratorContext<'_>,
    ) -> Option<StructurePosition> {
        let mut context = context;
        let structure = context
            .structure_key
            .map(|key| pumpkin_data::structures::Structure::get(&key));

        let start_y = if let Some(s) = structure {
            s.start_height.unwrap_or(context.sea_level as i16) as i32
        } else {
            context.sea_level
        };

        let start_pos = BlockPos::new(
            crate::generation::positions::chunk_pos::start_block_x(context.chunk_x),
            start_y,
            crate::generation::positions::chunk_pos::start_block_z(context.chunk_z),
        );

        let project_start_to_heightmap = structure
            .and_then(|s| s.project_start_to_heightmap)
            .is_some();

        let max_distance = structure
            .and_then(|s| s.max_distance_from_center)
            .unwrap_or(80); // Vanilla default is 80

        let liquid_settings =
            structure
                .and_then(|s| s.liquid_settings)
                .map_or(LiquidSettings::ApplyWaterlog, |ls| match ls {
                    "ignore_waterlogging" => LiquidSettings::IgnoreWaterlogDone,
                    _ => LiquidSettings::ApplyWaterlog,
                });

        let dimension_padding =
            structure
                .and_then(|s| s.dimension_padding)
                .map_or(DimensionPadding::ZERO, |dp| DimensionPadding {
                    top: dp,
                    bottom: dp,
                });

        JigsawPlacement::add_pieces(
            &mut context,
            &self.start_pool,
            self.start_jigsaw_name.as_deref(),
            self.size,
            start_pos,
            self.use_expansion_hack,
            project_start_to_heightmap,
            &MaxDistance::new(max_distance),
            &dimension_padding,
            liquid_settings,
            &PoolAliasLookup,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pumpkin_data::structures::StructureKeys;

    const VILLAGE_VARIANTS: &[(&str, StructureKeys, usize, u32, u32)] = &[
        ("plains", StructureKeys::VillagePlains, 8, 204, 4),
        ("desert", StructureKeys::VillageDesert, 6, 250, 5),
        ("savanna", StructureKeys::VillageSavanna, 8, 459, 9),
        ("snowy", StructureKeys::VillageSnowy, 6, 306, 6),
        ("taiga", StructureKeys::VillageTaiga, 4, 100, 2),
    ];
    const ABANDONED_VILLAGE_POOLS: &[(&str, usize, u32)] = &[
        ("desert/zombie/decor", 4, 28),
        ("desert/zombie/houses", 29, 68),
        ("desert/zombie/streets", 11, 35),
        ("desert/zombie/terminators", 2, 2),
        ("desert/zombie/villagers", 2, 11),
        ("plains/zombie/decor", 5, 6),
        ("plains/zombie/houses", 36, 83),
        ("plains/zombie/streets", 16, 49),
        ("plains/zombie/villagers", 2, 11),
        ("savanna/zombie/decor", 5, 17),
        ("savanna/zombie/houses", 32, 72),
        ("savanna/zombie/streets", 19, 56),
        ("savanna/zombie/terminators", 5, 5),
        ("savanna/zombie/villagers", 2, 11),
        ("snowy/zombie/decor", 7, 22),
        ("snowy/zombie/houses", 31, 65),
        ("snowy/zombie/streets", 16, 47),
        ("snowy/zombie/villagers", 2, 11),
        ("taiga/zombie/decor", 10, 26),
        ("taiga/zombie/houses", 27, 74),
        ("taiga/zombie/streets", 16, 49),
        ("taiga/zombie/villagers", 2, 11),
    ];
    const ABANDONED_VILLAGE_DEPENDENCIES: &[(&str, usize, u32)] = &[
        ("common/animals", 10, 26),
        ("common/butcher_animals", 4, 8),
        ("common/cats", 11, 13),
        ("common/iron_golem", 1, 1),
        ("common/sheep", 2, 2),
        ("common/well_bottoms", 1, 1),
        ("plains/terminators", 4, 4),
        ("snowy/terminators", 4, 4),
        ("taiga/terminators", 4, 4),
    ];

    fn assert_pool(id: &str, element_count: usize, total_weight: u32, fallback: &str) {
        let pool = TemplatePool::discover(id).unwrap_or_else(|| panic!("missing pool {id}"));
        assert_eq!(pool.elements.len(), element_count, "{id}");
        assert_eq!(
            pool.elements
                .iter()
                .map(|element| element.weight)
                .sum::<u32>(),
            total_weight,
            "{id}"
        );
        assert_eq!(pool.fallback, fallback, "{id}");
    }

    fn is_abandoned(kind: &PoolElementKind) -> bool {
        matches!(
            kind,
            PoolElementKind::Single { template, .. } if template.contains("/zombie/")
        )
    }

    fn village_pool_id(path: &str) -> String {
        format!("minecraft:village/{path}")
    }

    fn abandoned_fallback(path: &str) -> String {
        let (biome, pool) = path.split_once("/zombie/").unwrap();
        if matches!(pool, "houses" | "streets") {
            if matches!(biome, "desert" | "savanna") {
                village_pool_id(&format!("{biome}/zombie/terminators"))
            } else {
                village_pool_id(&format!("{biome}/terminators"))
            }
        } else {
            "minecraft:empty".to_string()
        }
    }

    fn assert_legacy_pool_templates(id: &str) {
        let pool = TemplatePool::discover(id).unwrap_or_else(|| panic!("missing pool {id}"));
        for element in pool.elements {
            match element.kind {
                PoolElementKind::Single {
                    template, legacy, ..
                } => {
                    assert!(legacy, "{template} must use legacy placement");
                    assert!(
                        crate::generation::structure::template::get_template(&template).is_some(),
                        "missing template {template}"
                    );
                }
                PoolElementKind::Empty | PoolElementKind::Feature(_) => {}
                PoolElementKind::List(_) => panic!("unexpected list element in {id}"),
            }
        }
    }

    #[test]
    fn ancient_city_pools_match_vanilla_weights() {
        let expected = [
            ("minecraft:ancient_city/city_center", 3, 3),
            ("minecraft:ancient_city/sculk", 2, 7),
            ("minecraft:ancient_city/structures", 20, 46),
            ("minecraft:ancient_city/walls", 16, 27),
            ("minecraft:ancient_city/city/entrance", 6, 6),
            ("minecraft:ancient_city/city_center/walls", 10, 10),
            ("minecraft:ancient_city/walls/no_corners", 8, 8),
        ];

        for (id, element_count, total_weight) in expected {
            let pool = TemplatePool::discover(id).unwrap_or_else(|| panic!("missing pool {id}"));
            assert_eq!(pool.elements.len(), element_count, "{id}");
            assert_eq!(
                pool.elements
                    .iter()
                    .map(|element| element.weight)
                    .sum::<u32>(),
                total_weight,
                "{id}"
            );
            assert_eq!(pool.fallback, "minecraft:empty", "{id}");
        }
    }

    #[test]
    fn village_start_pools_match_vanilla_abandoned_weights() {
        for &(biome, _, element_count, total_weight, abandoned_weight) in VILLAGE_VARIANTS {
            let id = village_pool_id(&format!("{biome}/town_centers"));
            let pool = TemplatePool::discover(&id).unwrap_or_else(|| panic!("missing pool {id}"));
            assert_eq!(pool.elements.len(), element_count, "{id}");
            assert_eq!(
                pool.elements
                    .iter()
                    .map(|element| element.weight)
                    .sum::<u32>(),
                total_weight,
                "{id}"
            );

            let actual_abandoned_weight = pool
                .elements
                .iter()
                .filter(|element| is_abandoned(&element.kind))
                .map(|element| element.weight)
                .sum::<u32>();
            assert_eq!(actual_abandoned_weight, abandoned_weight, "{id}");
        }
    }

    #[test]
    fn abandoned_village_starts_build_jigsaw_graphs() {
        for &(biome, structure_key, _, _, _) in VILLAGE_VARIANTS {
            let pool_id = village_pool_id(&format!("{biome}/town_centers"));
            let pool = TemplatePool::discover(&pool_id).unwrap();
            let seed = (0i64..10_000)
                .find(|seed| {
                    let mut random = super::super::create_chunk_random(*seed, 0, 0);
                    is_abandoned(&pool.get_random_element(&mut random).kind)
                })
                .expect("no abandoned start selected");
            let structure = pumpkin_data::structures::Structure::get(&structure_key);
            let generator = JigsawGenerator::new(&pool_id, structure.size.unwrap());
            let context = StructureGeneratorContext {
                seed,
                chunk_x: 0,
                chunk_z: 0,
                random: super::super::create_chunk_random(seed, 0, 0),
                sea_level: 63,
                min_y: -64,
                height_sampler: None,
                structure_key: Some(structure_key),
            };

            let position = generator
                .get_structure_position(context)
                .unwrap_or_else(|| panic!("{pool_id} did not generate"));
            let collector = position.collector.lock().unwrap();
            let start = collector.pieces[0]
                .as_any()
                .downcast_ref::<PoolElementStructurePiece>()
                .expect("village start must be a pool element");
            assert!(is_abandoned(&start.element.kind), "{pool_id}");
            assert_eq!(
                start.ground_level_delta,
                start.element.ground_level_delta(),
                "{pool_id}"
            );

            let mut terrain_matching_pieces = 0;
            for piece in collector
                .pieces
                .iter()
                .filter_map(|piece| piece.as_any().downcast_ref::<PoolElementStructurePiece>())
            {
                if piece.projection == JigsawProjection::TerrainMatching {
                    terrain_matching_pieces += 1;
                    assert_eq!(
                        piece.ground_level_delta,
                        piece.element.ground_level_delta(),
                        "{pool_id}"
                    );
                }
            }
            assert!(terrain_matching_pieces > 0, "{pool_id} produced no streets");
            assert!(collector.pieces.len() > 1, "{pool_id} produced no branches");
        }
    }

    #[test]
    fn abandoned_village_pools_match_vanilla_weights() {
        for &(path, element_count, total_weight) in ABANDONED_VILLAGE_POOLS {
            let id = village_pool_id(path);
            assert_pool(&id, element_count, total_weight, &abandoned_fallback(path));
        }
    }

    #[test]
    fn abandoned_village_dependency_pools_match_vanilla_weights() {
        for &(path, element_count, total_weight) in ABANDONED_VILLAGE_DEPENDENCIES {
            assert_pool(
                &village_pool_id(path),
                element_count,
                total_weight,
                "minecraft:empty",
            );
        }
    }

    #[test]
    fn abandoned_village_templates_are_embedded_and_use_legacy_placement() {
        for &(biome, _, _, _, _) in VILLAGE_VARIANTS {
            assert_legacy_pool_templates(&village_pool_id(&format!("{biome}/town_centers")));
        }
        for &(path, _, _) in ABANDONED_VILLAGE_POOLS {
            assert_legacy_pool_templates(&village_pool_id(path));
        }
        for &(path, _, _) in ABANDONED_VILLAGE_DEPENDENCIES {
            assert_legacy_pool_templates(&village_pool_id(path));
        }
    }

    #[test]
    fn abandoned_village_template_states_resolve_for_every_rotation() {
        let mut pool_ids = VILLAGE_VARIANTS
            .iter()
            .map(|(biome, ..)| village_pool_id(&format!("{biome}/town_centers")))
            .collect::<Vec<_>>();
        pool_ids.extend(
            ABANDONED_VILLAGE_POOLS
                .iter()
                .map(|(path, ..)| village_pool_id(path)),
        );
        pool_ids.extend(
            ABANDONED_VILLAGE_DEPENDENCIES
                .iter()
                .map(|(path, ..)| village_pool_id(path)),
        );

        for pool_id in pool_ids {
            let pool = TemplatePool::discover(&pool_id).unwrap();
            for element in &pool.elements {
                element.for_each_template(|name, _, _, template| {
                    for entry in &template.palette {
                        for rotation in BlockRotation::values() {
                            assert!(
                                crate::generation::structure::template::BlockStateResolver::resolve(
                                    entry,
                                    rotation,
                                    BlockMirror::None,
                                )
                                .is_some(),
                                "{name} has unresolved state {} at {rotation:?}",
                                entry.name
                            );
                        }
                    }
                });
            }
        }
    }

    #[test]
    fn abandoned_village_decor_features_are_executable() {
        fn assert_features(kind: &PoolElementKind, pool_id: &str) {
            match kind {
                PoolElementKind::Feature(feature) => assert!(
                    crate::generation::feature::placed_features::PLACED_FEATURES
                        .contains_key(feature),
                    "{pool_id} uses an unsupported placed feature"
                ),
                PoolElementKind::List(elements) => {
                    for element in elements {
                        assert_features(element, pool_id);
                    }
                }
                PoolElementKind::Empty | PoolElementKind::Single { .. } => {}
            }
        }

        for &(path, _, _) in ABANDONED_VILLAGE_POOLS {
            let pool_id = village_pool_id(path);
            let pool = TemplatePool::discover(&pool_id).unwrap();
            for element in &pool.elements {
                assert_features(&element.kind, &pool_id);
            }
        }
    }

    #[test]
    fn ancient_city_start_templates_and_anchor_exist() {
        let pool = TemplatePool::discover("minecraft:ancient_city/city_center").unwrap();
        for element in pool.elements {
            let template = element.first_template().expect("missing start template");
            assert!(
                template.blocks.iter().any(|block| {
                    JigsawBlock::from_template_block(block, &template.palette[block.state as usize])
                        .is_some_and(|jigsaw| jigsaw.name == "minecraft:city_anchor")
                }),
                "start template has no city_anchor"
            );
        }
    }

    #[test]
    fn ancient_city_pool_templates_are_embedded() {
        fn check(kind: &PoolElementKind) {
            match kind {
                PoolElementKind::Single { template, .. } => {
                    // This entry exists in vanilla's pool data but has no corresponding
                    // template in the vanilla server jar.
                    if template == "minecraft:ancient_city/walls/intact_horizontal_wall_stairs_5" {
                        assert!(
                            crate::generation::structure::template::get_template(template)
                                .is_none()
                        );
                    } else {
                        assert!(
                            crate::generation::structure::template::get_template(template)
                                .is_some(),
                            "missing template {template}"
                        );
                    }
                }
                PoolElementKind::List(elements) => elements.iter().for_each(check),
                PoolElementKind::Empty | PoolElementKind::Feature(_) => {}
            }
        }

        for id in [
            "minecraft:ancient_city/city_center",
            "minecraft:ancient_city/structures",
            "minecraft:ancient_city/walls",
            "minecraft:ancient_city/city/entrance",
            "minecraft:ancient_city/city_center/walls",
            "minecraft:ancient_city/walls/no_corners",
        ] {
            for element in TemplatePool::discover(id).unwrap().elements {
                check(&element.kind);
            }
        }
    }

    #[test]
    fn ancient_city_builds_a_multi_piece_graph() {
        let generator = JigsawGenerator::new("minecraft:ancient_city/city_center", 7)
            .with_start_jigsaw("minecraft:city_anchor");
        let context = StructureGeneratorContext {
            seed: 0,
            chunk_x: 0,
            chunk_z: 0,
            random: super::super::create_chunk_random(0, 0, 0),
            sea_level: 63,
            min_y: -64,
            height_sampler: None,
            structure_key: Some(pumpkin_data::structures::StructureKeys::AncientCity),
        };

        let position = generator
            .get_structure_position(context)
            .expect("ancient city graph should generate");
        let collector = position.collector.lock().unwrap();
        assert!(
            collector.pieces.len() > 10,
            "ancient city generated only {} pieces",
            collector.pieces.len()
        );
        assert_eq!(position.start_pos.0.y, -27);
    }
}
