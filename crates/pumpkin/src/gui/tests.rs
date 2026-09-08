use tracing_subscriber::prelude::*;

use super::*;

fn running_handle() -> GuiHandle {
    let handle = GuiHandle::new();
    {
        let mut snapshot = handle.state.snapshot.lock().unwrap();
        snapshot.status = ServerStatus::Running;
        snapshot.commands_enabled = true;
    };
    handle
}

#[test]
fn command_queue_is_bounded_and_validates_input() {
    let handle = running_handle();
    for invalid in ["", "  ", "say hello\nstop", "say hi\rstop", "say \0"] {
        assert!(handle.submit_command(invalid).is_err());
    }
    assert!(
        handle
            .submit_command(&"x".repeat(MAX_COMMAND_BYTES + 1))
            .is_err()
    );
    for _ in 0..MAX_COMMANDS {
        handle.submit_command("  say hello  ").unwrap();
    }
    assert!(handle.submit_command("overflow").is_err());

    let mut receiver = handle.state.command_receiver.lock().unwrap();
    let receiver = receiver.as_mut().unwrap();
    assert_eq!(receiver.try_recv().unwrap(), "say hello");
    handle.submit_command("now there is room").unwrap();
    assert!(handle.submit_command("overflow again").is_err());
    for _ in 1..MAX_COMMANDS {
        assert_eq!(receiver.try_recv().unwrap(), "say hello");
    }
    assert_eq!(receiver.try_recv().unwrap(), "now there is room");
    assert!(receiver.try_recv().is_err());
}

#[test]
fn commands_require_running_enabled_console_and_connected_receiver() {
    let starting = GuiHandle::new();
    assert!(starting.submit_command("list").is_err());

    let running = running_handle();
    running.state.snapshot.lock().unwrap().commands_enabled = false;
    assert!(running.submit_command("list").is_err());
    running.state.snapshot.lock().unwrap().commands_enabled = true;
    running.state.command_receiver.lock().unwrap().take();
    assert!(running.submit_command("list").is_err());
}

#[test]
fn shutdown_state_is_idempotent_without_cancelling_global_server() {
    let handle = running_handle();
    // Exercise the local state transition, not request_stop's process-wide cancellation.
    assert!(handle.begin_stop());
    assert!(!handle.begin_stop());
    assert_eq!(handle.snapshot().status, ServerStatus::Stopping);
    assert!(!handle.snapshot().commands_enabled);
    assert!(handle.submit_command("list").is_err());
    handle.finish(None);
    assert_eq!(handle.snapshot().status, ServerStatus::Stopped);
    assert!(!handle.begin_stop());

    let failed = GuiHandle::new();
    failed.finish(Some("\x1b[31mstartup failed\x1b[0m".to_string()));
    assert_eq!(failed.snapshot().status, ServerStatus::Failed);
    assert_eq!(
        failed.snapshot().last_error.as_deref(),
        Some("startup failed")
    );
    assert!(!failed.begin_stop());
    let logs = failed.drain_logs();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].level, Level::ERROR);
}

#[test]
fn snapshots_are_owned_copies_and_finish_clears_online_state() {
    let handle = running_handle();
    {
        let mut snapshot = handle.state.snapshot.lock().unwrap();
        snapshot.players.push(GuiPlayer {
            name: "Alex".to_string(),
            edition: "Java".to_string(),
        });
        snapshot.tps = 20.0;
        snapshot.sample_id = 4;
    };
    let mut copy = handle.snapshot();
    copy.players.clear();
    assert_eq!(handle.snapshot().players.len(), 1);
    handle.finish(None);
    let finished = handle.snapshot();
    assert!(finished.players.is_empty());
    assert_eq!(finished.tps, 0.0);
    assert_eq!(finished.sample_id, 4);
    assert!(!finished.commands_enabled);
}

#[test]
fn log_backlog_drops_oldest_and_drains_once() {
    let handle = GuiHandle::new();
    for index in 0..MAX_LOG_LINES + 3 {
        handle.push_log(Level::INFO, &format!("line {index}"));
    }
    let lines = handle.drain_logs();
    assert_eq!(lines.len(), MAX_LOG_LINES);
    assert_eq!(lines.first().unwrap().text, "line 3");
    assert_eq!(
        lines.last().unwrap().text,
        format!("line {}", MAX_LOG_LINES + 2)
    );
    assert!(!lines[0].timestamp.is_empty());
    assert!(handle.drain_logs().is_empty());
}

#[test]
fn log_sanitization_removes_terminal_sequences_and_bounds_utf8() {
    assert_eq!(
        sanitize_log_text("\x1b[31mred\x1b[0m\nline\tvalue\0"),
        "red\nline value"
    );
    assert_eq!(sanitize_log_text("a\x1b]0;hidden title\x07b"), "ab");
    assert_eq!(
        sanitize_log_text("a\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\b"),
        "alinkb"
    );
    assert_eq!(sanitize_log_text("visible\x1b[31"), "visible");
    let bounded = sanitize_log_text(&"🍊".repeat(MAX_LOG_BYTES));
    assert_eq!(bounded.len(), MAX_EVENT_BYTES);
    assert!(bounded.chars().all(|c| c == '🍊'));
    assert_eq!(sanitize_single_line("red\nline\rvalue"), "red line value");
    assert_eq!(sanitize_single_line(&bounded).len(), MAX_LOG_BYTES);
}

#[test]
fn multiline_logs_are_ordered_rows_with_shared_metadata_and_bounded_budget() {
    let handle = GuiHandle::new();
    let subscriber = tracing_subscriber::registry().with(GuiLogLayer::new(handle.clone()));
    tracing::subscriber::with_default(subscriber, || {
        tracing::error!("\x1b[31mfirst\x1b[0m\r\n    at frame\n\nlast");
    });
    let logs = handle.drain_logs();
    assert_eq!(
        logs.iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>(),
        ["first", "    at frame", "last"]
    );
    assert!(logs.iter().all(|line| {
        line.level == Level::ERROR
            && line.timestamp == logs[0].timestamp
            && !line.text.contains('\x1b')
            && !line.text.contains('\n')
    }));

    // The same path also handles raw console replies; many lines cannot evade the event cap.
    let row = format!("{}\n", "🍊".repeat(700));
    handle.push_log(Level::INFO, &row.repeat(8));
    let logs = handle.drain_logs();
    assert!(logs.len() > 1);
    assert!(logs.len() <= MAX_LOG_LINES);
    assert!(logs.iter().all(|line| line.text.len() <= MAX_LOG_BYTES));
    assert!(logs.iter().map(|line| line.text.len()).sum::<usize>() <= MAX_EVENT_BYTES);
}

#[test]
fn tracing_layer_captures_messages_fields_and_levels_without_global_install() {
    let handle = GuiHandle::new();
    let subscriber = tracing_subscriber::registry().with(GuiLogLayer::new(handle.clone()));
    tracing::subscriber::with_default(subscriber, || {
        tracing::warn!(player = "Alex", count = 7, "\x1b[31mTest warning\x1b[0m");
        tracing::info!("{}", "x".repeat(MAX_EVENT_BYTES * 2));
    });
    let logs = handle.drain_logs();
    assert_eq!(logs.len(), 2);
    assert_eq!(logs[0].level, Level::WARN);
    assert!(logs[0].text.contains("Test warning"));
    assert!(logs[0].text.contains("player="));
    assert!(logs[0].text.contains("Alex"));
    assert!(logs[0].text.contains("count=7"));
    assert!(!logs[0].text.contains('\x1b'));
    assert_eq!(logs[1].text.len(), MAX_LOG_BYTES);
}
