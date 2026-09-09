use super::*;
use tokio::sync::{mpsc, watch};

const TARGET: &str = "@a[nbt={UUID:[I;0,0,0,1]},limit=1]";

fn collect_painted_rects(shape: &egui::Shape, rects: &mut Vec<egui::Rect>) {
    // Popup frames group the blurred shadow and actual frame in Shape::Vec.
    // Measure widget backgrounds, not the decorative shadow around them.
    match shape {
        egui::Shape::Rect(rect) if rect.blur_width == 0.0 => rects.push(rect.rect),
        egui::Shape::Vec(shapes) => {
            for shape in shapes {
                collect_painted_rects(shape, rects);
            }
        }
        _ => {}
    }
}

struct PlayerFrame {
    row: egui::Response,
    rows: Vec<(uuid::Uuid, egui::Response)>,
    texts: Vec<(String, egui::Rect)>,
    painted_rects: Vec<egui::Rect>,
    selection_rects: Vec<egui::Rect>,
    status_dots: Vec<(egui::Pos2, f32)>,
}

impl PlayerFrame {
    fn text_rect(&self, label: &str) -> egui::Rect {
        self.texts
            .iter()
            .find(|(text, _)| text == label)
            .unwrap_or_else(|| panic!("Missing {label:?} in {:?}", self.texts))
            .1
    }

    fn text_center(&self, label: &str) -> egui::Pos2 {
        self.text_rect(label).center()
    }

    fn assert_selection(&self, id: uuid::Uuid) {
        let row = &self
            .rows
            .iter()
            .find(|(row_id, _)| *row_id == id)
            .unwrap()
            .1;
        assert_eq!(self.selection_rects.len(), 1);
        let selected = self.selection_rects[0];
        assert!(row.rect.expand(0.5).contains_rect(selected));
        assert!((selected.width() - row.rect.width()).abs() <= 1.0);
        assert!((selected.height() - row.rect.height()).abs() <= 1.0);
    }

    fn assert_row_geometry(&self, name: &str, edition: &str) {
        let row = self.row.rect;
        let name = self.text_rect(name);
        let edition = self.text_rect(edition);
        let (center, radius) = self
            .status_dots
            .iter()
            .find(|(center, _)| row.contains(*center))
            .unwrap();
        assert!((row.width() - 270.0).abs() <= 0.5, "{row:?}");
        assert!((row.height() - 28.0).abs() <= 0.5, "{row:?}");
        assert!((radius - 3.0).abs() <= 0.5);
        assert!((center.x - row.left() - 11.0).abs() <= 0.5);
        assert!((name.left() - row.left() - 22.0).abs() <= 1.0, "{name:?}");
        assert!(
            (row.right() - edition.right() - 8.0).abs() <= 1.0,
            "{edition:?}"
        );
        assert!(
            name.right() + 7.0 <= edition.left(),
            "{name:?} overlaps {edition:?}"
        );
        for y in [center.y, name.center().y, edition.center().y] {
            assert!(
                (y - row.center().y).abs() <= 1.0,
                "{y} is not centered in {row:?}"
            );
        }
    }
}

struct PlayerUi {
    context: egui::Context,
    controls: PlayerControls,
    handle: GuiHandle,
    commands: mpsc::Receiver<String>,
    _desired: watch::Receiver<bool>,
    player: GuiPlayer,
    rows: Vec<GuiPlayer>,
    submitted: Vec<String>,
    errors: Vec<String>,
    frame_number: u32,
}

impl PlayerUi {
    fn new() -> Self {
        let context = egui::Context::default();
        super::super::configure_style(&context);
        let (handle, desired) = GuiHandle::new_managed();
        let commands = handle.take_command_receiver().unwrap();
        let player = GuiPlayer {
            id: uuid::Uuid::from_u128(1),
            name: "Alex".to_owned(),
            edition: "Java".to_owned(),
            is_op: false,
        };
        let ui = Self {
            context,
            controls: PlayerControls::default(),
            handle,
            commands,
            _desired: desired,
            rows: vec![player.clone()],
            player,
            submitted: Vec::new(),
            errors: Vec::new(),
            frame_number: 0,
        };
        ui.update_server(ServerStatus::Running, true);
        ui
    }

    fn update_server(&self, status: ServerStatus, commands_enabled: bool) {
        let mut snapshot = self.handle.snapshot();
        snapshot.status = status;
        snapshot.commands_enabled = commands_enabled;
        snapshot.players.clone_from(&self.rows);
        self.handle.apply_backend_update(snapshot, Vec::new());
    }

