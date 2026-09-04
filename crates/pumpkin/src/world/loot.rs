use pumpkin_data::BlockState;
use pumpkin_data::damage::DamageType;
use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_util::loot_table::{LootBonusFormula, LootCondition, LootEntry, LootTable};
use pumpkin_util::random::{RandomImpl, xoroshiro128::Xoroshiro};

#[derive(Default, Clone)]
pub struct LootContextParameters {
    pub explosion_radius: Option<f32>,
    pub block_state: Option<&'static BlockState>,
    pub killed_by_player: Option<bool>,
    pub luck: f32,
    pub this_entity: Option<&'static EntityType>,
    pub killer_entity: Option<&'static EntityType>,
    pub direct_killer_entity: Option<&'static EntityType>,
    pub position: Option<pumpkin_util::math::vector3::Vector3<f64>>,
    pub world_time: u64,
    pub damage_type: Option<DamageType>,
    pub tool: Option<ItemStack>,
    pub is_raining: Option<bool>,
    pub is_thundering: Option<bool>,
    /// Whether the killed entity was on fire at death time.
    /// Computed from `Entity.fire_ticks > 0`.
    pub is_on_fire: Option<bool>,
}

fn check_condition(
    cond: LootCondition,
    has_silk_touch: bool,
    has_shears: bool,
    fortune_level: i32,
    params: &LootContextParameters,
    rng: &mut Xoroshiro,
) -> bool {
    match cond {
        LootCondition::None => true,
        LootCondition::SilkTouch => has_silk_touch,
        LootCondition::NoSilkTouch => !has_silk_touch,
        LootCondition::Shears => has_shears,
        LootCondition::SilkTouchOrShears => has_silk_touch || has_shears,
        LootCondition::NoSilkTouchOrShears => !has_silk_touch && !has_shears,
        LootCondition::KilledByPlayer => params.killed_by_player.unwrap_or(false),
        LootCondition::SurvivesExplosion => params
            .explosion_radius
            .is_none_or(|radius| rng.next_f32() <= 1.0 / radius),
        LootCondition::RandomChance { chance } => rng.next_f32() < chance,
        LootCondition::RandomChanceWithEnchantedBonus {
            unenchanted_chance,
            enchanted_chance_base,
            enchanted_chance_per_level_above_first,
        } => {
            let chance = if fortune_level > 0 {
                enchanted_chance_base
                    + enchanted_chance_per_level_above_first * (fortune_level - 1) as f32
            } else {
                unenchanted_chance
            };
            rng.next_f32() < chance
        }
        LootCondition::TableBonus {
            enchantment,
            chances,
        } => {
            let level = params.tool.as_ref().map_or(0, |tool| {
                pumpkin_data::Enchantment::from_name(enchantment)
                    .map_or(0, |enchantment| tool.get_enchantment_level(enchantment))
            });
            let index = (level.max(0) as usize).min(chances.len().saturating_sub(1));
            chances
                .get(index)
                .is_some_and(|chance| rng.next_f32() < *chance)
        }
        LootCondition::AllOf(conditions) => conditions
            .iter()
            .all(|c| check_condition(*c, has_silk_touch, has_shears, fortune_level, params, rng)),
    }
}

fn apply_bonus_formula(
    base_count: i32,
    bonus: LootBonusFormula,
    fortune_level: i32,
    rng: &mut Xoroshiro,
) -> i32 {
    match bonus {
        LootBonusFormula::OreDrops => {
            if fortune_level > 0 {
                let bonus = (rng.next_bounded_i32(fortune_level + 2) - 1).max(0);
                base_count * (bonus + 1)
            } else {
                base_count
            }
        }
        LootBonusFormula::UniformBonusCount(bonus_multiplier) => {
            let max_bonus = fortune_level * bonus_multiplier;
            let extra = if max_bonus > 0 {
                rng.next_bounded_i32(max_bonus + 1)
            } else {
                0
            };
            base_count + extra
        }
        LootBonusFormula::BinomialWithBonusCount { extra, probability } => {
            let n = fortune_level + extra;
            let mut bonus_count = 0;
            for _ in 0..n {
                if rng.next_f32() < probability {
                    bonus_count += 1;
                }
            }
            base_count + bonus_count
        }
    }
}

