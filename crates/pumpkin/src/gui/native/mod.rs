use std::{collections::VecDeque, time::Duration};

use eframe::egui::{self, Align, Color32, FontId, RichText, Stroke, Vec2};
use pumpkin::gui::{
    GuiHandle, MEMORY_HISTORY_CAPACITY, MEMORY_SAMPLE_INTERVAL, ServerSnapshot, ServerStatus,
};

mod console;
mod players;
use console::ConsoleDocument;
use players::{PLAYER_ROW_HEIGHT, PlayerControls};

const BACKGROUND: Color32 = Color32::from_rgb(19, 21, 24);
const SURFACE: Color32 = Color32::from_rgb(27, 30, 34);
const BORDER: Color32 = Color32::from_rgb(47, 51, 57);
const TEXT: Color32 = Color32::from_rgb(232, 234, 237);
const MUTED: Color32 = Color32::from_rgb(146, 154, 166);
const ORANGE: Color32 = Color32::from_rgb(245, 148, 66);
const GREEN: Color32 = Color32::from_rgb(124, 202, 153);
const RED: Color32 = Color32::from_rgb(244, 131, 131);
const HISTORY_CAPACITY: usize = 64;
const APP_ID: &str = "org.pumpkinmc.Pumpkin";

pub fn run(handle: GuiHandle) -> eframe::Result {
    let icon = image::load_from_memory(include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/default_icon.png"
    )))
    .ok()
    .map(image::DynamicImage::into_rgba8);
    let mut viewport = egui::ViewportBuilder::default()
        // Wayland desktops resolve the taskbar icon through the matching .desktop entry.
        .with_app_id(APP_ID)
        .with_inner_size([1_100.0, 740.0])
        .with_min_inner_size([800.0, 550.0]);
    if let Some(icon) = &icon {
        viewport = viewport.with_icon(egui::IconData {
            rgba: icon.as_raw().clone(),
            width: icon.width(),
            height: icon.height(),
        });
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "Pumpkin Server",
        options,
        Box::new(move |context| {
            configure_style(&context.egui_ctx);
            let logo = icon.map(|icon| {
                context.egui_ctx.load_texture(
                    "pumpkin",
                    egui::ColorImage::from_rgba_unmultiplied(
                        [icon.width() as usize, icon.height() as usize],
                        icon.as_raw(),
                    ),
                    egui::TextureOptions::LINEAR,
                )
            });
            Ok(Box::new(ServerApp {
                handle,
                logo,
                console_document: ConsoleDocument::default(),
                history: CommandHistory::default(),
                command: String::new(),
                command_error: None,
                player_controls: PlayerControls::default(),
                close_when_stopped: false,
            }))
        }),
    )
}

fn configure_style(context: &egui::Context) {
    context.set_theme(egui::Theme::Dark);
    context.style_mut_of(egui::Theme::Dark, |style| {
        style.visuals = egui::Visuals::dark();
        style.visuals.panel_fill = BACKGROUND;
        style.visuals.window_fill = SURFACE;
        style.visuals.extreme_bg_color = BACKGROUND;
        style.visuals.override_text_color = Some(TEXT);
        style.visuals.weak_text_color = Some(MUTED);
        style.visuals.selection.bg_fill = Color32::from_rgb(109, 66, 34);
        style.visuals.selection.stroke = Stroke::new(1.0, ORANGE);
        style.visuals.widgets.inactive.bg_fill = SURFACE;
        style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
        style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(57, 48, 40);
        style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ORANGE);
        style.visuals.widgets.active.bg_stroke = Stroke::new(1.0, ORANGE);
        style.spacing.item_spacing = Vec2::new(10.0, 8.0);
        style.spacing.button_padding = Vec2::new(13.0, 8.0);
        style.spacing.interact_size.y = 32.0;
        style
            .text_styles
            .insert(egui::TextStyle::Body, FontId::proportional(14.0));
        style
            .text_styles
            .insert(egui::TextStyle::Monospace, FontId::monospace(12.5));
    });
}

struct ServerApp {
    handle: GuiHandle,
    logo: Option<egui::TextureHandle>,
    console_document: ConsoleDocument,
    history: CommandHistory,
    command: String,
    command_error: Option<String>,
    player_controls: PlayerControls,
    close_when_stopped: bool,
}