    fn set_operator(&mut self, is_op: bool) {
        self.player.is_op = is_op;
        self.rows
            .iter_mut()
            .find(|player| player.id == self.player.id)
            .unwrap()
            .is_op = is_op;
        self.update_server(ServerStatus::Running, true);
    }

    fn frame(&mut self, events: Vec<egui::Event>) -> PlayerFrame {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 550.0),
            )),
            time: Some(f64::from(self.frame_number) / 60.0),
            focused: true,
            events,
            ..Default::default()
        };
        self.frame_number += 1;
        let snapshot = self.handle.snapshot();
        let enabled = snapshot.status == ServerStatus::Running
            && snapshot.commands_enabled
            && !self.handle.close_requested();
        let mut rows = Vec::new();
        let mut output = self.context.run_ui(input, |ui| {
            rows.clear();
            ui.set_width(270.0);
            for player in &self.rows {
                let response = self.controls.show_player(ui, player, enabled);
                rows.push((player.id, response));
            }
            if let Some(result) = self.controls.process_actions(ui.ctx(), &self.handle) {
                match result {
                    Ok(command) => self.submitted.push(command),
                    Err(error) => self.errors.push(error),
                }
            }
        });
        output.textures_delta.clear();
        let texts = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some((
                    text.galley.text().to_owned(),
                    text.galley
                        .rect
                        .translate(text.pos.to_vec2())
                        .intersect(shape.clip_rect),
                )),
                _ => None,
            })
            .collect();
        let mut painted_rects = Vec::new();
        for shape in &output.shapes {
            collect_painted_rects(&shape.shape, &mut painted_rects);
        }
        let selection_fill = self
            .context
            .style_of(egui::Theme::Dark)
            .visuals
            .selection
            .bg_fill;
        let selection_rects = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Rect(rect) if rect.fill == selection_fill => Some(rect.rect),
                _ => None,
            })
            .collect();
        let status_dots = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Circle(circle) if circle.fill == super::super::GREEN => {
                    Some((circle.center, circle.radius))
                }
                _ => None,
            })
            .collect();
        PlayerFrame {
            row: rows
                .iter()
                .find(|(id, _)| *id == self.player.id)
                .unwrap()
                .1
                .clone(),
            rows,
            texts,
            painted_rects,
            selection_rects,
            status_dots,
        }
    }

    fn settle(&mut self) -> PlayerFrame {
        // Menus/modals need an initial sizing pass before their hit rectangles
        // settle. Keep egui's normal multi-pass behavior enabled throughout.
        self.frame(Vec::new());
        self.frame(Vec::new())
    }

    fn click(&mut self, position: egui::Pos2, button: egui::PointerButton) {
        self.frame(vec![
            egui::Event::PointerMoved(position),
            egui::Event::PointerButton {
                pos: position,
                button,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos: position,
                button,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ]);
    }

    fn click_text(&mut self, frame: &PlayerFrame, label: &str) {
        self.click(frame.text_center(label), egui::PointerButton::Primary);
    }

    fn open_menu(&mut self) -> PlayerFrame {
        let frame = self.settle();
        // Click the actual name, not empty row space: selectable labels must
        // not consume the row's secondary click.
        self.click(
            frame.text_center(&self.player.name),
            egui::PointerButton::Secondary,
        );
        let menu = self.settle();
        assert!(menu.row.context_menu_opened());
        menu
    }

    fn select_action(&mut self, action: PlayerAction) -> PlayerFrame {
        let mut menu = self.open_menu();
        let submenu = match action {
            PlayerAction::Survival
            | PlayerAction::Creative
            | PlayerAction::Adventure
            | PlayerAction::Spectator => Some("Game mode"),
            PlayerAction::WhitelistAdd | PlayerAction::WhitelistRemove => Some("Whitelist"),
            _ => None,
        };
        if let Some(submenu) = submenu {
            self.click_text(&menu, submenu);
            menu = self.settle();
        }
        let label = if action.accepts_reason() {
            format!("{}...", action.label())
        } else {
            action.label().to_owned()
        };
        self.click_text(&menu, &label);
        self.settle()
    }

    fn open_confirmation(&mut self, action: PlayerAction) -> PlayerFrame {
        assert!(action.accepts_reason());
        self.select_action(action);
        let dialog = self.settle();
        let pending = self.controls.pending.as_ref().expect("confirmation opened");
        assert_eq!(pending.action, action);
        assert_eq!(pending.player.id, self.player.id);
        assert!(dialog.texts.iter().any(|(text, _)| text == "Alex (Java)"));
        assert!(
            dialog
                .texts
                .iter()
                .any(|(text, _)| text == "Reason (optional)")
        );
        self.assert_no_submission();
        dialog
    }

    fn assert_no_submission(&mut self) {
        assert!(self.submitted.is_empty());
        assert_eq!(
            self.commands.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        );
    }
}

