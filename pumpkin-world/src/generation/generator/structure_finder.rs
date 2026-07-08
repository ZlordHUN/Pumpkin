use pumpkin_data::{
    dimension::Dimension,
    structures::{
        ConcentricRingsStructurePlacement, RandomSpreadStructurePlacement, Structure,
        StructureKeys, StructurePlacement, StructurePlacementType, StructureSet,
    },
    tag::{RegistryKey, get_tag_ids},
};
use pumpkin_util::{
    math::{floor_div, position::BlockPos},
    random::{RandomGenerator, RandomImpl, get_carver_seed, xoroshiro128::Xoroshiro},
};

use crate::{
    biome::{BiomeSupplier, MultiNoiseBiomeSupplier},
    generation::{
        biome_coords,
        generator::VanillaGenerator,
        noise::router::{
            multi_noise_sampler::{MultiNoiseSampler, MultiNoiseSamplerBuilderOptions},
            surface_height_sampler::{
                SurfaceHeightEstimateSampler, SurfaceHeightSamplerBuilderOptions,
            },
        },
        positions::chunk_pos,
        structure::{
            placement::{GlobalStructureCache, get_structure_chunk_in_region},
            structures::{
                StructureGeneratorContext, create_chunk_random,
                jigsaw::PoolElementKind,
                jigsaw_placement::{
                    DimensionPadding, JigsawPlacement, MaxDistance, PoolAliasLookup,
                },
            },
        },
    },
};

/// Block-level position of a found structure plus squared distance from the
/// search origin, used internally to track the running nearest candidate.
#[derive(Debug, Clone)]
pub struct FoundStructure {
    pub pos: BlockPos,
    pub distance_sq: f64,
}

/// Finds the block position of the nearest structure whose placement is listed
/// in `placements`, within `max_search_radius` chunk-region rings.
///
/// Mirrors the two-pass logic in vanilla's
/// `ChunkGenerator.findNearestMapStructure`:
///
/// 1. **Concentric-rings** placements (strongholds) are resolved in one pass
///    from the pre-computed [`GlobalStructureCache`].
/// 2. **Random-spread** placements are searched ring-by-ring outward, stopping
///    at the first radius that produces any result.
///
/// The best candidate from both passes is returned.
pub fn find_nearest_structure(
    origin: BlockPos,
    placements: &[&StructurePlacement],
    max_search_radius: i32,
    world_seed: i64,
    global_cache: &GlobalStructureCache,
) -> Option<BlockPos> {
    if placements.is_empty() {
        return None;
    }

    let mut nearest: Option<FoundStructure> = None;

    // ── Pass 1: Concentric-rings (strongholds) ──────────────────────────────
    for p in placements {
        if let StructurePlacementType::ConcentricRings(rings) = &p.placement_type
            && let Some(found) = find_nearest_concentric(origin, rings, global_cache)
            && nearest
                .as_ref()
                .is_none_or(|n| found.distance_sq < n.distance_sq)
        {
            nearest = Some(found);
        }
    }

    let random_spread: Vec<(&RandomSpreadStructurePlacement, u32)> = placements
        .iter()
        .filter_map(|p| {
            if let StructurePlacementType::RandomSpread(r) = &p.placement_type {
                Some((r, p.salt))
            } else {
                None
            }
        })
        .collect();

    if !random_spread.is_empty() {
        let chunk_origin_x = origin.0.x >> 4;
        let chunk_origin_z = origin.0.z >> 4;

        'radius: for radius in 0..=max_search_radius {
            for (placement, salt) in &random_spread {
                if let Some(found) = find_nearest_random_spread_at_radius(
                    origin,
                    chunk_origin_x,
                    chunk_origin_z,
                    radius,
                    world_seed,
                    placement,
                    *salt,
                ) {
                    if nearest
                        .as_ref()
                        .is_none_or(|n| found.distance_sq < n.distance_sq)
                    {
                        nearest = Some(found);
                    }
                    break 'radius;
                }
            }
        }
    }

    nearest.map(|f| f.pos)
}

