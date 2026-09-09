use std::{collections::VecDeque, sync::Arc};

use eframe::egui::{self, FontId, text::LayoutJob, text::TextFormat};
use pumpkin::gui::LogLine;

use super::{BACKGROUND, MUTED, TEXT, log_color};

const MAX_ROWS: usize = 2_000;
// Row storage, the cached document, and its laid-out text each stay below this
// limit, rather than retaining several megabytes of long lines in each copy.
const MAX_TEXT_BYTES: usize = 512 * 1_024;
const MAX_LINE_BYTES: usize = 4_096;
const ROW_HEIGHT: f32 = 18.0;

struct ConsoleLine {
    text: String,
    timestamp_end: usize,
    level_end: usize,
    level: tracing::Level,
    chars: usize,
}

impl From<LogLine> for ConsoleLine {
    fn from(line: LogLine) -> Self {
        let timestamp = bounded_prefix(&line.timestamp, 32);
        let message = bounded_prefix(&line.text, MAX_LINE_BYTES);
        let text = format!("{timestamp} {:5} {message}\n", line.level.as_str());
        Self {
            chars: text.chars().count(),
            timestamp_end: timestamp.len() + 1,
            level_end: timestamp.len() + 7,
            level: line.level,
            text,
        }
    }
}

/// One read-only text editor, so selections and copy include offscreen lines.
#[derive(Default)]
pub(super) struct ConsoleDocument {
    lines: VecDeque<ConsoleLine>,
    bytes: usize,
    layout: LayoutJob,
    dirty: bool,
    trimmed_chars: usize,
    trimmed_rows: usize,
}

impl ConsoleDocument {
    pub(super) fn append(&mut self, lines: impl IntoIterator<Item = LogLine>) {
        for line in lines {
            let line = ConsoleLine::from(line);
            self.bytes += line.text.len();
            self.lines.push_back(line);
            self.dirty = true;
            while self.lines.len() > MAX_ROWS || self.bytes > MAX_TEXT_BYTES {
                if let Some(line) = self.lines.pop_front() {
                    self.bytes -= line.text.len();
                    self.trimmed_chars += line.chars;
                    self.trimmed_rows += 1;
                }
            }
        }
    }

    pub(super) fn show(&mut self, ui: &mut egui::Ui, height: f32) {
        self.rebuild_layout();
        let id = document_id();
        let trimmed_chars = std::mem::take(&mut self.trimmed_chars);
        let trimmed_rows = std::mem::take(&mut self.trimmed_rows);
        if trimmed_chars > 0
            && let Some(mut state) = egui::text_edit::TextEditState::load(ui.ctx(), id)
        {
            state
                .cursor
                .set_char_range(state.cursor.char_range().map(|mut range| {
                    range.primary -= trimmed_chars;
                    range.secondary -= trimmed_chars;
                    range
                }));
            state.store(ui.ctx(), id);
        }

        // Let egui's font cache handle scale/font changes. Only formatting the
        // document is cached here; unchanged frames do not rebuild its sections.
        let galley = ui.fonts_mut(|fonts| fonts.layout_job(self.layout.clone()));
        let mut layouter =
            |_ui: &egui::Ui, _text: &dyn egui::TextBuffer, _width: f32| Arc::clone(&galley);
        let mut read_only = self.layout.text.as_str();
        egui::Frame::NONE
            .fill(BACKGROUND)
            .inner_margin(10)
            .corner_radius(6)
            .show(ui, |ui| {
                // ScrollArea first wraps its supplied salt before combining it with the UI ID.
                let scroll_id = ui.make_persistent_id(egui::IdSalt::new("console_document_scroll"));
                if trimmed_rows > 0
                    && let Some(mut state) = egui::scroll_area::State::load(ui.ctx(), scroll_id)
                {
                    state.offset.y = (state.offset.y - trimmed_rows as f32 * ROW_HEIGHT).max(0.0);
                    state.store(ui.ctx(), scroll_id);
                }
                egui::ScrollArea::both()
                    .id_salt("console_document_scroll")
                    .stick_to_bottom(true)
                    .animated(false)
                    .auto_shrink([false, false])
                    .max_height((height - 20.0).max(40.0))
                    .show(ui, |ui| {
                        let mut output = egui::TextEdit::multiline(&mut read_only)
                            .id(id)
                            .font(FontId::monospace(12.5))
                            .desired_width(f32::INFINITY)
                            .desired_rows(1)
                            .clip_text(false)
                            .frame(egui::Frame::NONE)
                            .margin(0)
                            .layouter(&mut layouter)
                            .show(ui);
                        // egui also snapshots immutable text into its undoer.
                        // Log documents have no edits to undo; do not retain old logs there.
                        output.state.clear_undoer();
                        output.state.store(ui.ctx(), id);
                    });
            });
    }