#[test]
fn non_reason_actions_queue_exactly_once_without_confirmation() {
    for (action, prefix) in [
        (PlayerAction::Op, "op"),
        (PlayerAction::Deop, "deop"),
        (PlayerAction::Survival, "gamemode survival"),
        (PlayerAction::Creative, "gamemode creative"),
        (PlayerAction::Adventure, "gamemode adventure"),
        (PlayerAction::Spectator, "gamemode spectator"),
        (PlayerAction::WhitelistAdd, "whitelist add"),
        (PlayerAction::WhitelistRemove, "whitelist remove"),
        (PlayerAction::Kill, "kill"),
        (PlayerAction::ClearInventory, "clear"),
    ] {
        let mut ui = PlayerUi::new();
        if action == PlayerAction::Deop {
            ui.set_operator(true);
        }
        let frame = ui.select_action(action);
        assert!(
            !frame
                .texts
                .iter()
                .any(|(text, _)| matches!(text.as_str(), "Confirm" | "Cancel"))
        );
        let expected = format!("{prefix} {TARGET}");
        assert_eq!(ui.submitted, vec![expected.clone()]);
        assert_eq!(ui.commands.try_recv().unwrap(), expected);
        assert_eq!(
            ui.commands.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        );
        assert!(ui.controls.pending.is_none());
        assert!(ui.errors.is_empty());
    }
}

#[test]
fn kick_and_ban_confirm_with_or_without_an_optional_reason() {
    for action in [PlayerAction::Kick, PlayerAction::Ban] {
        for reason in ["", "  Please follow the rules  "] {
            let mut ui = PlayerUi::new();
            ui.open_confirmation(action);
            // This is the string bound to the visible optional-reason editor.
            ui.controls.pending.as_mut().unwrap().reason = reason.to_owned();
            let dialog = ui.settle();
            if !reason.is_empty() {
                assert!(dialog.texts.iter().any(|(text, _)| text == reason));
            }
            ui.assert_no_submission();
            ui.click_text(&dialog, "Confirm");
            ui.settle();
            let prefix = if action == PlayerAction::Kick {
                "kick"
            } else {
                "ban"
            };
            let expected = format!("{prefix} {TARGET} {}", reason.trim())
                .trim_end()
                .to_owned();
            assert_eq!(ui.submitted, vec![expected.clone()]);
            assert_eq!(ui.commands.try_recv().unwrap(), expected);
            assert_eq!(
                ui.commands.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            );
            assert!(ui.controls.pending.is_none());
            assert!(ui.errors.is_empty());
        }
    }
}

#[test]
fn kick_and_ban_dialogs_have_roomy_controls_and_fit_the_minimum_window() {
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 550.0));
    for action in [PlayerAction::Kick, PlayerAction::Ban] {
        let mut ui = PlayerUi::new();
        let frame = ui.open_confirmation(action);
        // The smallest painted rectangle containing a button's text is its
        // actual background, not the enclosing modal or full-screen backdrop.
        let button = |label| {
            *frame
                .painted_rects
                .iter()
                .filter(|rect| rect.contains(frame.text_center(label)))
                .min_by(|a, b| a.area().total_cmp(&b.area()))
                .expect("painted dialog button")
        };
        let cancel = button("Cancel");
        let confirm = button("Confirm");
        for rect in [cancel, confirm] {
            assert!(rect.width() >= 109.5 && rect.height() >= 37.5, "{rect:?}");
        }
        let reason_label = frame.text_rect("Reason (optional)");
        let reason = *frame
            .painted_rects
            .iter()
            .find(|rect| {
                rect.width() > 300.0
                    && rect.height() > 20.0
                    && rect.top() >= reason_label.bottom()
                    && rect.bottom() <= cancel.top()
            })
            .expect("painted reason field below its label");
        assert!((reason.width() - 480.0).abs() <= 2.0, "{reason:?}");
        assert!(reason.height() >= 37.5, "{reason:?}");
        let heading = frame.text_rect(action.label());
        let modal = *frame
            .painted_rects
            .iter()
            .filter(|rect| {
                rect.contains_rect(heading)
                    && rect.contains_rect(reason)
                    && rect.contains_rect(cancel)
                    && rect.contains_rect(confirm)
            })
            .min_by(|a, b| a.area().total_cmp(&b.area()))
            .expect("painted modal frame");
        assert!(screen.contains_rect(modal), "{modal:?} exceeds {screen:?}");
        assert!((520.0..=524.0).contains(&modal.width()), "{modal:?}");
        // Frame strokes can add one point outside the 20-point content margin.
        for padding in [
            reason.left() - modal.left(),
            modal.right() - reason.right(),
            heading.top() - modal.top(),
            modal.bottom() - confirm.bottom(),
        ] {
            assert!(
                (19.5..=22.0).contains(&padding),
                "unexpected dialog padding: {padding}"
            );
        }
        ui.assert_no_submission();
    }
}