/// Finds the nearest generated abandoned village of the requested biome variant.
///
/// Village variants share one placement set, so placement coordinates alone are
/// insufficient. This replays the weighted structure choice, biome validation,
/// and seeded town-center selection used during chunk generation.
#[must_use]
pub fn find_nearest_abandoned_village(
    origin: BlockPos,
    variant: StructureKeys,
    max_search_radius: i32,
    generator: &VanillaGenerator,
) -> Option<BlockPos> {
    if generator.dimension != Dimension::OVERWORLD
        || !StructureSet::VILLAGES
            .structures
            .iter()
            .any(|entry| entry.structure == variant)
    {
        return None;
    }

    let StructurePlacementType::RandomSpread(placement) =
        &StructureSet::VILLAGES.placement.placement_type
    else {
        return None;
    };

    let shape = &generator.settings.shape;
    let start_biome_x = biome_coords::from_block(origin.0.x);
    let start_biome_z = biome_coords::from_block(origin.0.z);
    let mut multi_noise_sampler = MultiNoiseSampler::generate(
        &generator.base_router.multi_noise,
        &MultiNoiseSamplerBuilderOptions::new(start_biome_x, start_biome_z, 1),
    );
    let mut height_sampler = SurfaceHeightEstimateSampler::generate(
        &generator.base_router.surface_estimator,
        &SurfaceHeightSamplerBuilderOptions::new(
            start_biome_x,
            start_biome_z,
            1,
            shape.min_y as i32,
            shape.height as i32,
            (shape.height / shape.vertical_cell_block_count() as u16) as usize,
        ),
    );

    let chunk_origin_x = origin.0.x >> 4;
    let chunk_origin_z = origin.0.z >> 4;
    let origin_region_x = floor_div(chunk_origin_x, placement.spacing);
    let origin_region_z = floor_div(chunk_origin_z, placement.spacing);
    let seed = generator.random_config.seed;

    for radius in 0..=max_search_radius {
        let mut nearest = None;

        for region_offset_x in -radius..=radius {
            for region_offset_z in -radius..=radius {
                if region_offset_x.abs() != radius && region_offset_z.abs() != radius {
                    continue;
                }

                let (chunk_x, chunk_z) = get_structure_chunk_in_region(
                    placement,
                    seed as i64,
                    origin_region_x + region_offset_x,
                    origin_region_z + region_offset_z,
                    StructureSet::VILLAGES.placement.salt,
                );

                let Some(village) = generated_village_at_chunk(
                    chunk_x,
                    chunk_z,
                    generator,
                    &mut height_sampler,
                    &mut multi_noise_sampler,
                ) else {
                    continue;
                };

                if village.variant != variant || !village.abandoned {
                    continue;
                }

                let distance_sq = horizontal_distance_sq(origin, village.pos);
                if nearest
                    .as_ref()
                    .is_none_or(|found: &FoundStructure| distance_sq < found.distance_sq)
                {
                    nearest = Some(FoundStructure {
                        pos: village.pos,
                        distance_sq,
                    });
                }
            }
        }

        if let Some(found) = nearest {
            return Some(found.pos);
        }
    }

    None
}

struct GeneratedVillage {
    variant: StructureKeys,
    abandoned: bool,
    pos: BlockPos,
}

