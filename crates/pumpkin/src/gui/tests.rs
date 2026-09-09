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
    assert!(failed.drain_logs().is_empty());
}

#[test]
fn snapshots_are_owned_copies_and_finish_clears_online_state() {
    let handle = running_handle();
    {
        let mut snapshot = handle.state.snapshot.lock().unwrap();
        snapshot.players.push(GuiPlayer {
            id: uuid::Uuid::from_u128(1),
            name: "Alex".to_string(),
            edition: "Java".to_string(),
            is_op: true,
        });
        snapshot.tps = 20.0;
        snapshot.record_memory_sample(1234);
        snapshot.sample_id = 4;
    };
    let mut copy = handle.snapshot();
    assert!(copy.players[0].is_op);
    copy.players[0].is_op = false;
    assert!(handle.snapshot().players[0].is_op);
    copy.players.clear();
    copy.memory_history.clear();
    assert_eq!(handle.snapshot().players.len(), 1);
    assert_eq!(handle.snapshot().memory_history, VecDeque::from([1234]));
    handle.finish(None);
    let finished = handle.snapshot();
    assert!(finished.players.is_empty());
    assert_eq!(finished.tps, 0.0);
    assert_eq!(finished.sample_id, 4);
    assert_eq!(finished.memory_bytes, 1234);
    assert_eq!(finished.memory_history, VecDeque::from([1234]));
    assert!(!finished.commands_enabled);
}

#[test]
fn memory_history_retains_the_latest_256_samples_in_order_after_failure() {
    let handle = running_handle();
    {
        let mut snapshot = handle.state.snapshot.lock().unwrap();
        assert!(snapshot.memory_history.is_empty());
        for sample in 0..=300 {
            snapshot.record_memory_sample(sample);
        }
    }
    let snapshot = handle.snapshot();
    assert_eq!(snapshot.memory_bytes, 300);
    assert_eq!(snapshot.memory_history.len(), MEMORY_HISTORY_CAPACITY);
    assert!(snapshot.memory_history.iter().copied().eq(45..=300));
    handle.finish(Some("backend failed".to_string()));
    let finished = handle.snapshot();
    assert_eq!(finished.status, ServerStatus::Failed);
    assert_eq!(finished.memory_bytes, 300);
    assert_eq!(finished.memory_history, snapshot.memory_history);
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

#[test]
fn managed_restart_resets_session_state_after_draining_old_commands() {
    let (handle, desired) = GuiHandle::new_managed();
    let mut commands = handle.take_command_receiver().unwrap();
    assert!(handle.take_command_receiver().is_none());
    assert!(*desired.borrow());
    assert_eq!(handle.snapshot().uptime, Duration::ZERO);
    assert!(handle.request_start().is_err());

    let mut first = running_handle().snapshot();
    first.players.push(GuiPlayer {
        id: uuid::Uuid::from_u128(1),
        name: "Alex".to_string(),
        edition: "Java".to_string(),
        is_op: false,
    });
    first.record_memory_sample(1234);
    first.uptime = Duration::from_secs(42);
    first.sample_id = 84;
    first.tps = 19.0;
    first.mspt = 12.0;
    first.target_tps = 10.0;
    first.tick_frozen = true;
    handle.apply_backend_update(first, Vec::new());
    assert_eq!(handle.snapshot().uptime, Duration::from_secs(42));
    handle.submit_command("old command").unwrap();

    // The supervisor disables command admission before draining/reaping, then exposes Start.
    handle.request_stop();
    assert!(!*desired.borrow());
    assert!(handle.submit_command("late old command").is_err());
    assert_eq!(commands.try_recv().unwrap(), "old command");
    assert!(commands.try_recv().is_err());
    handle.push_log(Level::ERROR, "first backend failed");
    handle.finish(Some("first backend failed".to_string()));
    assert!(!*desired.borrow());
    assert_eq!(handle.snapshot().uptime, Duration::from_secs(42));
    assert_eq!(handle.snapshot().memory_history, VecDeque::from([1234]));

    handle.request_start().unwrap();
    assert!(*desired.borrow());
    let second = handle.snapshot();
    assert_eq!(second.status, ServerStatus::Starting);
    assert!(second.players.is_empty());
    assert!(second.memory_history.is_empty());
    assert_eq!(second.memory_bytes, 0);
    assert_eq!(second.sample_id, 0);
    assert_eq!(second.uptime, Duration::ZERO);
    assert_eq!(second.tps, 0.0);
    assert_eq!(second.mspt, 0.0);
    assert_eq!(second.target_tps, 20.0);
    assert!(!second.tick_frozen);
    assert!(second.last_error.is_none());
    assert!(!second.commands_enabled);
    assert!(handle.submit_command("too early").is_err());
    assert!(handle.request_start().is_err());
    let logs = handle.drain_logs();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].text, "first backend failed");

    handle.apply_backend_update(running_handle().snapshot(), Vec::new());
    handle.submit_command("new command").unwrap();
    assert_eq!(commands.try_recv().unwrap(), "new command");
    assert!(commands.try_recv().is_err());
    handle.request_stop();
    handle.finish(None);
    assert_eq!(handle.snapshot().status, ServerStatus::Stopped);
    assert!(!*desired.borrow());
    handle.request_start().unwrap();
    assert!(*desired.borrow());
}

