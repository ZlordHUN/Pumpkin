//! Woodland Mansion structure generator.
//!
//! Woodland mansions are large, multi-room structures found in dark forest biomes.
//! They use a grid-based layout with 1x1, 1x2, and 2x2 rooms connected by
//! corridors, surrounded by walls. Built from 73 NBT template pieces.

use std::sync::{Arc, Mutex};

use pumpkin_data::structures::Structure;
use pumpkin_util::math::{block_box::BlockBox, position::BlockPos, vector3::Vector3};

use crate::generation::structure::{
    piece::StructurePieceType,
    structures::{
        StructureGenerator, StructureGeneratorContext, StructurePiece, StructurePieceBase,
        StructurePiecesCollector, StructurePosition,
    },
    template::{StructureTemplate, cache},
};

/// Generates woodland mansions.
pub struct WoodlandMansionGenerator;

impl StructureGenerator for WoodlandMansionGenerator {
    fn get_structure_position(
        &self,
        context: StructureGeneratorContext,
    ) -> Option<StructurePosition> {
        let chunk_x = context.chunk_x;
        let chunk_z = context.chunk_z;

        let x = chunk_x * 16 + 8;
        let z = chunk_z * 16 + 8;
        let y = context.sea_level + 4;

        let start_pos = BlockPos::new(x, y, z);
        let collector = Arc::new(Mutex::new(StructurePiecesCollector::new()));

        Some(StructurePosition {
            start_pos,
            collector,
        })
    }
}

/// A room in the mansion layout grid.
struct MansionRoom {
    x: i32,
    z: i32,
    template: String,
}

/// Places the mansion by assembling template pieces in a grid.
pub struct WoodlandMansionPiece {
    pub piece: StructurePiece,
    rooms: Vec<MansionRoom>,
    grid_width: usize,
    grid_depth: usize,
}

impl WoodlandMansionPiece {
    #[must_use]
    pub fn new(origin: Vector3<i32>, seed: i64) -> Self {
        let grid_width: usize = 4;
        let grid_depth: usize = 4;

        let room_unit = 8;
        let corridor_width = 3;

        let total_width = grid_width as i32 * (room_unit + corridor_width) + corridor_width + 10;
        let total_depth = grid_depth as i32 * (room_unit + corridor_width) + corridor_width + 10;
        let height = 20;

        let bounding_box = BlockBox::new(
            origin.x,
            origin.y,
            origin.z,
            origin.x + total_width,
            origin.y + height,
            origin.z + total_depth,
        );

        let rooms = vec![
            MansionRoom {
                x: 0,
                z: 0,
                template: "woodland_mansion/2x2_a1".to_string(),
            },
            MansionRoom {
                x: 0,
                z: 1,
                template: "woodland_mansion/1x2_a1".to_string(),
            },
            MansionRoom {
                x: 0,
                z: 2,
                template: "woodland_mansion/1x2_a2".to_string(),
            },
            MansionRoom {
                x: 0,
                z: 3,
                template: "woodland_mansion/1x1_a1".to_string(),
            },
            MansionRoom {
                x: 1,
                z: 0,
                template: "woodland_mansion/1x2_a3".to_string(),
            },
            MansionRoom {
                x: 1,
                z: 1,
                template: "woodland_mansion/2x2_a2".to_string(),
            },
            MansionRoom {
                x: 1,
                z: 2,
                template: "woodland_mansion/1x2_a4".to_string(),
            },
            MansionRoom {
                x: 1,
                z: 3,
                template: "woodland_mansion/1x1_a2".to_string(),
            },
            MansionRoom {
                x: 2,
                z: 0,
                template: "woodland_mansion/1x2_a5".to_string(),
            },
            MansionRoom {
                x: 2,
                z: 1,
                template: "woodland_mansion/1x2_a6".to_string(),
            },
            MansionRoom {
                x: 2,
                z: 2,
                template: "woodland_mansion/2x2_a3".to_string(),
            },
            MansionRoom {
                x: 2,
                z: 3,
                template: "woodland_mansion/1x1_a3".to_string(),
            },
            MansionRoom {
                x: 3,
                z: 0,
                template: "woodland_mansion/1x1_a4".to_string(),
            },
            MansionRoom {
                x: 3,
                z: 1,
                template: "woodland_mansion/1x2_a7".to_string(),
            },
            MansionRoom {
                x: 3,
                z: 2,
                template: "woodland_mansion/1x2_a8".to_string(),
            },
            MansionRoom {
                x: 3,
                z: 3,
                template: "woodland_mansion/2x2_a4".to_string(),
            },
        ];

        Self {
            piece: StructurePiece::new(StructurePieceType::WoodlandMansion, bounding_box, 0),
            rooms,
            grid_width,
            grid_depth,
        }
    }
}