impl eframe::App for ServerApp {
    fn logic(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.console_document.append(self.handle.drain_logs());
        let snapshot = self.handle.snapshot();
        if self.handle.close_requested() {
            self.close_when_stopped = true;
        }
        if update_close_state(context, snapshot.status, &mut self.close_when_stopped) {
            self.handle.request_stop();
        }
        context.request_repaint_after(Duration::from_millis(250));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let snapshot = self.handle.snapshot();
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(BACKGROUND).inner_margin(20))
            .show(ui, |ui| {
                self.header(ui, &snapshot);
                ui.add_space(12.0);
                let available = ui.available_size();
                let sidebar_width = (available.x * 0.26).clamp(220.0, 270.0);
                ui.horizontal_top(|ui| {
                    ui.allocate_ui_with_layout(
                        Vec2::new(sidebar_width, available.y),
                        egui::Layout::top_down(Align::LEFT),
                        |ui| {
                            ui.set_width(sidebar_width);
                            self.sidebar(ui, &snapshot);
                        },
                    );
                    ui.allocate_ui_with_layout(
                        Vec2::new(ui.available_width(), available.y),
                        egui::Layout::top_down(Align::LEFT),
                        |ui| self.console(ui, &snapshot),
                    );
                });
            });
        if let Some(result) = self.player_controls.process_actions(ui.ctx(), &self.handle) {
            match result {
                Ok(command) => {
                    self.history.record(command);
                    self.command_error = None;
                }
                Err(error) => self.command_error = Some(error),
            }
        }
    }
}

