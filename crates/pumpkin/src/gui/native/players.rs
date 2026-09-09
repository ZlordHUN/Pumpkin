use eframe::egui::{self, Align, FontId, RichText, Vec2};
use pumpkin::gui::{GuiHandle, GuiPlayer, ServerStatus, player_actions::PlayerAction};

use super::{GREEN, MUTED, RED};

pub(super) const PLAYER_ROW_HEIGHT: f32 = 28.0;

#[derive(Default)]
pub(super) struct PlayerControls {
    selected: Option<uuid::Uuid>,
    pending: Option<PendingPlayerAction>,
}

struct PendingPlayerAction {
    player: GuiPlayer,
    action: PlayerAction,
    reason: String,
    error: Option<String>,
}

impl PlayerControls {
    pub(super) fn show_player(
        &mut self,
        ui: &mut egui::Ui,
        player: &GuiPlayer,
        enabled: bool,
    ) -> egui::Response {
        // Keep menus attached to the selected identity, not its sorted row index.
        ui.push_id(player.id, |ui| {
            let (_, rect) = ui.allocate_space(Vec2::new(ui.available_width(), PLAYER_ROW_HEIGHT));
            let row = ui
                .interact(
                    rect,
                    ui.make_persistent_id("player_row"),
                    egui::Sense::click(),
                )
                .on_hover_text(format!("{}\nRight-click for player actions", player.name));
            if row.clicked() || row.secondary_clicked() {
                self.selected = Some(player.id);
                ui.ctx().request_repaint();
            }
            let selected = self.selected == Some(player.id);
            paint_player_row(ui, rect, player, selected);
            row.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::SelectableLabel,
                    ui.is_enabled(),
                    selected,
                    &player.name,
                )
            });
            row.context_menu(|ui| {
                ui.label(RichText::new(&player.name).strong());
                ui.separator();
                ui.add_enabled_ui(enabled, |ui| {
                    for action in [PlayerAction::Kick, PlayerAction::Ban] {
                        self.action_button(ui, player, action);
                    }
                    self.action_button(
                        ui,
                        player,
                        if player.is_op {
                            PlayerAction::Deop
                        } else {
                            PlayerAction::Op
                        },
                    );
                    ui.menu_button("Game mode", |ui| {
                        for action in [
                            PlayerAction::Survival,
                            PlayerAction::Creative,
                            PlayerAction::Adventure,
                            PlayerAction::Spectator,
                        ] {
                            self.action_button(ui, player, action);
                        }
                    });
                    ui.menu_button("Whitelist", |ui| {
                        for action in [PlayerAction::WhitelistAdd, PlayerAction::WhitelistRemove] {
                            self.action_button(ui, player, action);
                        }
                    });
                    ui.separator();
                    self.action_button(ui, player, PlayerAction::Kill);
                    self.action_button(ui, player, PlayerAction::ClearInventory);
                });
                ui.separator();
                if ui.button("Copy name").clicked() {
                    ui.ctx().copy_text(player.name.clone());
                    ui.close();
                }
                if ui.button("Copy UUID").clicked() {
                    ui.ctx().copy_text(player.id.to_string());
                    ui.close();
                }
            });
            row
        })
        .inner
    }

    fn action_button(&mut self, ui: &mut egui::Ui, player: &GuiPlayer, action: PlayerAction) {
        let label = if action.accepts_reason() {
            format!("{}...", action.label())
        } else {
            action.label().to_string()
        };
        if ui
            .button(label)
            .on_hover_text(action.description())
            .clicked()
        {
            self.pending = Some(PendingPlayerAction {
                player: player.clone(),
                action,
                reason: String::new(),
                error: None,
            });
            ui.close();
        }
    }

    /// Runs immediate actions once, or shows the Kick/Ban reason dialog. Returns
    /// queued commands and admission errors to the existing console UI.
    pub(super) fn process_actions(
        &mut self,
        context: &egui::Context,
        handle: &GuiHandle,
    ) -> Option<Result<String, String>> {
        let snapshot = handle.snapshot();
        if snapshot.status != ServerStatus::Running
            || handle.close_requested()
            || self
                .selected
                .is_some_and(|id| !snapshot.players.iter().any(|player| player.id == id))
        {
            self.selected = None;
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| !pending.action.accepts_reason())
        {
            let pending = self.pending.take()?;
            return Some(handle.submit_player_action(pending.player.id, pending.action, ""));
        }
        let pending = self.pending.as_mut()?;
        // Do not carry a confirmation into a later login or backend run, or leave
        // a modal covering Start after an external shutdown.
        if snapshot.status != ServerStatus::Running || handle.close_requested() {
            self.pending = None;
            return None;
        }
        if !snapshot
            .players
            .iter()
            .any(|player| player.id == pending.player.id)
        {
            self.pending = None;
            return None;
        }
        let unavailable = (!snapshot.commands_enabled)
            .then_some("The server console is disabled in configuration.");
        let mut queued = None;
        let mut cancelled = false;
        let dialog = egui::Modal::new(egui::Id::new("player_action_confirmation"))
            .frame(egui::Frame::popup(&context.style_of(egui::Theme::Dark)).inner_margin(20))
            .show(context, |ui| {
                ui.set_width(480.0);
                ui.spacing_mut().item_spacing = Vec2::new(12.0, 10.0);
                ui.heading(RichText::new(pending.action.label()).size(24.0));
                ui.label(
                    RichText::new(format!(
                        "{} ({})",
                        pending.player.name, pending.player.edition
                    ))
                    .size(16.0),
                );
                ui.label(
                    RichText::new(pending.player.id.to_string())
                        .monospace()
                        .size(12.0)
                        .color(MUTED),
                );
                ui.add_space(12.0);
                ui.label(pending.action.description());
                if pending.action.accepts_reason() {
                    ui.add_space(12.0);
                    ui.label("Reason (optional)");
                    ui.add(
                        egui::TextEdit::singleline(&mut pending.reason)
                            .id_salt("player_action_reason")
                            .desired_width(f32::INFINITY)
                            .min_size(Vec2::new(0.0, 38.0))
                            .vertical_align(Align::Center)
                            .margin(egui::Margin::symmetric(10, 8))
                            .char_limit(256),
                    );
                }
                if let Some(error) = unavailable.or(pending.error.as_deref()) {
                    ui.label(RichText::new(error).color(RED));
                }
                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    cancelled = ui
                        .add_sized(Vec2::new(110.0, 38.0), egui::Button::new("Cancel"))
                        .clicked();
                    if ui
                        .add_enabled(
                            unavailable.is_none(),
                            egui::Button::new("Confirm").min_size(Vec2::new(110.0, 38.0)),
                        )
                        .clicked()
                    {
                        match handle.submit_player_action(
                            pending.player.id,
                            pending.action,
                            &pending.reason,
                        ) {
                            Ok(command) => queued = Some(command),
                            Err(error) => pending.error = Some(error),
                        }
                    }
                });
            });
        if cancelled || dialog.should_close() || queued.is_some() {
            self.pending = None;
        }
        queued.map(Ok)
    }
}

