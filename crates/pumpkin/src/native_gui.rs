use std::{collections::VecDeque, time::Duration};

use eframe::egui::{self, Align, Color32, FontId, RichText, Stroke, Vec2};
use pumpkin::gui::{GuiHandle, LogLine, ServerSnapshot, ServerStatus};

const BACKGROUND: Color32 = Color32::from_rgb(19, 21, 24);
const SURFACE: Color32 = Color32::from_rgb(27, 30, 34);
const BORDER: Color32 = Color32::from_rgb(47, 51, 57);
const TEXT: Color32 = Color32::from_rgb(232, 234, 237);
const MUTED: Color32 = Color32::from_rgb(146, 154, 166);
const ORANGE: Color32 = Color32::from_rgb(245, 148, 66);
const GREEN: Color32 = Color32::from_rgb(124, 202, 153);
const RED: Color32 = Color32::from_rgb(244, 131, 131);
const LOG_CAPACITY: usize = 2_000;
const HISTORY_CAPACITY: usize = 64;
const SAMPLE_CAPACITY: usize = 90;

pub fn run(handle: GuiHandle) -> eframe::Result {
    let icon = image::load_from_memory(include_bytes!("../../../assets/default_icon.png"))
        .ok()
        .map(image::DynamicImage::into_rgba8);
    let mut viewport = egui::ViewportBuilder::default()
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
                logs: VecDeque::new(),
                history: CommandHistory::default(),
                command: String::new(),
                command_error: None,
                samples: VecDeque::new(),
                last_sample: None,
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
    logs: VecDeque<LogLine>,
    history: CommandHistory,
    command: String,
    command_error: Option<String>,
    samples: VecDeque<f64>,
    last_sample: Option<u64>,
    close_when_stopped: bool,
}