fn generated_village_at_chunk(
    chunk_x: i32,
    chunk_z: i32,
    generator: &VanillaGenerator,
    height_sampler: &mut SurfaceHeightEstimateSampler<'_>,
    multi_noise_sampler: &mut MultiNoiseSampler<'_>,
) -> Option<GeneratedVillage> {
    let seed = generator.random_config.seed;
    let mut candidates = StructureSet::VILLAGES.structures.to_vec();
    let mut total_weight: u32 = candidates.iter().map(|entry| entry.weight).sum();
    let mut random = RandomGenerator::Xoroshiro(Xoroshiro::from_seed(get_carver_seed(
        seed, chunk_x, chunk_z,
    )));

    while !candidates.is_empty() {
        let mut roll = random.next_bounded_i32(total_weight as i32);
        let mut selected_idx = 0;

        for (index, entry) in candidates.iter().enumerate() {
            roll -= entry.weight as i32;
            if roll < 0 {
                selected_idx = index;
                break;
            }
        }

        let selected = &candidates[selected_idx];
        if let Some((pos, abandoned)) = try_village_start(
            selected.structure,
            chunk_x,
            chunk_z,
            generator,
            height_sampler,
            multi_noise_sampler,
        ) {
            return Some(GeneratedVillage {
                variant: selected.structure,
                abandoned,
                pos,
            });
        }

        let failed = candidates.remove(selected_idx);
        total_weight -= failed.weight;
    }

    None
}

fn try_village_start(
    variant: StructureKeys,
    chunk_x: i32,
    chunk_z: i32,
    generator: &VanillaGenerator,
    height_sampler: &mut SurfaceHeightEstimateSampler<'_>,
    multi_noise_sampler: &mut MultiNoiseSampler<'_>,
) -> Option<(BlockPos, bool)> {
    let structure = Structure::get(&variant);
    let start_y = structure
        .start_height
        .unwrap_or(generator.settings.sea_level as i16) as i32;
    let start_pos = BlockPos::new(
        chunk_pos::start_block_x(chunk_x),
        start_y,
        chunk_pos::start_block_z(chunk_z),
    );
    let max_distance = MaxDistance::new(structure.max_distance_from_center.unwrap_or(80));
    let dimension_padding = structure
        .dimension_padding
        .map_or(DimensionPadding::ZERO, |padding| DimensionPadding {
            top: padding,
            bottom: padding,
        });

    let start = {
        let mut context = StructureGeneratorContext {
            seed: generator.random_config.seed as i64,
            chunk_x,
            chunk_z,
            random: create_chunk_random(generator.random_config.seed as i64, chunk_x, chunk_z),
            sea_level: generator.settings.sea_level,
            min_y: generator.settings.shape.min_y as i32,
            height_sampler: Some(height_sampler),
            structure_key: Some(variant),
        };

        JigsawPlacement::create_start(
            &mut context,
            structure.start_pool?,
            structure.start_jigsaw_name,
            start_pos,
            structure.project_start_to_heightmap.is_some(),
            &max_distance,
            &dimension_padding,
            &PoolAliasLookup,
        )
    }?;

    let biome = MultiNoiseBiomeSupplier::OVERWORLD.biome(
        biome_coords::from_block(start.start_pos.0.x),
        biome_coords::from_block(start.start_pos.0.y),
        biome_coords::from_block(start.start_pos.0.z),
        multi_noise_sampler,
    );
    let allowed_biomes = get_tag_ids(
        RegistryKey::WorldgenBiome,
        structure
            .biomes
            .strip_prefix('#')
            .unwrap_or(structure.biomes),
    )?;

    allowed_biomes
        .contains(&(biome.id as u16))
        .then(|| (start.start_pos, is_abandoned_start(&start.element)))
}

fn is_abandoned_start(
    element: &crate::generation::structure::structures::jigsaw::PoolElement,
) -> bool {
    fn contains_zombie_template(kind: &PoolElementKind) -> bool {
        match kind {
            PoolElementKind::Single { template, .. } => template.contains("/zombie/"),
            PoolElementKind::List(elements) => elements.iter().any(contains_zombie_template),
            PoolElementKind::Empty | PoolElementKind::Feature(_) => false,
        }
    }

    contains_zombie_template(&element.kind)
}