fn apply_explosion_decay(count: i32, radius: Option<f32>, rng: &mut Xoroshiro) -> i32 {
    let Some(radius) = radius else {
        return count;
    };
    (0..count)
        .filter(|_| rng.next_f32() <= 1.0 / radius)
        .count() as i32
}

#[must_use]
pub fn generate_loot(table: &LootTable, seed: i64) -> Vec<ItemStack> {
    generate_loot_with_context(table, seed, &LootContextParameters::default())
}

#[must_use]
pub fn generate_loot_with_context(
    table: &LootTable,
    seed: i64,
    params: &LootContextParameters,
) -> Vec<ItemStack> {
    let mut rng = Xoroshiro::from_seed(seed as u64);
    let mut items_to_place: Vec<ItemStack> = Vec::new();

    let has_silk_touch = params.tool.as_ref().is_some_and(|tool| {
        pumpkin_data::Enchantment::from_name("silk_touch")
            .is_some_and(|e| tool.get_enchantment_level(e) > 0)
    });

    let has_shears = params.tool.as_ref().is_some_and(|tool| {
        let name = tool
            .item
            .registry_key
            .strip_prefix("minecraft:")
            .unwrap_or(tool.item.registry_key);
        name == "shears"
    });

    let fortune_level = params.tool.as_ref().map_or(0, |tool| {
        let fortune = pumpkin_data::Enchantment::from_name("fortune")
            .map_or(0, |e| tool.get_enchantment_level(e));
        let looting = pumpkin_data::Enchantment::from_name("looting")
            .map_or(0, |e| tool.get_enchantment_level(e));
        fortune.max(looting)
    });

    for pool in table.pools {
        if !check_condition(
            pool.condition,
            has_silk_touch,
            has_shears,
            fortune_level,
            params,
            &mut rng,
        ) {
            continue;
        }

        let eligible_entries: Vec<&LootEntry> = pool
            .entries
            .iter()
            .filter(|e| {
                check_condition(
                    e.condition,
                    has_silk_touch,
                    has_shears,
                    fortune_level,
                    params,
                    &mut rng,
                )
            })
            .collect();

        if eligible_entries.is_empty() && pool.empty_weight == 0 {
            continue;
        }

        let range = pool.max_rolls - pool.min_rolls;
        let rolls = pool.min_rolls
            + if range > 0 {
                rng.next_bounded_i32(range + 1)
            } else {
                0
            };

        for _ in 0..rolls {
            let entry_weight: i32 = eligible_entries.iter().map(|e| e.weight).sum();
            let total_weight = entry_weight + pool.empty_weight;
            if total_weight == 0 {
                continue;
            }

            let mut pick = rng.next_bounded_i32(total_weight);

            pick -= pool.empty_weight;
            if pick < 0 {
                continue;
            }

            for entry in &eligible_entries {
                pick -= entry.weight;
                if pick < 0 {
                    let count_range = entry.max_count - entry.min_count;
                    let base_count = entry.min_count
                        + if count_range > 0 {
                            rng.next_bounded_i32(count_range + 1)
                        } else {
                            0
                        };

                    let mut final_count = base_count;
                    if let Some(bonus) = entry.bonus_formula {
                        final_count =
                            apply_bonus_formula(final_count, bonus, fortune_level, &mut rng);
                    }
                    if entry.explosion_decay {
                        final_count =
                            apply_explosion_decay(final_count, params.explosion_radius, &mut rng);
                    }

                    if final_count > 0 {
                        let item_key = entry.item.strip_prefix("minecraft:").unwrap_or(entry.item);

                        if let Some(item) = Item::from_registry_key(item_key) {
                            items_to_place.push(ItemStack::new(final_count as u8, item));
                        }
                    }
                    break;
                }
            }
        }
    }

    items_to_place
}