impl ServerApp {
    fn header(&mut self, ui: &mut egui::Ui, snapshot: &ServerSnapshot) {
        ui.horizontal(|ui| {
            if let Some(logo) = &self.logo {
                ui.image((logo.id(), Vec2::splat(42.0)));
            }
            ui.vertical(|ui| {
                ui.label(RichText::new("Pumpkin").size(26.0).strong());
                ui.label(
                    RichText::new(format!("SERVER CONSOLE  /  {}", snapshot.version))
                        .size(11.0)
                        .color(MUTED),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                let (button, _badge, action) =
                    server_controls(ui, snapshot.status, self.close_when_stopped);
                if button.clicked() {
                    match action {
                        ServerControlAction::Stop => self.handle.request_stop(),
                        ServerControlAction::Start => {
                            self.command_error = self.handle.request_start().err();
                        }
                    }
                }
            });
        });
    }

    fn sidebar(&mut self, ui: &mut egui::Ui, snapshot: &ServerSnapshot) {
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.spacing_mut().interact_size.y = 18.0;
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            section_label(ui, "OVERVIEW");
            ui.add(
                egui::Label::new(RichText::new(&snapshot.world_name).size(19.0).strong())
                    .truncate(),
            );
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(format!("{:.1}", snapshot.tps))
                            .size(28.0)
                            .color(GREEN),
                    );
                    ui.label(RichText::new("ticks / second").size(11.0).color(MUTED));
                });
                ui.with_layout(egui::Layout::right_to_left(Align::TOP), |ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(format!("{:.3}", snapshot.mspt)).size(28.0));
                        ui.label(RichText::new("ms / tick").size(11.0).color(MUTED));
                    });
                });
            });
            if snapshot.tick_frozen {
                ui.label(RichText::new("Ticking is frozen").small().color(ORANGE));
            }
            ui.separator();
            detail_row(
                ui,
                "Process RAM",
                &format!("{:.0} MiB", snapshot.memory_bytes as f64 / 1_048_576.0),
            );
            memory_graph(ui, &snapshot.memory_history);
            detail_row(ui, "Uptime", &format_uptime(snapshot.uptime));
        });
        ui.add_space(4.0);
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            section_label(ui, "CONNECTIONS");
            for (edition, address) in [
                ("Java", &snapshot.java_address),
                ("Bedrock", &snapshot.bedrock_address),
            ] {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(edition).size(12.0).color(MUTED));
                    ui.add(
                        egui::Label::new(
                            RichText::new(address.as_deref().unwrap_or("Disabled"))
                                .monospace()
                                .size(11.0),
                        )
                        .truncate(),
                    );
                });
            }
        });
        ui.add_space(4.0);
        self.player_list(ui, snapshot);
    }

    fn player_list(&mut self, ui: &mut egui::Ui, snapshot: &ServerSnapshot) {
        let height = ui.available_height().max(76.0);
        card().show(ui, |ui| {
            ui.set_min_size(Vec2::new(ui.available_width(), (height - 30.0).max(46.0)));
            ui.horizontal(|ui| {
                section_label(ui, "PLAYERS");
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!(
                            "{} / {}",
                            snapshot.players.len(),
                            snapshot.max_players
                        ))
                        .size(12.0)
                        .color(MUTED),
                    );
                });
            });
            if snapshot.players.is_empty() {
                ui.add_space(6.0);
                ui.label(RichText::new("No players online").color(MUTED));
            } else {
                ui.spacing_mut().interact_size.y = PLAYER_ROW_HEIGHT;
                egui::ScrollArea::vertical()
                    .id_salt("players")
                    .max_height((height - 64.0).max(24.0))
                    .show_rows(
                        ui,
                        PLAYER_ROW_HEIGHT,
                        snapshot.players.len(),
                        |ui, range| {
                            for index in range {
                                let player = &snapshot.players[index];
                                self.player_controls.show_player(
                                    ui,
                                    player,
                                    snapshot.status == ServerStatus::Running
                                        && snapshot.commands_enabled
                                        && !self.close_when_stopped,
                                );
                            }
                        },
                    );
            }
        });
    }

    fn console(&mut self, ui: &mut egui::Ui, snapshot: &ServerSnapshot) {
        let size = ui.available_size();
        card().show(ui, |ui| {
            ui.set_min_size(Vec2::new(
                (size.x - 30.0).max(0.0),
                (size.y - 30.0).max(0.0),
            ));
            ui.horizontal(|ui| {
                ui.label(RichText::new("Server console").strong().size(16.0));
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new("Scroll up to pause").color(MUTED).size(11.0));
                });
            });
            ui.separator();
            let status_message =
                if self.close_when_stopped || snapshot.status == ServerStatus::Stopping {
                    Some("Saving worlds and stopping safely…")
                } else if has_stopped(snapshot.status) {
                    Some("Server stopped. Start the server to run again.")
                } else if snapshot.status == ServerStatus::Starting {
                    Some("Starting the server…")
                } else if !snapshot.commands_enabled {
                    Some("The server console is disabled in configuration.")
                } else {
                    None
                };
            let mut footer_height = 48.0;
            if status_message.is_some() {
                footer_height += 24.0;
            }
            if snapshot.last_error.is_some() || self.command_error.is_some() {
                footer_height += 38.0;
            }
            let log_height = (ui.available_height() - footer_height).max(60.0);
            self.console_document.show(ui, log_height);
            ui.add_space(3.0);
            self.command_input(ui, snapshot.commands_enabled);
            if let Some(error) = self.command_error.as_ref().or(snapshot.last_error.as_ref()) {
                ui.add(egui::Label::new(RichText::new(error).small().color(RED)).truncate());
            }
            if let Some(message) = status_message {
                ui.label(RichText::new(message).size(11.0).color(MUTED));
            }
        });
    }

    fn command_input(&mut self, ui: &mut egui::Ui, enabled: bool) {
        ui.add_enabled_ui(enabled, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(">").monospace().color(ORANGE));
                let response = ui.add_sized(
                    Vec2::new((ui.available_width() - 72.0).max(40.0), 34.0),
                    command_text_edit(&mut self.command),
                );
                if response.has_focus() {
                    if ui.input(|input| input.key_pressed(egui::Key::ArrowUp)) {
                        self.history.previous(&mut self.command);
                    } else if ui.input(|input| input.key_pressed(egui::Key::ArrowDown)) {
                        self.history.next(&mut self.command);
                    }
                }
                let enter =
                    response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                let clicked = ui
                    .add_enabled(!self.command.trim().is_empty(), egui::Button::new("Send"))
                    .clicked();
                if enter || clicked {
                    let command = self.command.trim();
                    if !command.is_empty() {
                        match self.handle.submit_command(command) {
                            Ok(()) => {
                                self.history.record(command.to_owned());
                                self.command.clear();
                                self.command_error = None;
                            }
                            Err(error) => self.command_error = Some(error),
                        }
                    }
                    response.request_focus();
                }
            });
        });
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ServerControlAction {
    Start,
    Stop,
}