#[test]
fn cancel_and_escape_dismiss_confirmation_without_queueing() {
    for escape in [false, true] {
        let mut ui = PlayerUi::new();
        let dialog = ui.open_confirmation(PlayerAction::Kick);
        if escape {
            ui.frame(vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }]);
        } else {
            ui.click_text(&dialog, "Cancel");
        }
        ui.settle();
        assert!(ui.controls.pending.is_none());
        ui.assert_no_submission();
    }
}

#[test]
fn disabled_player_actions_cannot_open_a_confirmation() {
    for status in [
        ServerStatus::Starting,
        ServerStatus::Running,
        ServerStatus::Stopping,
        ServerStatus::Stopped,
        ServerStatus::Failed,
    ] {
        let mut ui = PlayerUi::new();
        match status {
            ServerStatus::Stopped => ui.handle.finish(None),
            ServerStatus::Failed => ui.handle.finish(Some("startup failed".to_owned())),
            _ => ui.update_server(status, false),
        }
        // Even a displayed row from an older snapshot must not admit actions.
        let menu = ui.open_menu();
        ui.click_text(&menu, "Op");
        ui.settle();
        assert!(ui.controls.pending.is_none());
        assert!(ui.errors.is_empty());
        ui.assert_no_submission();
    }
}

#[test]
fn disabling_console_rechecks_an_already_open_confirmation() {
    let mut ui = PlayerUi::new();
    ui.open_confirmation(PlayerAction::Kick);
    ui.update_server(ServerStatus::Running, false);
    let dialog = ui.settle();
    ui.click_text(&dialog, "Confirm");
    ui.settle();
    assert!(ui.controls.pending.is_some());
    ui.assert_no_submission();
}

#[test]
fn stopping_invalidates_confirmation_and_restart_cannot_revive_it() {
    let mut ui = PlayerUi::new();
    ui.open_confirmation(PlayerAction::Ban);
    ui.handle.request_stop();
    ui.settle();
    assert!(ui.controls.pending.is_none());
    assert!(ui.controls.selected.is_none());
    ui.handle.finish(None);
    ui.handle.request_start().unwrap();
    ui.update_server(ServerStatus::Running, true);
    ui.settle();
    assert!(ui.controls.pending.is_none());
    assert!(ui.controls.selected.is_none());
    ui.assert_no_submission();
}

#[test]
fn another_player_with_the_same_name_cannot_inherit_confirmation() {
    let mut ui = PlayerUi::new();
    ui.open_confirmation(PlayerAction::Ban);
    let mut snapshot = ui.handle.snapshot();
    snapshot.players[0].id = uuid::Uuid::from_u128(2);
    ui.handle.apply_backend_update(snapshot, Vec::new());
    ui.settle();
    assert!(ui.controls.pending.is_none());
    ui.update_server(ServerStatus::Running, true);
    ui.settle();
    assert!(ui.controls.pending.is_none());
    ui.assert_no_submission();
}