#[test]
fn managed_terminal_updates_wait_for_reaping_and_cannot_undo_stop() {
    for terminal in [ServerStatus::Stopped, ServerStatus::Failed] {
        let (handle, _) = GuiHandle::new_managed();
        let mut update = running_handle().snapshot();
        update.status = terminal;
        update.uptime = Duration::from_secs(17);
        handle.apply_backend_update(update, Vec::new());
        assert_eq!(handle.snapshot().status, ServerStatus::Stopping);
        assert!(!handle.snapshot().commands_enabled);
        assert!(handle.request_start().is_err());
        assert!(handle.submit_command("list").is_err());

        handle.apply_backend_update(running_handle().snapshot(), Vec::new());
        assert_eq!(handle.snapshot().status, ServerStatus::Stopping);
        assert!(!handle.snapshot().commands_enabled);
        handle.finish(None);
        handle.apply_backend_update(running_handle().snapshot(), Vec::new());
        assert_eq!(handle.snapshot().status, ServerStatus::Stopped);
    }

    let (handle, _) = GuiHandle::new_managed();
    handle.request_stop();
    handle.apply_backend_update(running_handle().snapshot(), Vec::new());
    assert_eq!(handle.snapshot().status, ServerStatus::Stopping);
    assert!(!handle.snapshot().commands_enabled);
}

#[test]
fn restart_rejects_a_missing_supervisor_and_preserves_terminal_details() {
    for error in [None, Some("supervisor wait failed".to_string())] {
        let (handle, supervisor) = GuiHandle::new_managed();
        let mut update = running_handle().snapshot();
        update.record_memory_sample(4096);
        update.uptime = Duration::from_secs(12);
        handle.apply_backend_update(update, Vec::new());
        drop(supervisor);
        handle.finish(error);
        let before = serde_json::to_value(handle.snapshot()).unwrap();

        assert!(handle.request_start().unwrap_err().contains("supervisor"));
        assert_eq!(serde_json::to_value(handle.snapshot()).unwrap(), before);
        assert!(!*handle.state.desired_running.as_ref().unwrap().borrow());
        assert!(handle.submit_command("list").is_err());
    }
}

#[test]
fn window_close_requests_stop_and_prevents_restart_without_global_cancellation() {
    let (handle, desired) = GuiHandle::new_managed();
    assert!(!handle.close_requested());
    handle.request_close();
    assert!(handle.close_requested());
    assert!(!*desired.borrow());
    assert_eq!(handle.snapshot().status, ServerStatus::Stopping);
    handle.request_close();
    handle.finish(None);
    assert!(handle.request_start().is_err());
    assert!(!*desired.borrow());

    let local = GuiHandle::new();
    local.finish(None);
    assert!(local.request_start().is_err());
}