fn server_controls(
    ui: &mut egui::Ui,
    status: ServerStatus,
    closing: bool,
) -> (egui::Response, egui::Response, ServerControlAction) {
    let button_font = egui::TextStyle::Button.resolve(ui.style());
    let status_font = FontId::proportional(12.0);
    let mut content_size = Vec2::ZERO;
    for label in ["Start server", "Stop server"] {
        let galley = ui
            .painter()
            .layout_no_wrap(label.to_owned(), button_font.clone(), TEXT);
        content_size = content_size.max(galley.size());
    }
    for status in [
        ServerStatus::Starting,
        ServerStatus::Running,
        ServerStatus::Stopping,
        ServerStatus::Stopped,
        ServerStatus::Failed,
    ] {
        let galley = ui.painter().layout_no_wrap(
            status_label(status).0.to_owned(),
            status_font.clone(),
            TEXT,
        );
        content_size = content_size.max(galley.size() + Vec2::new(14.0, 0.0));
    }
    let size = (content_size + ui.spacing().button_padding * 2.0 + Vec2::splat(2.0))
        .max(ui.spacing().interact_size);
    let (action, label, enabled, hint) = if has_stopped(status) {
        (
            ServerControlAction::Start,
            "Start server",
            !closing,
            "Start the server again",
        )
    } else {
        (
            ServerControlAction::Stop,
            "Stop server",
            !closing && status != ServerStatus::Stopping,
            "Save the worlds and stop the server",
        )
    };
    let button = ui
        .add_enabled(
            enabled,
            egui::Button::new(RichText::new(label).font(button_font))
                .min_size(size)
                .corner_radius(3),
        )
        .on_hover_text(hint);

    // Match the button's actual bounds, including any theme-dependent sizing.
    let (rect, badge) = ui.allocate_exact_size(button.rect.size(), egui::Sense::hover());
    let (label, color) = status_label(status);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), status_font, color);
    let content_width = galley.size().x + 14.0;
    let left = rect.center().x - content_width / 2.0;
    ui.painter()
        .rect_filled(rect, 3, color.gamma_multiply(0.12));
    ui.painter()
        .circle_filled(egui::pos2(left + 3.0, rect.center().y), 3.0, color);
    ui.painter().galley(
        egui::pos2(left + 14.0, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    badge
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, ui.is_enabled(), label));
    (button, badge, action)
}

fn command_text_edit(command: &mut String) -> egui::TextEdit<'_> {
    egui::TextEdit::singleline(command)
        .font(egui::TextStyle::Monospace)
        .hint_text("Enter a server command…")
        .vertical_align(Align::Center)
        .margin(egui::Margin::symmetric(10, 0))
        .char_limit(4_096)
}

/// Returns whether this frame needs to request backend shutdown. A close event
/// never bypasses the backend's normal shutdown and save sequence.
fn update_close_state(
    context: &egui::Context,
    status: ServerStatus,
    close_when_stopped: &mut bool,
) -> bool {
    if *close_when_stopped && has_stopped(status) {
        context.send_viewport_cmd(egui::ViewportCommand::Close);
    } else if context.input(|input| input.viewport().close_requested()) && !has_stopped(status) {
        context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        let request_stop = !*close_when_stopped;
        *close_when_stopped = true;
        return request_stop;
    }
    false
}

fn memory_graph(ui: &mut egui::Ui, samples: &VecDeque<u64>) {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 40.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let ceiling = memory_graph_ceiling(samples);
    for fraction in [0.0, 0.5, 1.0] {
        let y = rect.bottom() - rect.height() * fraction;
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            Stroke::new(1.0, BORDER),
        );
    }
    let points = memory_graph_points(rect, samples, ceiling);
    if points.len() > 1 {
        painter.add(egui::Shape::line(points, Stroke::new(1.5, ORANGE)));
    }
    response.on_hover_text(format!(
        "Pumpkin process resident memory, sampled every {:.1} seconds.\nLast {:.0} seconds; vertical scale 0 to {:.0} MiB (automatic).\nThis is process RAM, not a JVM heap limit.",
        MEMORY_SAMPLE_INTERVAL.as_secs_f64(),
        MEMORY_SAMPLE_INTERVAL.as_secs_f64() * MEMORY_HISTORY_CAPACITY as f64,
        ceiling / 1_048_576.0
    ));
}

fn memory_graph_ceiling(samples: &VecDeque<u64>) -> f64 {
    // Leave a little headroom, with a nonzero scale while the first sample arrives.
    (samples.iter().copied().max().unwrap_or(0) as f64 * 1.1).max(1_048_576.0)
}

