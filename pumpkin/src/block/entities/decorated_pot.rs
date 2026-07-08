//! Decorated Pot block entity. Stores 4 pottery sherd slots (front, back,
//! left, right) and an optional contained item. Supports the decorated pot
//! block placed in the world.

use crate::block::entities::BlockEntity;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_nbt::tag::NbtTag;
use pumpkin_util::math::position::BlockPos;
use std::any::Any;
use std::pin::Pin;

/// Stores the 4 sherd IDs that decorate the pot's faces.
/// Order in vanilla: back, left, right, front (matching facing direction).
#[derive(Clone, Default)]
pub struct PotDecorations {
    pub back: Option<String>,
    pub left: Option<String>,
    pub right: Option<String>,
    pub front: Option<String>,
}

pub struct DecoratedPotBlockEntity {
    pub position: BlockPos,
    /// The 4 sherd decorations applied to this pot.
    pub decorations: tokio::sync::Mutex<PotDecorations>,
    /// An optional item stored in the pot (dropped when broken).
    pub stored_item: tokio::sync::Mutex<Option<ItemStack>>,
}

impl DecoratedPotBlockEntity {
    pub const ID: &'static str = "minecraft:decorated_pot";

    #[must_use]
    pub fn new(position: BlockPos) -> Self {
        Self {
            position,
            decorations: tokio::sync::Mutex::new(PotDecorations {
                back: None,
                left: None,
                right: None,
                front: None,
            }),
            stored_item: tokio::sync::Mutex::new(None),
        }
    }
}

impl BlockEntity for DecoratedPotBlockEntity {
    fn write_nbt<'a>(
        &'a self,
        nbt: &'a mut NbtCompound,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            let decorations = self.decorations.lock().await;

            // Write pot_decorations as a list of sherd IDs
            let mut sherds: Vec<String> = Vec::with_capacity(4);
            for sherd in [
                decorations.back.as_ref(),
                decorations.left.as_ref(),
                decorations.right.as_ref(),
                decorations.front.as_ref(),
            ] {
                sherds.push(
                    sherd
                        .cloned()
                        .unwrap_or_else(|| "minecraft:brick".to_string()),
                );
            }
            // Store as a string list under "sherds" key
            nbt.put(
                "sherds",
                NbtTag::List(
                    sherds
                        .into_iter()
                        .map(|s| NbtTag::String(s.into_boxed_str()))
                        .collect(),
                ),
            );

            // Write stored item if present
            if let Some(ref item) = *self.stored_item.lock().await {
                let mut item_nbt = NbtCompound::new();
                item_nbt.put_string("id", item.item.registry_key.to_string());
                item_nbt.put_byte("Count", item.item_count as i8);
                nbt.put("item", NbtTag::Compound(item_nbt));
            }
        })
    }

    fn from_nbt(nbt: &NbtCompound, position: BlockPos) -> Self
    where
        Self: Sized,
    {
        let decorations = if let Some(sherd_list) = nbt.get_list("sherds") {
            let mut deco = PotDecorations::default();
            for (i, tag) in sherd_list.iter().enumerate() {
                if let Some(s) = tag.extract_string() {
                    match i {
                        0 => deco.back = Some(s.to_string()),
                        1 => deco.left = Some(s.to_string()),
                        2 => deco.right = Some(s.to_string()),
                        3 => deco.front = Some(s.to_string()),
                        _ => {}
                    }
                }
            }
            deco
        } else {
            PotDecorations::default()
        };

        Self {
            position,
            decorations: tokio::sync::Mutex::new(decorations),
            stored_item: tokio::sync::Mutex::new(None),
        }
    }

    fn resource_location(&self) -> &'static str {
        Self::ID
    }

    fn get_position(&self) -> BlockPos {
        self.position
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