#[test]
fn backend_updates_drain_at_most_64_log_rows_in_order() {
    let handle = GuiHandle::new();
    for index in 0..MAX_UPDATE_LOGS + 3 {
        handle.push_log(Level::INFO, &format!("line {index}"));
    }
    let (_, first) = handle.backend_update();
    assert_eq!(first.len(), MAX_UPDATE_LOGS);
    assert_eq!(first[0].text, "line 0");
    assert_eq!(first.last().unwrap().text, "line 63");
    let (_, next) = handle.backend_update();
    assert_eq!(next.len(), 3);
    assert_eq!(next[0].text, "line 64");
    assert!(handle.backend_update().1.is_empty());
}

#[test]
fn mirrored_logs_and_memory_history_remain_bounded_and_sanitized() {
    let (handle, _) = GuiHandle::new_managed();
    let mut update = running_handle().snapshot();
    update.memory_history = (0..300).collect();
    let log = LogLine {
        timestamp: "\x1b[31m12:34:56\x1b[0m".to_string(),
        level: Level::WARN,
        text: format!("\x1b[31m{}\x1b[0m", "x".repeat(MAX_LOG_BYTES + 1)),
    };
    for _ in 0..=(MAX_LOG_LINES / MAX_UPDATE_LOGS) {
        handle.apply_backend_update(update.clone(), vec![log.clone(); MAX_UPDATE_LOGS]);
    }
    let snapshot = handle.snapshot();
    assert_eq!(snapshot.memory_history.len(), MEMORY_HISTORY_CAPACITY);
    assert!(snapshot.memory_history.iter().copied().eq(44..300));
    let logs = handle.drain_logs();
    assert_eq!(logs.len(), MAX_LOG_LINES);
    assert!(logs.iter().all(|line| {
        line.timestamp == "12:34:56"
            && line.level == Level::WARN
            && line.text.len() == MAX_LOG_BYTES
            && !line.text.contains('\x1b')
    }));
}

#[test]
fn snapshots_and_log_levels_round_trip_through_ipc_serialization() {
    let mut snapshot = running_handle().snapshot();
    snapshot.record_memory_sample(4096);
    snapshot.uptime = Duration::new(12, 345);
    snapshot.players.push(GuiPlayer {
        id: uuid::Uuid::from_u128(1),
        name: "Alex".to_string(),
        edition: "Bedrock".to_string(),
        is_op: true,
    });
    for level in [
        Level::TRACE,
        Level::DEBUG,
        Level::INFO,
        Level::WARN,
        Level::ERROR,
    ] {
        let update = (
            snapshot.clone(),
            vec![LogLine {
                timestamp: "12:34:56".to_string(),
                level,
                text: "A console reply".to_string(),
            }],
        );
        let encoded = serde_json::to_string(&update).unwrap();
        let decoded: (ServerSnapshot, Vec<LogLine>) = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.1[0].level, level);
        assert_eq!(decoded.0.uptime, snapshot.uptime);
        assert_eq!(decoded.0.memory_history, snapshot.memory_history);
        assert!(decoded.0.players[0].is_op);
        assert_eq!(serde_json::to_string(&decoded).unwrap(), encoded);
    }
    assert!(
        serde_json::from_str::<LogLine>(
            r#"{"timestamp":"12:34:56","level":"INVALID","text":"bad level"}"#,
        )
        .is_err()
    );
    assert!(
        serde_json::from_str::<GuiPlayer>(
            r#"{"id":"00000000-0000-0000-0000-000000000001","name":"Alex","edition":"Java"}"#,
        )
        .is_err()
    );
}

#[test]
fn operator_membership_uses_current_config_uuids_not_names_or_levels() {
    use pumpkin_config::op::Op;
    use pumpkin_util::permission::PermissionLvl;

    let id = uuid::Uuid::from_u128(1);
    let other_id = uuid::Uuid::from_u128(2);
    let mut ops = vec![Op::new(
        id,
        "Old Name".to_string(),
        PermissionLvl::Zero,
        false,
    )];
    assert_eq!(operator_ids(&ops), HashSet::from([id]));

    ops[0].name = "New Name".to_string();
    ops[0].level = PermissionLvl::Four;
    assert_eq!(operator_ids(&ops), HashSet::from([id]));

    // Reusing the old operator's name never grants membership to a different UUID.
    ops[0].uuid = other_id;
    assert_eq!(operator_ids(&ops), HashSet::from([other_id]));
    ops.clear();
    assert!(operator_ids(&ops).is_empty());
}