fn memory_graph_points(rect: egui::Rect, samples: &VecDeque<u64>, ceiling: f64) -> Vec<egui::Pos2> {
    samples
        .iter()
        .enumerate()
        .map(|(index, value)| {
            egui::pos2(
                rect.right()
                    - (samples.len() - 1 - index) as f32 * rect.width()
                        / (MEMORY_HISTORY_CAPACITY - 1) as f32,
                rect.bottom() - (*value as f64 / ceiling).clamp(0.0, 1.0) as f32 * rect.height(),
            )
        })
        .collect()
}

fn card() -> egui::Frame {
    egui::Frame::NONE
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(10)
        .inner_margin(14)
}

fn section_label(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(10.0).strong().color(MUTED));
}

fn detail_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(12.0).color(MUTED));
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(value).size(12.0));
        });
    });
}

const fn has_stopped(status: ServerStatus) -> bool {
    matches!(status, ServerStatus::Stopped | ServerStatus::Failed)
}

const fn status_label(status: ServerStatus) -> (&'static str, Color32) {
    match status {
        ServerStatus::Starting => ("Starting", ORANGE),
        ServerStatus::Running => ("Running", GREEN),
        ServerStatus::Stopping => ("Stopping", ORANGE),
        ServerStatus::Stopped => ("Stopped", MUTED),
        ServerStatus::Failed => ("Failed", RED),
    }
}

const fn log_color(level: tracing::Level) -> Color32 {
    match level {
        tracing::Level::ERROR => RED,
        tracing::Level::WARN => ORANGE,
        tracing::Level::INFO => GREEN,
        _ => MUTED,
    }
}

fn format_uptime(uptime: Duration) -> String {
    let seconds = uptime.as_secs();
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3_600,
        seconds / 60 % 60,
        seconds % 60
    )
}

fn push_bounded<T>(values: &mut VecDeque<T>, value: T, capacity: usize) {
    if values.len() == capacity {
        values.pop_front();
    }
    values.push_back(value);
}

#[derive(Default)]
struct CommandHistory {
    entries: VecDeque<String>,
    cursor: Option<usize>,
    draft: String,
}

impl CommandHistory {
    fn record(&mut self, command: String) {
        if self.entries.back() != Some(&command) {
            push_bounded(&mut self.entries, command, HISTORY_CAPACITY);
        }
        self.cursor = None;
        self.draft.clear();
    }

    fn previous(&mut self, command: &mut String) {
        if self.entries.is_empty() {
            return;
        }
        let index = self.cursor.map_or_else(
            || {
                self.draft.clone_from(command);
                self.entries.len() - 1
            },
            |index| index.saturating_sub(1),
        );
        self.cursor = Some(index);
        command.clone_from(&self.entries[index]);
    }