fn horizontal_distance_sq(first: BlockPos, second: BlockPos) -> f64 {
    let delta_x = f64::from(second.0.x) - f64::from(first.0.x);
    let delta_z = f64::from(second.0.z) - f64::from(first.0.z);
    delta_x.mul_add(delta_x, delta_z * delta_z)
}

fn find_nearest_concentric(
    origin: BlockPos,
    // Kept for potential future bounds / distance validation.
    _rings: &ConcentricRingsStructurePlacement,
    global_cache: &GlobalStructureCache,
) -> Option<FoundStructure> {
    let strongholds = global_cache.get_stronghold_chunks();
    if strongholds.is_empty() {
        return None;
    }

    let ox = origin.0.x as f64;
    let oz = origin.0.z as f64;

    strongholds
        .iter()
        .map(|(cx, cz)| {
            // Centre of the chunk in block coords.
            let bx = (cx << 4) + 8;
            let bz = (cz << 4) + 8;
            let dx = bx as f64 - ox;
            let dz = bz as f64 - oz;
            FoundStructure {
                pos: BlockPos::new(bx, 0, bz),
                distance_sq: dx * dx + dz * dz,
            }
        })
        .min_by(|a, b| a.distance_sq.partial_cmp(&b.distance_sq).unwrap())
}

fn find_nearest_random_spread_at_radius(
    origin: BlockPos,
    chunk_origin_x: i32,
    chunk_origin_z: i32,
    radius: i32,
    world_seed: i64,
    placement: &RandomSpreadStructurePlacement,
    salt: u32,
) -> Option<FoundStructure> {
    let spacing = placement.spacing;
    let ox = origin.0.x as f64;
    let oz = origin.0.z as f64;

    let mut best: Option<FoundStructure> = None;

    for rx_off in -radius..=radius {
        for rz_off in -radius..=radius {
            if rx_off.abs() != radius && rz_off.abs() != radius {
                continue;
            }

            let rx = floor_div(chunk_origin_x, spacing) + rx_off;
            let rz = floor_div(chunk_origin_z, spacing) + rz_off;

            let (struct_cx, struct_cz) =
                get_structure_chunk_in_region(placement, world_seed, rx, rz, salt);

            let bx = (struct_cx << 4) + 8;
            let bz = (struct_cz << 4) + 8;
            let dx = bx as f64 - ox;
            let dz = bz as f64 - oz;
            let dist_sq = dx * dx + dz * dz;

            if best.as_ref().is_none_or(|b| dist_sq < b.distance_sq) {
                best = Some(FoundStructure {
                    pos: BlockPos::new(bx, 0, bz),
                    distance_sq: dist_sq,
                });
            }
        }
    }

    best
}

#[cfg(test)]
mod tests {
    use pumpkin_data::{dimension::Dimension, structures::StructureKeys};
    use pumpkin_util::{math::position::BlockPos, world_seed::Seed};

    use super::find_nearest_abandoned_village;
    use crate::generation::generator::{GeneratorInit, VanillaGenerator};

    #[test]
    fn finds_all_abandoned_village_variants() {
        let generator = VanillaGenerator::new(Seed(0), Dimension::OVERWORLD);
        let expected = [
            (StructureKeys::VillagePlains, BlockPos::new(-3403, 63, 3476)),
            (StructureKeys::VillageDesert, BlockPos::new(13992, 63, 8852)),
            (StructureKeys::VillageSavanna, BlockPos::new(9349, 64, 3494)),
            (
                StructureKeys::VillageSnowy,
                BlockPos::new(-2348, 96, -10571),
            ),
            (StructureKeys::VillageTaiga, BlockPos::new(4744, 88, 11802)),
        ];

        for (variant, expected_pos) in expected {
            let found = find_nearest_abandoned_village(BlockPos::ZERO, variant, 100, &generator)
                .unwrap_or_else(|| panic!("failed to locate {variant:?}"));
            assert_eq!(found, expected_pos, "incorrect location for {variant:?}");
        }
    }
}