impl eframe::App for ServerApp {
    fn logic(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        for line in self.handle.drain_logs() {
            push_bounded(&mut self.logs, line, LOG_CAPACITY);
        }
        let snapshot = self.handle.snapshot();
        if self.last_sample != Some(snapshot.sample_id) {
            self.last_sample = Some(snapshot.sample_id);
            if snapshot.status == ServerStatus::Running {
                push_bounded(&mut self.samples, snapshot.tps, SAMPLE_CAPACITY);
            }
        }
        if context.input(|input| input.viewport().close_requested())
            && !has_stopped(snapshot.status)
        {
            context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if !self.close_when_stopped {
                self.close_when_stopped = true;
                self.handle.request_stop();
            }
        }
        if self.close_when_stopped && has_stopped(snapshot.status) {
            context.send_viewport_cmd(egui::ViewportCommand::Close);
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
    }
}

impl ServerApp {
    fn header(&self, ui: &mut egui::Ui, snapshot: &ServerSnapshot) {
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
                let can_stop = matches!(
                    snapshot.status,
                    ServerStatus::Starting | ServerStatus::Running
                );
                if ui
                    .add_enabled(can_stop, egui::Button::new("Stop server"))
                    .on_hover_text("Save the worlds and shut down safely")
                    .clicked()
                {
                    self.handle.request_stop();
                }
                let (label, color) = status_label(snapshot.status);
                egui::Frame::NONE
                    .fill(color.gamma_multiply(0.12))
                    .corner_radius(20)
                    .inner_margin(egui::Margin::symmetric(12, 6))
                    .show(ui, |ui| {
                        ui.label(RichText::new(format!("●  {label}")).color(color).size(12.0));
                    });
            });
        });
    }

    fn sidebar(&self, ui: &mut egui::Ui, snapshot: &ServerSnapshot) {
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
                        ui.label(RichText::new(format!("{:.1}", snapshot.mspt)).size(28.0));
                        ui.label(RichText::new("ms / tick").size(11.0).color(MUTED));
                    });
                });
            });
            self.tick_graph(ui, snapshot.target_tps);
            if snapshot.tick_frozen {
                ui.label(RichText::new("Ticking is frozen").small().color(ORANGE));
            }
            ui.separator();
            detail_row(
                ui,
                "Memory",
                &format!("{:.0} MiB", snapshot.memory_bytes as f64 / 1_048_576.0),
            );
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
                ui.spacing_mut().interact_size.y = 22.0;
                egui::ScrollArea::vertical()
                    .id_salt("players")
                    .max_height((height - 64.0).max(24.0))
                    .show_rows(ui, 22.0, snapshot.players.len(), |ui, range| {
                        for index in range {
                            let player = &snapshot.players[index];
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("●").color(GREEN));
                                ui.add(egui::Label::new(&player.name).truncate());
                                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                    ui.label(
                                        RichText::new(&player.edition).size(11.0).color(MUTED),
                                    );
                                });
                            });
                        }
                    });
            }
        });
    }

    fn tick_graph(&self, ui: &mut egui::Ui, target_tps: f64) {
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 40.0), egui::Sense::hover());
        let painter = ui.painter_at(rect);
        for fraction in [0.0, 0.5, 1.0] {
            let y = rect.bottom() - rect.height() * fraction;
            painter.line_segment(
                [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
                Stroke::new(1.0, BORDER),
            );
        }
        if self.samples.len() > 1 {
            let ceiling = target_tps.max(1.0);
            let points = self
                .samples
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    egui::pos2(
                        rect.right()
                            - (self.samples.len() - 1 - index) as f32 * rect.width()
                                / (SAMPLE_CAPACITY - 1) as f32,
                        rect.bottom() - (value / ceiling).clamp(0.0, 1.0) as f32 * rect.height(),
                    )
                })
                .collect();
            painter.add(egui::Shape::line(points, Stroke::new(1.5, GREEN)));
        }
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
            let footer_height = if snapshot.last_error.is_some() || self.command_error.is_some() {
                110.0
            } else {
                72.0
            };
            let log_height = (ui.available_height() - footer_height).max(60.0);
            egui::Frame::NONE
                .fill(BACKGROUND)
                .inner_margin(10)
                .corner_radius(6)
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(8.0, 3.0);
                    ui.spacing_mut().interact_size.y = 18.0;
                    egui::ScrollArea::both()
                        .id_salt("server_logs")
                        .stick_to_bottom(true)
                        .auto_shrink([false, false])
                        .max_height(log_height - 20.0)
                        .show_rows(ui, 18.0, self.logs.len(), |ui, range| {
                            for index in range {
                                let line = &self.logs[index];
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(&line.timestamp)
                                            .monospace()
                                            .size(11.0)
                                            .color(MUTED),
                                    );
                                    ui.label(
                                        RichText::new(format!("{:5}", line.level.as_str()))
                                            .monospace()
                                            .size(11.0)
                                            .color(log_color(line.level)),
                                    );
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(&line.text).monospace().size(12.5),
                                        )
                                        .wrap_mode(egui::TextWrapMode::Extend)
                                        .selectable(true),
                                    );
                                });
                            }
                        });
                });
            ui.add_space(3.0);
            self.command_input(ui, snapshot.commands_enabled);
            if let Some(error) = self.command_error.as_ref().or(snapshot.last_error.as_ref()) {
                ui.add(egui::Label::new(RichText::new(error).small().color(RED)).truncate());
            }
            let hint = if self.close_when_stopped || snapshot.status == ServerStatus::Stopping {
                "Saving worlds and stopping safely…"
            } else if has_stopped(snapshot.status) {
                "Server stopped. You can close this window."
            } else if snapshot.status == ServerStatus::Starting {
                "Starting the server…"
            } else if !snapshot.commands_enabled {
                "The server console is disabled in configuration."
            } else {
                "Enter to send  ·  ↑ ↓ command history  ·  type help for commands"
            };
            ui.label(RichText::new(hint).size(11.0).color(MUTED));
        });
    }

    fn command_input(&mut self, ui: &mut egui::Ui, enabled: bool) {
        ui.add_enabled_ui(enabled, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(">").monospace().color(ORANGE));
                let response = ui.add_sized(
                    Vec2::new((ui.available_width() - 72.0).max(40.0), 34.0),
                    egui::TextEdit::singleline(&mut self.command)
                        .font(egui::TextStyle::Monospace)
                        .hint_text("Enter a server command…")
                        .char_limit(4_096),
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