#[test]
fn full_command_queue_preserves_confirmation_and_allows_retry() {
    let mut ui = PlayerUi::new();
    let dialog = ui.open_confirmation(PlayerAction::Kick);
    for _ in 0..ui.commands.max_capacity() {
        ui.handle.submit_command("list").unwrap();
    }
    ui.click_text(&dialog, "Confirm");
    let dialog = ui.settle();
    let pending = ui
        .controls
        .pending
        .as_ref()
        .expect("failed action remains open");
    assert!(pending.error.as_deref().unwrap().contains("queue is full"));
    assert!(ui.submitted.is_empty());
    for _ in 0..ui.commands.max_capacity() {
        assert_eq!(ui.commands.try_recv().unwrap(), "list");
    }
    ui.click_text(&dialog, "Confirm");
    ui.settle();
    let expected = format!("kick {TARGET}");
    assert_eq!(ui.submitted, vec![expected.clone()]);
    assert_eq!(ui.commands.try_recv().unwrap(), expected);
    assert_eq!(
        ui.commands.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    );
    assert!(ui.controls.pending.is_none());
}

#[test]
fn a_closed_command_queue_reports_failure_without_dismissing_confirmation() {
    let mut ui = PlayerUi::new();
    let dialog = ui.open_confirmation(PlayerAction::Ban);
    ui.commands.close();
    ui.click_text(&dialog, "Confirm");
    ui.settle();
    let pending = ui
        .controls
        .pending
        .as_ref()
        .expect("failed action remains open");
    assert!(
        pending
            .error
            .as_deref()
            .unwrap()
            .contains("no longer accepting")
    );
    assert!(ui.submitted.is_empty());
    assert!(ui.commands.try_recv().is_err());
}

#[test]
fn reordering_players_keeps_the_context_menu_attached_to_its_uuid() {
    let mut ui = PlayerUi::new();
    let original_menu = ui.open_menu();
    ui.rows.insert(
        0,
        GuiPlayer {
            id: uuid::Uuid::from_u128(2),
            name: "Aaron".to_owned(),
            edition: "Bedrock".to_owned(),
            is_op: false,
        },
    );
    ui.update_server(ServerStatus::Running, true);
    let menu = ui.settle();
    assert_eq!(menu.row.id, original_menu.row.id);
    assert!(menu.row.context_menu_opened());
    assert_eq!(ui.controls.selected, Some(ui.player.id));
    menu.assert_selection(ui.player.id);
    ui.click_text(&menu, "Op");
    ui.settle();
    assert!(ui.controls.pending.is_none());
    assert_eq!(ui.commands.try_recv().unwrap(), format!("op {TARGET}"));
    assert_eq!(
        ui.commands.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    );
}

#[test]
fn player_rows_have_fixed_height_and_padded_left_aligned_content() {
    for name in [
        "ReaperHUN96".to_owned(),
        "A very long Bedrock player name ".repeat(20),
    ] {
        let mut ui = PlayerUi::new();
        ui.player.name = name;
        ui.player.edition = "Bedrock".to_owned();
        ui.rows[0] = ui.player.clone();
        ui.update_server(ServerStatus::Running, true);
        let frame = ui.settle();
        frame.assert_row_geometry(&ui.player.name, "Bedrock");
        ui.click(
            frame.text_center(&ui.player.name),
            egui::PointerButton::Primary,
        );
        let selected = ui.settle();
        selected.assert_selection(ui.player.id);
        selected.assert_row_geometry(&ui.player.name, "Bedrock");
        ui.click(
            selected.text_center("Bedrock"),
            egui::PointerButton::Secondary,
        );
        let menu = ui.settle();
        assert!(menu.row.context_menu_opened());
        ui.assert_no_submission();
    }
}

#[test]
fn an_open_menu_tracks_the_latest_operator_state() {
    let mut ui = PlayerUi::new();
    let menu = ui.open_menu();
    assert!(menu.texts.iter().any(|(text, _)| text == "Op"));
    assert!(!menu.texts.iter().any(|(text, _)| text == "Deop"));
    for is_op in [true, false] {
        ui.set_operator(is_op);
        let menu = ui.settle();
        assert!(menu.row.context_menu_opened());
        assert_eq!(menu.texts.iter().any(|(text, _)| text == "Op"), !is_op);
        assert_eq!(menu.texts.iter().any(|(text, _)| text == "Deop"), is_op);
        menu.assert_selection(ui.player.id);
    }
    ui.assert_no_submission();
}

