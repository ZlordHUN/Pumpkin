use std::borrow::Cow;

use pumpkin_data::{
    structures::StructureKeys,
    translation::java::{
        CHAT_COORDINATES_TOOLTIP, COMMANDS_LOCATE_STRUCTURE_NOT_FOUND,
        COMMANDS_LOCATE_STRUCTURE_SUCCESS,
    },
};
use pumpkin_util::{
    PermissionLvl,
    math::position::BlockPos,
    permission::{Permission, PermissionDefault, PermissionRegistry},
    text::{TextComponent, click::ClickEvent, color::NamedColor, hover::HoverEvent},
};
use pumpkin_world::generation::generator::structure_finder::find_nearest_abandoned_village;

use crate::command::{
    argument_builder::{ArgumentBuilder, command, literal},
    context::command_context::CommandContext,
    errors::error_types::CommandErrorType,
    node::{CommandExecutor, CommandExecutorResult, dispatcher::CommandDispatcher},
};

const DESCRIPTION: &str = "Locates an abandoned village.";
const PERMISSION: &str = "minecraft:command.locate";
const MAX_SEARCH_RADIUS: i32 = 100;

const NOT_FOUND: CommandErrorType<1> = CommandErrorType::new(
    COMMANDS_LOCATE_STRUCTURE_NOT_FOUND,
    COMMANDS_LOCATE_STRUCTURE_NOT_FOUND,
);

struct LocateAbandonedVillageExecutor {
    variant: StructureKeys,
    name: &'static str,
}

impl CommandExecutor for LocateAbandonedVillageExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let origin = BlockPos(context.source.position.floor_to_i32());
            let generator = context.world().level.world_gen.clone();
            let variant = self.variant;
            let (sender, receiver) = tokio::sync::oneshot::channel();

            rayon::spawn(move || {
                let found =
                    find_nearest_abandoned_village(origin, variant, MAX_SEARCH_RADIUS, &generator);
                let _ = sender.send(found);
            });

            let Some(found) = receiver.await.expect("locate worker dropped") else {
                return Err(NOT_FOUND.create_without_context(TextComponent::text(self.name)));
            };

            let distance = horizontal_distance(origin, found);
            context
                .source
                .send_feedback(success_message(self.name, found, distance), false)
                .await;

            Ok(distance)
        })
    }
}

fn success_message(name: &'static str, pos: BlockPos, distance: i32) -> TextComponent {
    let coordinates = TextComponent::text(format!("{}, ~, {}", pos.0.x, pos.0.z))
        .wrap_in_square_brackets()
        .color_named(NamedColor::Green)
        .hover_event(HoverEvent::show_text(TextComponent::translate_cross(
            CHAT_COORDINATES_TOOLTIP,
            CHAT_COORDINATES_TOOLTIP,
            [],
        )))
        .click_event(ClickEvent::RunCommand {
            command: Cow::Owned(format!("/tp @s {} ~ {}", pos.0.x, pos.0.z)),
        });

    TextComponent::translate_cross(
        COMMANDS_LOCATE_STRUCTURE_SUCCESS,
        COMMANDS_LOCATE_STRUCTURE_SUCCESS,
        [
            TextComponent::text(name),
            coordinates,
            TextComponent::text(distance.to_string()),
        ],
    )
}

fn horizontal_distance(origin: BlockPos, found: BlockPos) -> i32 {
    let delta_x = f64::from(found.0.x) - f64::from(origin.0.x);
    let delta_z = f64::from(found.0.z) - f64::from(origin.0.z);
    delta_x.hypot(delta_z).floor() as i32
}

pub fn register(dispatcher: &mut CommandDispatcher, registry: &mut PermissionRegistry) {
    registry.register_permission_or_panic(Permission::new(
        PERMISSION,
        DESCRIPTION,
        PermissionDefault::Op(PermissionLvl::Two),
    ));

    let structure = literal("structure")
        .then(
            literal("abandoned_village_plains").executes(LocateAbandonedVillageExecutor {
                variant: StructureKeys::VillagePlains,
                name: "minecraft:abandoned_village_plains",
            }),
        )
        .then(
            literal("abandoned_village_desert").executes(LocateAbandonedVillageExecutor {
                variant: StructureKeys::VillageDesert,
                name: "minecraft:abandoned_village_desert",
            }),
        )
        .then(
            literal("abandoned_village_savanna").executes(LocateAbandonedVillageExecutor {
                variant: StructureKeys::VillageSavanna,
                name: "minecraft:abandoned_village_savanna",
            }),
        )
        .then(
            literal("abandoned_village_snowy").executes(LocateAbandonedVillageExecutor {
                variant: StructureKeys::VillageSnowy,
                name: "minecraft:abandoned_village_snowy",
            }),
        )
        .then(
            literal("abandoned_village_taiga").executes(LocateAbandonedVillageExecutor {
                variant: StructureKeys::VillageTaiga,
                name: "minecraft:abandoned_village_taiga",
            }),
        );

    dispatcher.register(
        command("locate", DESCRIPTION)
            .requires(PERMISSION)
            .then(structure),
    );
}