fn paint_player_row(ui: &egui::Ui, rect: egui::Rect, player: &GuiPlayer, selected: bool) {
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, 3, ui.visuals().selection.bg_fill);
    }
    let content = rect.shrink2(Vec2::new(8.0, 0.0));
    painter.circle_filled(
        egui::pos2(content.left() + 3.0, rect.center().y),
        3.0,
        GREEN,
    );
    let edition = painter.layout_no_wrap(player.edition.clone(), FontId::proportional(11.0), MUTED);
    let edition_left = content.right() - edition.size().x;
    painter.galley(
        egui::pos2(edition_left, rect.center().y - edition.size().y / 2.0),
        edition,
        MUTED,
    );

    let name_left = content.left() + 14.0;
    let name_width = (edition_left - 8.0 - name_left).max(0.0);
    let name_rect = egui::Rect::from_min_size(
        egui::pos2(name_left, rect.top()),
        Vec2::new(name_width, rect.height()),
    );
    let color = ui.visuals().text_color();
    let mut job = egui::text::LayoutJob::simple_singleline(
        player.name.clone(),
        egui::TextStyle::Body.resolve(ui.style()),
        color,
    );
    job.wrap.max_width = name_width;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let name = painter.layout_job(job);
    // Clip even the ellipsis when the name column becomes exceptionally narrow.
    painter.with_clip_rect(name_rect.intersect(rect)).galley(
        egui::pos2(name_left, rect.center().y - name.size().y / 2.0),
        name,
        color,
    );
}

#[cfg(test)]
#[path = "players_tests.rs"]
mod tests;