    fn rebuild_layout(&mut self) {
        if !self.dirty {
            return;
        }
        self.layout.clear();
        self.layout.keep_trailing_whitespace = true;
        for line in &self.lines {
            for (text, color) in [
                (&line.text[..line.timestamp_end], MUTED),
                (
                    &line.text[line.timestamp_end..line.level_end],
                    log_color(line.level),
                ),
                (&line.text[line.level_end..], TEXT),
            ] {
                self.layout.append(
                    text,
                    0.0,
                    TextFormat {
                        font_id: FontId::monospace(12.5),
                        color,
                        line_height: Some(ROW_HEIGHT),
                        ..Default::default()
                    },
                );
            }
        }
        self.dirty = false;
    }
}

fn document_id() -> egui::Id {
    egui::Id::new("pumpkin_server_console_document")
}

fn bounded_prefix(text: &str, max_bytes: usize) -> &str {
    let mut end = text.len().min(max_bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Key, Modifiers, OutputCommand, text::CCursor, text::CCursorRange};

    fn line(text: impl Into<String>) -> LogLine {
        LogLine {
            timestamp: "12:34:56".to_owned(),
            level: tracing::Level::INFO,
            text: text.into(),
        }
    }

    fn frame(
        context: &egui::Context,
        document: &mut ConsoleDocument,
        events: Vec<Event>,
    ) -> Vec<OutputCommand> {
        std::mem::take(
            &mut render_frame(context, document, events)
                .platform_output
                .commands,
        )
    }

    fn render_frame(
        context: &egui::Context,
        document: &mut ConsoleDocument,
        events: Vec<Event>,
    ) -> egui::FullOutput {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(500.0, 160.0),
            )),
            events,
            ..Default::default()
        };
        let mut output = context.run_ui(input, |ui| document.show(ui, 140.0));
        output.textures_delta.clear();
        output
    }

    fn painted_document_top(output: &egui::FullOutput) -> (f32, f32) {
        output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some((text.pos.y, shape.clip_rect.top())),
                _ => None,
            })
            .expect("the console document should be painted")
    }

    fn select_all_and_copy() -> Vec<Event> {
        let modifiers = Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        vec![
            Event::ModifiersChanged(modifiers),
            Event::Key {
                key: Key::A,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            },
            Event::Key {
                key: Key::A,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers,
            },
            Event::ModifiersChanged(Modifiers::NONE),
            Event::Copy,
        ]
    }

    fn copied(commands: &[OutputCommand]) -> &str {
        commands
            .iter()
            .find_map(|command| match command {
                OutputCommand::CopyText(text) => Some(text.as_str()),
                _ => None,
            })
            .expect("the focused document should emit clipboard text")
    }

    #[test]
    fn keyboard_copy_selects_offscreen_document_and_typing_cannot_edit_it() {
        let context = egui::Context::default();
        let mut document = ConsoleDocument::default();
        document.append((0..100).map(|index| line(format!("line {index}: hráč joined"))));
        frame(&context, &mut document, vec![]);
        context.memory_mut(|memory| memory.request_focus(document_id()));
        let commands = frame(&context, &mut document, select_all_and_copy());
        let original = document.layout.text.clone();
        assert_eq!(copied(&commands), original);
        assert_eq!(copied(&commands).lines().count(), 100);
        frame(
            &context,
            &mut document,
            vec![
                Event::Text("not a command".to_owned()),
                Event::Paste("nor pasted text".to_owned()),
            ],
        );
        let commands = frame(&context, &mut document, select_all_and_copy());
        assert_eq!(copied(&commands), original);
        assert_eq!(document.layout.text, original);
    }

    #[test]
    fn offscreen_selection_survives_new_logs_without_selecting_them() {
        let context = egui::Context::default();
        let mut document = ConsoleDocument::default();
        document.append((0..100).map(|index| line(format!("line {index}"))));
        frame(&context, &mut document, vec![]);
        context.memory_mut(|memory| memory.request_focus(document_id()));
        let commands = frame(&context, &mut document, select_all_and_copy());
        let selected = copied(&commands).to_owned();
        document.append([line("new log after selection")]);
        let commands = frame(&context, &mut document, vec![Event::Copy]);
        assert_eq!(copied(&commands), selected);
        assert!(document.layout.text.ends_with("new log after selection\n"));
    }

    #[test]
    fn bottom_follows_logs_but_wheel_scrollback_keeps_its_anchor_through_eviction() {
        let context = egui::Context::default();
        let mut document = ConsoleDocument::default();
        document.append((0..MAX_ROWS - 2).map(|index| line(format!("line {index}"))));
        frame(&context, &mut document, vec![]);
        let (initial_top, _) = painted_document_top(&render_frame(&context, &mut document, vec![]));
        document.append([
            line("following the first new log"),
            line("following the second new log"),
        ]);
        frame(&context, &mut document, vec![]);
        let (bottom_top, _) = painted_document_top(&render_frame(&context, &mut document, vec![]));
        assert!(
            (bottom_top - initial_top + 2.0 * ROW_HEIGHT).abs() < 0.5,
            "the viewport must follow appended rows while at the bottom"
        );

        frame(
            &context,
            &mut document,
            vec![
                Event::PointerMoved(egui::pos2(100.0, 60.0)),
                Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, 360.0),
                    phase: egui::TouchPhase::Move,
                    modifiers: Modifiers::NONE,
                },
            ],
        );
        // Advance egui's wheel smoothing without sleeping or changing scroll state directly.
        for _ in 0..30 {
            frame(&context, &mut document, vec![]);
        }
        let (paused_top, clip_top) =
            painted_document_top(&render_frame(&context, &mut document, vec![]));
        assert!(
            paused_top > bottom_top + ROW_HEIGHT,
            "wheel input must actually scroll back"
        );
        let anchor_index = ((clip_top - paused_top) / ROW_HEIGHT).ceil() as usize;
        assert!(anchor_index > 0 && anchor_index < document.lines.len());
        let anchor_text = document.lines[anchor_index].text.clone();
        let anchor_y = paused_top + anchor_index as f32 * ROW_HEIGHT;

        document.append([line("appending this evicts the oldest retained row")]);
        frame(&context, &mut document, vec![]);
        let (trimmed_top, _) = painted_document_top(&render_frame(&context, &mut document, vec![]));
        assert_eq!(document.lines.len(), MAX_ROWS);
        assert_eq!(document.lines[anchor_index - 1].text, anchor_text);
        let retained_anchor_y = trimmed_top + (anchor_index - 1) as f32 * ROW_HEIGHT;
        assert!(
            (retained_anchor_y - anchor_y).abs() < 0.5,
            "the same visible row must stay in place while scrollback is paused: initial_top={initial_top}, bottom_top={bottom_top}, paused_top={paused_top}, trimmed_top={trimmed_top}, anchor_index={anchor_index}, anchor_y={anchor_y}, retained_anchor_y={retained_anchor_y}"
        );
    }

    #[test]
    fn trimming_keeps_unicode_selection_and_enforces_row_and_byte_limits() {
        let context = egui::Context::default();
        let mut document = ConsoleDocument::default();
        document.append((0..MAX_ROWS).map(|index| line(format!("hráč {index}"))));
        frame(&context, &mut document, vec![]);
        context.memory_mut(|memory| memory.request_focus(document_id()));
        let removed_chars = document.lines[0].chars;
        let start = removed_chars + document.lines[1].chars;
        let end = start + document.lines[2].chars;
        let expected = document.lines[2].text.clone();
        let mut state = egui::text_edit::TextEditState::load(&context, document_id())
            .expect("rendered text state");
        state.cursor.set_char_range(Some(CCursorRange::two(
            CCursor::new(start),
            CCursor::new(end),
        )));
        state.store(&context, document_id());
        document.append([line("one more row")]);
        let commands = frame(&context, &mut document, vec![Event::Copy]);
        assert_eq!(copied(&commands), expected);
        assert_eq!(document.lines.len(), MAX_ROWS);
        let state = egui::text_edit::TextEditState::load(&context, document_id())
            .expect("retained text state");
        let range = state
            .cursor
            .char_range()
            .expect("retained selection")
            .as_sorted_char_range();
        assert_eq!(range.start.0, start - removed_chars);
        assert_eq!(range.end.0, end - removed_chars);

        document.append((0..MAX_ROWS).map(|index| line(format!("{index} {}", "🎃".repeat(1_025)))));
        let commands = frame(&context, &mut document, select_all_and_copy());
        assert!(document.bytes <= MAX_TEXT_BYTES);
        assert!(document.lines.len() < MAX_ROWS);
        assert_eq!(copied(&commands), document.layout.text);
        assert_eq!(document.layout.text.len(), document.bytes);
        assert!(document.layout.text.contains("1999 "));
        assert!(
            document
                .lines
                .iter()
                .all(|line| line.text.is_char_boundary(line.text.len()))
        );
    }
}