pub use generate_loot as generate_chest_loot;

pub fn fill_chest_inventory(
    inventory: &std::sync::Arc<dyn pumpkin_world::inventory::Inventory>,
    table: &LootTable,
    seed: i64,
) {
    let mut items_to_place = generate_loot(table, seed);

    if items_to_place.is_empty() {
        return;
    }

    let inv_size = inventory.size();
    let mut rng = Xoroshiro::from_seed(seed as u64);

    let mut available_slots: Vec<usize> = (0..inv_size)
        .filter(|&slot| inventory.get_stack(slot).is_empty())
        .collect();

    for i in (1..available_slots.len()).rev() {
        let j = rng.next_bounded_i32((i + 1) as i32) as usize;
        available_slots.swap(i, j);
    }

    shuffle_and_split_items(&mut items_to_place, available_slots.len(), &mut rng);

    for item in items_to_place {
        let Some(slot) = available_slots.pop() else {
            tracing::warn!("Tried to over-fill a container");
            return;
        };
        inventory.set_stack(slot, item);
    }
}

fn shuffle_and_split_items(
    result: &mut Vec<ItemStack>,
    available_slots: usize,
    rng: &mut Xoroshiro,
) {
    let mut splittable: Vec<ItemStack> = Vec::new();
    let mut i = 0;
    while i < result.len() {
        if result[i].is_empty() {
            result.swap_remove(i);
        } else if result[i].item_count > 1 {
            splittable.push(result.swap_remove(i));
        } else {
            i += 1;
        }
    }

    while available_slots > result.len() + splittable.len() && !splittable.is_empty() {
        let idx = rng.next_bounded_i32(splittable.len() as i32) as usize;
        let mut stack = splittable.swap_remove(idx);

        let count = stack.item_count as i32;
        let split_off = 1 + rng.next_bounded_i32(count / 2);
        stack.item_count = (count - split_off) as u8;
        let mut copy = stack.clone();
        copy.item_count = split_off as u8;

        if stack.item_count > 1 && rng.next_bool() {
            splittable.push(stack);
        } else {
            result.push(stack);
        }
        if copy.item_count > 1 && rng.next_bool() {
            splittable.push(copy);
        } else {
            result.push(copy);
        }
    }

    result.extend(splittable);

    let n = result.len();
    for i in (1..n).rev() {
        let j = rng.next_bounded_i32((i + 1) as i32) as usize;
        result.swap(i, j);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pumpkin_data::{Enchantment, loot_table};

    const LEAF_TABLES: [(&LootTable, &str, Option<&str>, bool); 11] = [
        (
            &loot_table::BLOCKS_OAK_LEAVES,
            "oak_leaves",
            Some("oak_sapling"),
            true,
        ),
        (
            &loot_table::BLOCKS_DARK_OAK_LEAVES,
            "dark_oak_leaves",
            Some("dark_oak_sapling"),
            true,
        ),
        (
            &loot_table::BLOCKS_PALE_OAK_LEAVES,
            "pale_oak_leaves",
            Some("pale_oak_sapling"),
            false,
        ),
        (
            &loot_table::BLOCKS_SPRUCE_LEAVES,
            "spruce_leaves",
            Some("spruce_sapling"),
            false,
        ),
        (
            &loot_table::BLOCKS_BIRCH_LEAVES,
            "birch_leaves",
            Some("birch_sapling"),
            false,
        ),
        (
            &loot_table::BLOCKS_JUNGLE_LEAVES,
            "jungle_leaves",
            Some("jungle_sapling"),
            false,
        ),
        (
            &loot_table::BLOCKS_ACACIA_LEAVES,
            "acacia_leaves",
            Some("acacia_sapling"),
            false,
        ),
        (
            &loot_table::BLOCKS_CHERRY_LEAVES,
            "cherry_leaves",
            Some("cherry_sapling"),
            false,
        ),
        (
            &loot_table::BLOCKS_AZALEA_LEAVES,
            "azalea_leaves",
            Some("azalea"),
            false,
        ),
        (
            &loot_table::BLOCKS_FLOWERING_AZALEA_LEAVES,
            "flowering_azalea_leaves",
            Some("flowering_azalea"),
            false,
        ),
        (
            &loot_table::BLOCKS_MANGROVE_LEAVES,
            "mangrove_leaves",
            None,
            false,
        ),
    ];

    fn enchanted_tool(enchantment: &'static Enchantment, level: u16) -> ItemStack {
        let mut tool = ItemStack::new(1, &Item::DIAMOND_HOE);
        tool.add_enchantment(enchantment, level);
        tool
    }

    fn assert_drop_rate(observed: usize, samples: usize, chance: f64, label: &str) {
        let expected = samples as f64 * chance;
        let tolerance = 6.0 * (expected * (1.0 - chance)).sqrt() + 1.0;
        assert!(
            (observed as f64 - expected).abs() <= tolerance,
            "{label}: got {observed}/{samples}, expected a chance of {chance}"
        );
    }

    #[test]
    fn leaf_tables_drop_only_leaves_with_shears_or_silk_touch() {
        let mut silk_touch = enchanted_tool(&Enchantment::SILK_TOUCH, 1);
        silk_touch.add_enchantment(&Enchantment::FORTUNE, 4);
        for tool in [ItemStack::new(1, &Item::SHEARS), silk_touch] {
            let params = LootContextParameters {
                tool: Some(tool),
                ..Default::default()
            };
            for (table, leaf, _, _) in LEAF_TABLES {
                let expected_item = Item::from_registry_key(leaf).unwrap();
                for seed in 0..128 {
                    let drops = generate_loot_with_context(table, seed, &params);
                    assert_eq!(drops.len(), 1, "{leaf}, seed {seed}");
                    assert_eq!(drops[0].item.id, expected_item.id, "{leaf}, seed {seed}");
                    assert_eq!(drops[0].item_count, 1);
                }
            }
        }
    }

    #[test]
    fn leaf_drop_rates_match_vanilla_without_a_tool_and_with_fortune() {
        // These probabilities are from the bundled vanilla 26.2 leaf loot tables.
        const SAMPLES: usize = 20_000;
        const SAPLING_CHANCES: [f64; 5] = [0.05, 0.0625, 0.083333336, 0.1, 0.1];
        const JUNGLE_CHANCES: [f64; 5] = [0.025, 0.027777778, 0.03125, 0.041666668, 0.1];
        const STICK_CHANCES: [f64; 5] = [0.02, 0.022222223, 0.025, 0.033333335, 0.1];
        const APPLE_CHANCES: [f64; 5] = [0.005, 0.0055555557, 0.00625, 0.008333334, 0.025];

        for (table, leaf, sapling, has_apples) in LEAF_TABLES {
            for (level, default_sapling_chance) in SAPLING_CHANCES.into_iter().enumerate() {
                let params = LootContextParameters {
                    tool: (level > 0).then(|| enchanted_tool(&Enchantment::FORTUNE, level as u16)),
                    ..Default::default()
                };
                let mut saplings = 0;
                let mut sticks = [0; 2];
                let mut apples = 0;
                for seed in 0..SAMPLES as i64 {
                    for drop in generate_loot_with_context(table, seed, &params) {
                        let name = drop
                            .item
                            .registry_key
                            .strip_prefix("minecraft:")
                            .unwrap_or(drop.item.registry_key);
                        match name {
                            "stick" => {
                                assert!((1..=2).contains(&drop.item_count));
                                sticks[drop.item_count as usize - 1] += 1;
                            }
                            "apple" => {
                                assert!(has_apples, "{leaf} must not drop apples");
                                assert_eq!(drop.item_count, 1);
                                apples += 1;
                            }
                            _ => {
                                assert_eq!(Some(name), sapling, "unexpected {leaf} drop");
                                assert_eq!(drop.item_count, 1);
                                saplings += 1;
                            }
                        }
                    }
                }
                let sapling_chance = if sapling.is_none() {
                    0.0
                } else if leaf == "jungle_leaves" {
                    JUNGLE_CHANCES[level]
                } else {
                    default_sapling_chance
                };
                let label = format!("{leaf}, Fortune {level}");
                assert_drop_rate(saplings, SAMPLES, sapling_chance, &label);
                // Vanilla chooses one or two sticks with equal probability.
                for count in sticks {
                    assert_drop_rate(count, SAMPLES, STICK_CHANCES[level] / 2.0, &label);
                }
                assert_drop_rate(
                    apples,
                    SAMPLES,
                    if has_apples {
                        APPLE_CHANCES[level]
                    } else {
                        0.0
                    },
                    &label,
                );
            }
        }
    }

    #[test]
    fn leaf_drop_chances_ignore_looting() {
        let params = LootContextParameters {
            tool: Some(enchanted_tool(&Enchantment::LOOTING, 3)),
            ..Default::default()
        };
        for (table, leaf, _, _) in LEAF_TABLES {
            for seed in 0..256 {
                let without_tool = generate_loot(table, seed);
                let with_looting = generate_loot_with_context(table, seed, &params);
                let contents = |drops: Vec<ItemStack>| {
                    drops
                        .into_iter()
                        .map(|drop| (drop.item.id, drop.item_count))
                        .collect::<Vec<_>>()
                };
                assert_eq!(
                    contents(without_tool),
                    contents(with_looting),
                    "{leaf}, seed {seed}"
                );
            }
        }
    }

    #[test]
    fn table_bonus_uses_the_requested_enchantment_and_clamps_its_level() {
        let condition = LootCondition::TableBonus {
            enchantment: "minecraft:fortune",
            chances: &[0.0, 0.0, 1.0],
        };
        for (tool, expected) in [
            (None, false),
            (Some(enchanted_tool(&Enchantment::FORTUNE, 1)), false),
            (Some(enchanted_tool(&Enchantment::FORTUNE, 2)), true),
            (Some(enchanted_tool(&Enchantment::FORTUNE, 100)), true),
            (Some(enchanted_tool(&Enchantment::LOOTING, 3)), false),
        ] {
            let params = LootContextParameters {
                tool,
                ..Default::default()
            };
            assert_eq!(
                check_condition(
                    condition,
                    false,
                    false,
                    100,
                    &params,
                    &mut Xoroshiro::from_seed(0)
                ),
                expected
            );
        }
    }

    #[test]
    fn explosions_remove_individual_sticks_from_leaf_drops() {
        const SAMPLES: usize = 20_000;
        let params = LootContextParameters {
            tool: Some(enchanted_tool(&Enchantment::FORTUNE, 4)),
            explosion_radius: Some(2.0),
            ..Default::default()
        };
        let mut sticks = [0; 2];
        for seed in 0..SAMPLES as i64 {
            for drop in
                generate_loot_with_context(&loot_table::BLOCKS_MANGROVE_LEAVES, seed, &params)
            {
                assert_eq!(drop.item.id, Item::STICK.id);
                assert!((1..=2).contains(&drop.item_count));
                sticks[drop.item_count as usize - 1] += 1;
            }
        }
        // A 10% chance of 1–2 sticks, each surviving independently with probability 1/2.
        assert_drop_rate(sticks[0], SAMPLES, 0.05, "one surviving stick");
        assert_drop_rate(sticks[1], SAMPLES, 0.0125, "two surviving sticks");
    }

    #[test]
    fn leaf_saplings_and_apples_must_survive_explosions() {
        let params = LootContextParameters {
            explosion_radius: Some(f32::INFINITY),
            ..Default::default()
        };
        for (table, leaf, _, _) in LEAF_TABLES {
            for seed in 0..256 {
                assert!(
                    generate_loot_with_context(table, seed, &params).is_empty(),
                    "{leaf}, seed {seed}"
                );
            }
        }
    }
}