#[test]
fn operator_state_changes_reject_an_immediate_action_without_reversing_or_retrying_it() {
    for (action, initially_op) in [(PlayerAction::Op, false), (PlayerAction::Deop, true)] {
        let mut ui = PlayerUi::new();
        ui.set_operator(initially_op);
        // Model an update arriving after menu selection but before processing.
        ui.controls.pending = Some(PendingPlayerAction {
            player: ui.player.clone(),
            action,
            reason: String::new(),
            error: None,
        });
        ui.set_operator(!initially_op);
        let frame = ui.settle();
        let expected_error = if initially_op {
            "This player is no longer an operator."
        } else {
            "This player is already an operator."
        };
        assert_eq!(ui.errors, vec![expected_error]);
        assert!(ui.controls.pending.is_none());
        assert!(!frame.texts.iter().any(|(text, _)| text == "Confirm"));
        ui.set_operator(initially_op);
        ui.settle();
        assert_eq!(ui.errors.len(), 1);
        ui.assert_no_submission();
    }
}

#[test]
fn immediate_queue_errors_are_returned_once_without_a_modal_or_automatic_retry() {
    for closed in [false, true] {
        let mut ui = PlayerUi::new();
        if closed {
            ui.commands.close();
        } else {
            for _ in 0..ui.commands.max_capacity() {
                ui.handle.submit_command("list").unwrap();
            }
        }
        let frame = ui.select_action(PlayerAction::Op);
        assert!(ui.controls.pending.is_none());
        assert!(!frame.texts.iter().any(|(text, _)| text == "Confirm"));
        assert!(ui.submitted.is_empty());
        assert_eq!(ui.errors.len(), 1);
        assert!(ui.errors[0].contains(if closed {
            "no longer accepting"
        } else {
            "queue is full"
        }));
        if !closed {
            for _ in 0..ui.commands.max_capacity() {
                assert_eq!(ui.commands.try_recv().unwrap(), "list");
            }
        }
        ui.settle();
        assert_eq!(ui.errors.len(), 1);
        assert!(ui.submitted.is_empty());
        assert!(ui.commands.try_recv().is_err());
    }
}

#[test]
fn left_and_right_click_select_the_uuid_and_paint_only_its_full_row() {
    let mut ui = PlayerUi::new();
    let other_id = uuid::Uuid::from_u128(2);
    ui.rows.push(GuiPlayer {
        id: other_id,
        name: "Steve".to_owned(),
        edition: "Bedrock".to_owned(),
        is_op: false,
    });
    ui.update_server(ServerStatus::Running, true);
    let frame = ui.settle();
    assert!(ui.controls.selected.is_none());
    assert!(frame.selection_rects.is_empty());

    ui.click_text(&frame, "Alex");
    let frame = ui.settle();
    assert_eq!(ui.controls.selected, Some(ui.player.id));
    assert!(!egui::Popup::is_any_open(&ui.context));
    frame.assert_selection(ui.player.id);

    ui.click(frame.text_center("Steve"), egui::PointerButton::Secondary);
    let menu = ui.settle();
    assert_eq!(ui.controls.selected, Some(other_id));
    assert!(egui::Popup::is_any_open(&ui.context));
    menu.assert_selection(other_id);

    ui.frame(vec![egui::Event::Key {
        key: egui::Key::Escape,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    }]);
    let frame = ui.settle();
    assert!(!egui::Popup::is_any_open(&ui.context));
    assert_eq!(ui.controls.selected, Some(other_id));
    frame.assert_selection(other_id);

    ui.rows.swap(0, 1);
    ui.update_server(ServerStatus::Running, true);
    let frame = ui.settle();
    assert_eq!(ui.controls.selected, Some(other_id));
    frame.assert_selection(other_id);

    // Selection cleanup must run even when no confirmation was ever opened.
    let mut snapshot = ui.handle.snapshot();
    snapshot.players.retain(|player| player.id != other_id);
    ui.handle.apply_backend_update(snapshot, Vec::new());
    let frame = ui.settle();
    assert!(ui.controls.pending.is_none());
    assert!(ui.controls.selected.is_none());
    assert!(frame.selection_rects.is_empty());
    ui.assert_no_submission();
}

#[test]
fn stopping_clears_row_selection_without_an_open_confirmation() {
    let mut ui = PlayerUi::new();
    let frame = ui.settle();
    ui.click_text(&frame, "Alex");
    ui.settle().assert_selection(ui.player.id);
    assert!(ui.controls.pending.is_none());
    ui.handle.request_stop();
    let frame = ui.settle();
    assert!(ui.controls.selected.is_none());
    assert!(frame.selection_rects.is_empty());
    ui.assert_no_submission();
}