impl StructurePieceBase for WoodlandMansionPiece {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn get_structure_piece(&self) -> &StructurePiece {
        &self.piece
    }

    fn get_structure_piece_mut(&mut self) -> &mut StructurePiece {
        &mut self.piece
    }

    fn place(
        &mut self,
        chunk: &mut crate::ProtoChunk,
        _block_registry: &dyn crate::world::WorldPortalExt,
        _random: &mut pumpkin_util::random::RandomGenerator,
        _seed: i64,
        chunk_box: &BlockBox,
    ) {
        use crate::generation::structure::template::TemplatePiece;
        use pumpkin_data::{Mirror, Rotation};

        let room_unit = 8;
        let corridor_width = 3;
        let origin = Vector3::new(
            self.piece.bounding_box.min.x + 5,
            self.piece.bounding_box.min.y,
            self.piece.bounding_box.min.z + 5,
        );

        // Place entrance
        let entrance_x = origin.x + (self.grid_width as i32 * (room_unit + corridor_width)) / 2;
        if let Some(bytes) = cache::get_template_bytes("woodland_mansion/entrance") {
            if let Ok(template) = StructureTemplate::from_nbt_bytes(bytes) {
                let mut piece = TemplatePiece::new(
                    Arc::new(template),
                    Rotation::None,
                    Mirror::None,
                    Vector3::new(entrance_x, origin.y, origin.z),
                    StructurePieceType::WoodlandMansion,
                );
                piece.place(chunk, _block_registry, _random, _seed, chunk_box);
            }
        }

        // Place rooms
        for room in &self.rooms {
            let rx = origin.x + room.x * (room_unit + corridor_width) + corridor_width;
            let rz = origin.z + room.z * (room_unit + corridor_width) + corridor_width;

            if let Some(bytes) = cache::get_template_bytes(&room.template) {
                if let Ok(template) = StructureTemplate::from_nbt_bytes(bytes) {
                    let mut piece = TemplatePiece::new(
                        Arc::new(template),
                        Rotation::None,
                        Mirror::None,
                        Vector3::new(rx, origin.y + 1, rz),
                        StructurePieceType::WoodlandMansion,
                    );
                    piece.place(chunk, _block_registry, _random, _seed, chunk_box);
                }
            }
        }

        // Place walls around perimeter
        let wall_y = origin.y + 1;
        for gx in -1..=self.grid_width as i32 {
            for gz in -1..=self.grid_depth as i32 {
                let is_corner = (gx == -1 || gx == self.grid_width as i32)
                    && (gz == -1 || gz == self.grid_depth as i32);
                let is_edge = gx == -1
                    || gx == self.grid_width as i32
                    || gz == -1
                    || gz == self.grid_depth as i32;

                if !is_edge {
                    continue;
                }

                let wx = origin.x + gx * (room_unit + corridor_width) + corridor_width;
                let wz = origin.z + gz * (room_unit + corridor_width) + corridor_width;

                let wall_name = if is_corner {
                    "woodland_mansion/wall_corner"
                } else {
                    "woodland_mansion/wall_flat"
                };

                if let Some(bytes) = cache::get_template_bytes(wall_name) {
                    if let Ok(template) = StructureTemplate::from_nbt_bytes(bytes) {
                        let mut piece = TemplatePiece::new(
                            Arc::new(template),
                            Rotation::None,
                            Mirror::None,
                            Vector3::new(wx, wall_y, wz),
                            StructurePieceType::WoodlandMansion,
                        );
                        piece.place(chunk, _block_registry, _random, _seed, chunk_box);
                    }
                }
            }
        }
    }
}