    fn next(&mut self, command: &mut String) {
        if let Some(index) = self.cursor {
            if index + 1 < self.entries.len() {
                self.cursor = Some(index + 1);
                command.clone_from(&self.entries[index + 1]);
            } else {
                self.cursor = None;
                command.clone_from(&self.draft);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn controls_frame(
        context: &egui::Context,
        status: ServerStatus,
        closing: bool,
        events: Vec<egui::Event>,
    ) -> (egui::Response, egui::Response, ServerControlAction) {
        let mut controls = None;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(400.0, 80.0),
            )),
            events,
            ..Default::default()
        };
        let mut output = context.run_ui(input, |ui| {
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                controls = Some(server_controls(ui, status, closing));
            });
        });
        output.textures_delta.clear();
        controls.expect("rendered server controls")
    }

    #[test]
    fn status_and_start_stop_controls_have_equal_stable_sizes() {
        let mut expected_size = None;
        for (status, expected_action, enabled) in [
            (ServerStatus::Starting, ServerControlAction::Stop, true),
            (ServerStatus::Running, ServerControlAction::Stop, true),
            (ServerStatus::Stopping, ServerControlAction::Stop, false),
            (ServerStatus::Stopped, ServerControlAction::Start, true),
            (ServerStatus::Failed, ServerControlAction::Start, true),
        ] {
            let context = egui::Context::default();
            configure_style(&context);
            let (button, badge, action) = controls_frame(&context, status, false, Vec::new());
            assert_eq!(action, expected_action);
            assert_eq!(button.enabled(), enabled);
            assert_eq!(button.rect.size(), badge.rect.size());
            assert_eq!(button.rect.center().y, badge.rect.center().y);
            assert!(badge.rect.right() < button.rect.left());
            assert_eq!(
                *expected_size.get_or_insert(button.rect.size()),
                button.rect.size()
            );

            let position = button.rect.center();
            let events = vec![
                egui::Event::PointerMoved(position),
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ];
            let (button, _, action) = controls_frame(&context, status, false, events);
            assert_eq!(button.clicked(), enabled);
            assert_eq!(action, expected_action);
        }
    }

    #[test]
    fn pending_window_close_disables_start_and_stop_controls() {
        for status in [
            ServerStatus::Running,
            ServerStatus::Stopped,
            ServerStatus::Failed,
        ] {
            let context = egui::Context::default();
            configure_style(&context);
            let (button, _, _) = controls_frame(&context, status, true, Vec::new());
            assert!(!button.enabled());
        }
    }

    #[test]
    fn desktop_entry_matches_the_window_and_icon_identity() {
        let entry_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets")
            .join(format!("{APP_ID}.desktop"));
        let entry = std::fs::read_to_string(entry_path).expect("matching desktop entry");
        for expected in [
            format!("Icon={APP_ID}"),
            format!("StartupWMClass={APP_ID}"),
            "Exec=pumpkin --gui".to_owned(),
            "NoDisplay=true".to_owned(),
        ] {
            assert!(entry.lines().any(|line| line == expected));
        }
        let icon = image::load_from_memory(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/default_icon.png"
        )))
        .expect("embedded Pumpkin icon")
        .into_rgba8();
        assert_eq!(icon.dimensions(), (64, 64));
        assert!(icon.pixels().any(|pixel| pixel[3] > 0));
    }

    fn command_field_frame(
        context: &egui::Context,
        command: &mut String,
        focused: bool,
    ) -> (egui::text_edit::TextEditOutput, egui::FullOutput) {
        let id = egui::Id::new("command_field_geometry");
        if focused {
            context.memory_mut(|memory| memory.request_focus(id));
        }
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(400.0, 100.0),
            )),
            focused: true,
            ..Default::default()
        };
        let mut field = None;
        let mut output = context.run_ui(input, |ui| {
            ui.horizontal(|ui| {
                // Match Ui::add_sized while retaining TextEditOutput geometry.
                let layout = egui::Layout::centered_and_justified(ui.layout().main_dir());
                field = Some(
                    ui.allocate_ui_with_layout(Vec2::new(300.0, 34.0), layout, |ui| {
                        command_text_edit(command).id(id).show(ui)
                    })
                    .inner,
                );
            });
        });
        output.textures_delta.clear();
        (field.expect("rendered command field"), output)
    }

    #[test]
    fn command_field_centers_hint_text_and_caret_with_left_padding() {
        for (text, focused) in [("", false), ("", true), ("say hello", true)] {
            let context = egui::Context::default();
            configure_style(&context);
            let (field, output) = command_field_frame(&context, &mut text.to_owned(), focused);
            let bounds = field.response.rect;
            assert!((bounds.height() - 34.0).abs() <= 1.0);
            let inset = field.text_clip_rect.left() - bounds.left();
            assert!((inset - 10.0).abs() <= 1.0, "left inset: {inset}");

            let displayed = if text.is_empty() {
                "Enter a server command…"
            } else {
                text
            };
            // An empty editor's galley is empty; inspect the painted hint too.
            let text_bounds = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(shape) if shape.galley.text() == displayed => {
                        Some(shape.galley.rect.translate(shape.pos.to_vec2()))
                    }
                    _ => None,
                })
                .expect("painted hint or command text");
            assert!((text_bounds.center().y - bounds.center().y).abs() <= 1.0);
            assert!(text_bounds.left() >= bounds.left() + 9.0);
            if !text.is_empty() {
                let center = field.galley_pos.y + field.galley.size().y / 2.0;
                assert!((center - bounds.center().y).abs() <= 1.0);
            }
            if focused {
                assert!(field.response.has_focus());
                let caret = output
                    .platform_output
                    .ime
                    .expect("focused caret")
                    .cursor_rect;
                assert!((caret.center().y - bounds.center().y).abs() <= 1.0);
                assert!(bounds.contains_rect(caret));
            }
        }
    }

    #[test]
    fn running_console_has_no_shortcut_footer_and_fits_its_panel() {
        let context = egui::Context::default();
        configure_style(&context);
        let handle = GuiHandle::new();
        let mut snapshot = handle.snapshot();
        snapshot.status = ServerStatus::Running;
        snapshot.commands_enabled = true;
        let mut app = ServerApp {
            handle,
            logo: None,
            console_document: ConsoleDocument::default(),
            history: CommandHistory::default(),
            command: String::new(),
            command_error: None,
            player_controls: PlayerControls::default(),
            close_when_stopped: false,
        };
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(530.0, 420.0),
            )),
            ..Default::default()
        };
        let mut output = context.run_ui(input, |ui| {
            let bounds = ui.max_rect();
            app.console(ui, &snapshot);
            assert!(
                bounds.expand(1.0).contains_rect(ui.min_rect()),
                "console content {:?} exceeds panel {bounds:?}",
                ui.min_rect()
            );
        });
        output.textures_delta.clear();
        let texts: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(shape) => Some(shape.galley.text()),
                _ => None,
            })
            .collect();
        assert!(texts.contains(&"Enter a server command…"));
        assert!(!texts.iter().any(|text| text.contains("Up/Down")));
    }

    fn close_frame(
        context: &egui::Context,
        status: ServerStatus,
        clicked_close: bool,
        pending_close: &mut bool,
    ) -> (bool, Vec<egui::ViewportCommand>) {
        let mut input = egui::RawInput::default();
        if clicked_close {
            input
                .viewports
                .entry(egui::ViewportId::ROOT)
                .or_default()
                .events
                .push(egui::ViewportEvent::Close);
        }
        let mut request_stop = false;
        let mut output = context.run_ui(input, |ui| {
            request_stop |= update_close_state(ui.ctx(), status, pending_close);
        });
        output.textures_delta.clear();
        (
            request_stop,
            output
                .viewport_output
                .remove(&egui::ViewportId::ROOT)
                .unwrap()
                .commands,
        )
    }

    #[test]
    fn native_close_waits_for_backend_completion_and_requests_stop_once() {
        let context = egui::Context::default();
        let mut pending_close = false;
        let (stop, commands) =
            close_frame(&context, ServerStatus::Running, false, &mut pending_close);
        assert!(!stop);
        assert!(
            !commands
                .iter()
                .any(|command| matches!(command, egui::ViewportCommand::Close))
        );

        let (stop, commands) =
            close_frame(&context, ServerStatus::Running, true, &mut pending_close);
        assert!(stop);
        assert!(pending_close);
        assert!(
            commands
                .iter()
                .any(|command| matches!(command, egui::ViewportCommand::CancelClose))
        );

        for clicked_close in [false, true, true] {
            let (stop, commands) = close_frame(
                &context,
                ServerStatus::Stopping,
                clicked_close,
                &mut pending_close,
            );
            assert!(!stop);
            assert!(
                !commands
                    .iter()
                    .any(|command| matches!(command, egui::ViewportCommand::Close))
            );
            if clicked_close {
                assert!(
                    commands
                        .iter()
                        .any(|command| matches!(command, egui::ViewportCommand::CancelClose))
                );
            }
        }
        let (stop, commands) =
            close_frame(&context, ServerStatus::Stopped, false, &mut pending_close);
        assert!(!stop);
        assert!(
            commands
                .iter()
                .any(|command| matches!(command, egui::ViewportCommand::Close))
        );
    }

    #[test]
    fn ordinary_stop_keeps_window_open_for_another_start() {
        let context = egui::Context::default();
        let mut pending_close = false;
        for status in [
            ServerStatus::Starting,
            ServerStatus::Running,
            ServerStatus::Stopping,
        ] {
            let (stop, commands) = close_frame(&context, status, false, &mut pending_close);
            assert!(!stop);
            assert!(
                !commands
                    .iter()
                    .any(|command| matches!(command, egui::ViewportCommand::Close))
            );
        }
        let (stop, commands) =
            close_frame(&context, ServerStatus::Stopped, false, &mut pending_close);
        assert!(!stop);
        assert!(
            !commands
                .iter()
                .any(|command| matches!(command, egui::ViewportCommand::Close))
        );
        assert!(!pending_close);

        for status in [ServerStatus::Starting, ServerStatus::Running] {
            let (stop, commands) = close_frame(&context, status, false, &mut pending_close);
            assert!(!stop);
            assert!(commands.is_empty());
        }

        let context = egui::Context::default();
        let (stop, commands) =
            close_frame(&context, ServerStatus::Failed, false, &mut pending_close);
        assert!(!stop);
        assert!(
            !commands
                .iter()
                .any(|command| matches!(command, egui::ViewportCommand::Close))
        );
        pending_close = true;
        let (stop, commands) =
            close_frame(&context, ServerStatus::Failed, false, &mut pending_close);
        assert!(!stop);
        assert!(
            commands
                .iter()
                .any(|command| matches!(command, egui::ViewportCommand::Close))
        );
    }

    #[test]
    fn memory_graph_uses_the_entire_bounded_history_and_a_finite_scale() {
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(255.0, 40.0));
        let samples: VecDeque<_> = (1..=MEMORY_HISTORY_CAPACITY as u64)
            .map(|sample| sample * 1_048_576)
            .collect();
        let ceiling = memory_graph_ceiling(&samples);
        let points = memory_graph_points(rect, &samples, ceiling);
        assert_eq!(points.len(), MEMORY_HISTORY_CAPACITY);
        assert_eq!(points.first().unwrap().x, rect.left());
        assert_eq!(points.last().unwrap().x, rect.right());
        assert!(points.iter().all(|point| rect.contains(*point)));
        assert!(
            points
                .windows(2)
                .all(|pair| pair[0].x < pair[1].x && pair[0].y > pair[1].y)
        );

        for samples in [VecDeque::new(), VecDeque::from([0, 0])] {
            let ceiling = memory_graph_ceiling(&samples);
            assert!(ceiling.is_finite() && ceiling > 0.0);
            assert!(
                memory_graph_points(rect, &samples, ceiling)
                    .iter()
                    .all(|point| rect.contains(*point))
            );
        }
    }

    #[test]
    fn status_indicators_render_without_symbol_glyphs() {
        let context = egui::Context::default();
        configure_style(&context);
        let mut output = context.run_ui(egui::RawInput::default(), |ui| {
            server_controls(ui, ServerStatus::Running, false);
            ui.fonts_mut(|fonts| {
                for status in [
                    ServerStatus::Starting,
                    ServerStatus::Running,
                    ServerStatus::Stopping,
                    ServerStatus::Stopped,
                    ServerStatus::Failed,
                ] {
                    let (label, _) = status_label(status);
                    assert!(fonts.has_glyphs(&FontId::proportional(12.0), label));
                }
            });
        });
        // This headless test has no renderer to consume the font texture uploads.
        output.textures_delta.clear();
        assert!(output.shapes.iter().any(|shape| {
            matches!(&shape.shape, egui::Shape::Circle(circle) if circle.fill == GREEN)
        }));
    }

    #[test]
    fn command_history_restores_draft_and_stays_bounded() {
        let mut history = CommandHistory::default();
        for index in 0..HISTORY_CAPACITY + 2 {
            history.record(format!("say {index}"));
        }
        assert_eq!(history.entries.len(), HISTORY_CAPACITY);
        assert_eq!(history.entries.front().map(String::as_str), Some("say 2"));
        let mut command = "unfinished command".to_owned();
        history.previous(&mut command);
        assert_eq!(command, "say 65");
        history.previous(&mut command);
        assert_eq!(command, "say 64");
        history.next(&mut command);
        history.next(&mut command);
        assert_eq!(command, "unfinished command");
        history.next(&mut command);
        assert_eq!(command, "unfinished command");
        history.record("say 65".to_owned());
        assert_eq!(history.entries.len(), HISTORY_CAPACITY);
    }

    #[test]
    fn bounded_buffers_keep_the_latest_values() {
        let mut values = VecDeque::new();
        for value in 0..10 {
            push_bounded(&mut values, value, 3);
        }
        assert_eq!(values, VecDeque::from([7, 8, 9]));
    }

    #[test]
    fn close_waits_for_a_terminal_server_status() {
        assert!(!has_stopped(ServerStatus::Starting));
        assert!(!has_stopped(ServerStatus::Running));
        assert!(!has_stopped(ServerStatus::Stopping));
        assert!(has_stopped(ServerStatus::Stopped));
        assert!(has_stopped(ServerStatus::Failed));
    }
}
