use pumpkin_data::item::Item;
pub use pumpkin_data::villager::{VillagerProfession, VillagerType};
use pumpkin_protocol::codec::var_int::VarInt;
use serde::Serialize;

pub const BREEDING_FOOD_THRESHOLD: i32 = 12;

#[must_use]
pub const fn get_food_points(item: &Item) -> i32 {
    match item.id {
        id if id == Item::BREAD.id => 4,
        id if id == Item::POTATO.id => 1,
        id if id == Item::CARROT.id => 1,
        id if id == Item::BEETROOT.id => 1,
        _ => 0,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[repr(i32)]
pub enum GossipType {
    MajorNegative = 0,
    MinorNegative = 1,
    MajorPositive = 2,
    MinorPositive = 3,
    Trading = 4,
}

impl GossipType {
    /// Maximum value this gossip type can accumulate.
    #[must_use]
    pub const fn max_value(self) -> i32 {
        200
    }

    /// Amount to decay per gossip cycle (every 20 ticks / 1 second).
    /// All gossip types decay at the same rate.
    #[must_use]
    pub const fn decay_amount(self) -> i32 {
        1
    }

    /// Whether this gossip type can be shared between villagers.
    #[must_use]
    pub const fn can_be_shared(self) -> bool {
        matches!(self, Self::MajorNegative | Self::MajorPositive)
    }

    /// Reputation value contributed per point of this gossip type.
    #[must_use]
    pub const fn reputation_weight(self) -> i32 {
        match self {
            Self::MajorNegative => -5,
            Self::MinorNegative => -1,
            Self::MajorPositive => 5,
            Self::MinorPositive => 1,
            Self::Trading => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct VillagerData {
    pub r#type: VarInt,
    pub profession: VarInt,
    pub level: VarInt,
}

impl VillagerData {
    #[must_use]
    pub const fn new(r#type: VillagerType, profession: VillagerProfession, level: i32) -> Self {
        Self {
            r#type: VarInt(r#type as i32),
            profession: VarInt(profession as i32),
            level: VarInt(level),
        }
    }

    #[must_use]
    pub fn type_enum(&self) -> VillagerType {
        VillagerType::from_i32(self.r#type.0).unwrap_or(VillagerType::Plains)
    }

    #[must_use]
    pub fn profession_enum(&self) -> VillagerProfession {
        VillagerProfession::from_i32(self.profession.0).unwrap_or(VillagerProfession::None)
    }
}
