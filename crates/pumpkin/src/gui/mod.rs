//! A bounded, toolkit-independent bridge between the server and its native GUI.

pub mod player_actions;
pub mod process;

use std::collections::{HashSet, VecDeque};
use std::fmt::{self, Write as _};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
use tokio::sync::{mpsc, watch};
use tracing::{Level, Subscriber};
use tracing_subscriber::Layer;

use crate::net::ClientPlatform;
use crate::server::Server;

const MAX_COMMANDS: usize = 32;
const MAX_COMMAND_BYTES: usize = 4096;
const MAX_LOG_LINES: usize = 2000;
const MAX_LOG_BYTES: usize = 4096;
const MAX_EVENT_BYTES: usize = MAX_LOG_BYTES * 2;
const MAX_UPDATE_LOGS: usize = 64;
pub const MEMORY_HISTORY_CAPACITY: usize = 256;
pub const MEMORY_SAMPLE_INTERVAL: Duration = Duration::from_millis(500);

static GUI: OnceLock<GuiHandle> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerStatus {
    Starting,
    Running,
    Stopping,
    Stopped,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GuiPlayer {
    pub id: uuid::Uuid,
    pub name: String,
    pub edition: String,
    pub is_op: bool,
}

fn operator_ids(ops: &[pumpkin_config::op::Op]) -> HashSet<uuid::Uuid> {
    ops.iter().map(|op| op.uuid).collect()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServerSnapshot {
    pub status: ServerStatus,
    pub version: String,
    pub world_name: String,
    pub java_address: Option<String>,
    pub bedrock_address: Option<String>,
    pub players: Vec<GuiPlayer>,
    pub max_players: usize,
    pub tps: f64,
    pub mspt: f64,
    pub target_tps: f64,
    pub tick_frozen: bool,
    pub memory_bytes: u64,
    /// Recent process RSS samples, oldest first, collected by the backend every 500 ms.
    pub memory_history: VecDeque<u64>,
    pub uptime: Duration,
    pub sample_id: u64,
    pub last_error: Option<String>,
    pub commands_enabled: bool,
}

impl ServerSnapshot {
    fn starting() -> Self {
        Self {
            status: ServerStatus::Starting,
            version: env!("CARGO_PKG_VERSION").to_string(),
            world_name: String::new(),
            java_address: None,
            bedrock_address: None,
            players: Vec::new(),
            max_players: 0,
            tps: 0.0,
            mspt: 0.0,
            target_tps: 20.0,
            tick_frozen: false,
            memory_bytes: 0,
            memory_history: VecDeque::new(),
            uptime: Duration::ZERO,
            sample_id: 0,
            last_error: None,
            commands_enabled: false,
        }
    }

    fn record_memory_sample(&mut self, memory_bytes: u64) {
        self.memory_bytes = memory_bytes;
        if self.memory_history.len() == MEMORY_HISTORY_CAPACITY {
            self.memory_history.pop_front();
        }
        self.memory_history.push_back(memory_bytes);
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogLine {
    pub timestamp: String,
    #[serde(with = "log_level")]
    pub level: Level,
    pub text: String,
}

#[derive(Debug)]
struct GuiState {
    snapshot: Mutex<ServerSnapshot>,
    logs: Mutex<VecDeque<LogLine>>,
    commands: mpsc::Sender<String>,
    command_receiver: Mutex<Option<mpsc::Receiver<String>>>,
    desired_running: Option<watch::Sender<bool>>,
    close_requested: AtomicBool,
    started: Instant,
    utc_offset: time::UtcOffset,
}

#[derive(Clone, Debug)]
pub struct GuiHandle {
    state: Arc<GuiState>,
}

impl Default for GuiHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl GuiHandle {
    #[must_use]
    pub fn new() -> Self {
        Self::with_manager(None)
    }

    /// Creates a persistent supervisor mirror, initially requesting its first backend.
    #[must_use]
    pub fn new_managed() -> (Self, watch::Receiver<bool>) {
        let (desired_running, receiver) = watch::channel(true);
        (Self::with_manager(Some(desired_running)), receiver)
    }

    fn with_manager(desired_running: Option<watch::Sender<bool>>) -> Self {
        let (commands, command_receiver) = mpsc::channel(MAX_COMMANDS);
        Self {
            state: Arc::new(GuiState {
                snapshot: Mutex::new(ServerSnapshot::starting()),
                logs: Mutex::new(VecDeque::new()),
                commands,
                command_receiver: Mutex::new(Some(command_receiver)),
                desired_running,
                close_requested: AtomicBool::new(false),
                started: Instant::now(),
                utc_offset: time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC),
            }),
        }
    }

    pub fn install(self) -> Result<(), Self> {
        GUI.set(self)
    }

    #[must_use]
    pub fn snapshot(&self) -> ServerSnapshot {
        let mut snapshot = self
            .state
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if self.state.desired_running.is_none()
            && !matches!(
                snapshot.status,
                ServerStatus::Stopped | ServerStatus::Failed
            )
        {
            snapshot.uptime = self.state.started.elapsed();
        }
        snapshot
    }

    pub fn drain_logs(&self) -> Vec<LogLine> {
        self.state
            .logs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain(..)
            .collect()
    }

    /// Returns a bounded IPC batch while retaining later log rows for the next update.
    pub fn backend_update(&self) -> (ServerSnapshot, Vec<LogLine>) {
        let snapshot = self.snapshot();
        let mut logs = self
            .state
            .logs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let count = logs.len().min(MAX_UPDATE_LOGS);
        (snapshot, logs.drain(..count).collect())
    }

    /// Mirrors the current child. The supervisor must stop applying that child's
    /// updates before calling `finish`, and reap it before permitting another Start.
    pub fn apply_backend_update(&self, mut update: ServerSnapshot, logs: Vec<LogLine>) {
        {
            let mut snapshot = self
                .state
                .snapshot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !matches!(
                snapshot.status,
                ServerStatus::Stopped | ServerStatus::Failed
            ) {
                if snapshot.status == ServerStatus::Stopping
                    || matches!(update.status, ServerStatus::Stopped | ServerStatus::Failed)
                {
                    update.status = ServerStatus::Stopping;
                }
                if update.status != ServerStatus::Running {
                    update.commands_enabled = false;
                }
                while update.memory_history.len() > MEMORY_HISTORY_CAPACITY {
                    update.memory_history.pop_front();
                }
                *snapshot = update;
            }
        }
        for log in logs.into_iter().take(MAX_UPDATE_LOGS) {
            self.append_log(&sanitize_single_line(&log.timestamp), log.level, &log.text);
        }
    }

    pub fn submit_command(&self, command: &str) -> Result<(), String> {
        // Serialize enqueueing with Stop/finish so the supervisor can drain old
        // commands before exposing the terminal state and enabling another Start.
        let snapshot = self
            .state
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if snapshot.status != ServerStatus::Running {
            return Err("The server is not accepting commands.".to_string());
        }
        if !snapshot.commands_enabled {
            return Err("The server console is disabled in configuration.".to_string());
        }
        if command.len() > MAX_COMMAND_BYTES {
            return Err(format!(
                "Commands are limited to {MAX_COMMAND_BYTES} bytes."
            ));
        }
        if command.chars().any(|c| c.is_control() && c != '\t') {
            return Err("Enter one command at a time, without control characters.".to_string());
        }
        let command = command.trim();
        if command.is_empty() {
            return Err("Enter a command first.".to_string());
        }
        self.state
            .commands
            .try_send(command.to_string())
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => {
                    "The command queue is full; wait for pending commands.".to_string()
                }
                mpsc::error::TrySendError::Closed(_) => {
                    "The server is no longer accepting commands.".to_string()
                }
            })
    }

    /// Requests graceful shutdown once, never the force-exit path.
    pub fn request_stop(&self) {
        if self.begin_stop() && self.state.desired_running.is_none() {
            crate::stop_server();
        }
    }

    pub fn request_close(&self) {
        self.state.close_requested.store(true, Ordering::Release);
        self.request_stop();
    }

    #[must_use]
    pub fn close_requested(&self) -> bool {
        self.state.close_requested.load(Ordering::Acquire)
    }

    /// Requests a fresh backend only after the supervisor has reaped the previous one.
    pub fn request_start(&self) -> Result<(), String> {
        let Some(desired_running) = &self.state.desired_running else {
            return Err("This server instance cannot be restarted in place.".to_string());
        };
        let mut snapshot = self
            .state
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.close_requested() {
            return Err("The server window is closing.".to_string());
        }
        if !matches!(
            snapshot.status,
            ServerStatus::Stopped | ServerStatus::Failed
        ) {
            return Err("Wait for the server to finish stopping before starting it.".to_string());
        }
        if desired_running.is_closed() {
            return Err(
                "The server supervisor is no longer available. Reopen the window to start again."
                    .to_string(),
            );
        }
        // Unlike send_replace, send also rejects a supervisor that disappeared
        // after the closed check, without discarding the previous terminal snapshot.
        desired_running.send(true).map_err(|_| {
            "The server supervisor is no longer available. Reopen the window to start again."
                .to_string()
        })?;
        *snapshot = ServerSnapshot::starting();
        Ok(())
    }

    fn begin_stop(&self) -> bool {
        let mut snapshot = self
            .state
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !matches!(
            snapshot.status,
            ServerStatus::Starting | ServerStatus::Running
        ) {
            return false;
        }
        snapshot.status = ServerStatus::Stopping;
        snapshot.commands_enabled = false;
        if let Some(desired_running) = &self.state.desired_running {
            desired_running.send_replace(false);
        }
        true
    }

    /// Taken once by either the local server worker or the process supervisor.
    /// A supervisor must request Stop, then drain pending commands before `finish`
    /// exposes a terminal state, including when the child exits unexpectedly.
    pub fn take_command_receiver(&self) -> Option<mpsc::Receiver<String>> {
        self.state
            .command_receiver
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    /// Attaches once, after server startup. Both tracked workers terminate before world saving.
    pub fn attach_server(&self, server: &Arc<Server>) {
        let Some(mut commands) = self.take_command_receiver() else {
            return;
        };
        {
            let mut snapshot = self
                .state
                .snapshot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if matches!(
                snapshot.status,
                ServerStatus::Stopped | ServerStatus::Failed
            ) {
                return;
            }
            let networking = &server.advanced_config.networking;
            snapshot
                .world_name
                .clone_from(&server.basic_config.default_level_name);
            snapshot.java_address = networking
                .java
                .enabled
                .then(|| networking.java.address.to_string());
            snapshot.bedrock_address = (networking.bedrock.enabled
                && networking.bedrock.nethernet.enabled)
                .then(|| networking.bedrock.nethernet.address.to_string());
            snapshot.max_players = if networking.java.enabled {
                networking.java.max_players as usize
            } else if networking.bedrock.enabled {
                networking.bedrock.max_players as usize
            } else {
                0
            };
            snapshot.target_tps = f64::from(server.tick_rate_manager.tickrate());
            if snapshot.status == ServerStatus::Starting && !crate::STOP_INTERRUPT.is_cancelled() {
                snapshot.status = ServerStatus::Running;
                snapshot.commands_enabled = server.advanced_config.commands.use_console;
            } else {
                snapshot.status = ServerStatus::Stopping;
            }
        }

        let handle = self.clone();
        let command_server = server.clone();
        server.spawn_task(async move {
            loop {
                let command = tokio::select! {
                    biased;
                    () = crate::STOP_INTERRUPT.cancelled() => break,
                    command = commands.recv() => match command {
                        Some(command) => command,
                        None => break,
                    },
                };
                if handle.snapshot().commands_enabled {
                    handle.push_log(Level::INFO, &format!("> {command}"));
                    crate::dispatch_console_command(&command_server, &command).await;
                }
            }
            handle.begin_stop();
        });

        let handle = self.clone();
        let snapshot_server = server.clone();
        server.spawn_task(async move {
            let mut interval = tokio::time::interval(MEMORY_SAMPLE_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut system = System::new();
            let pid = sysinfo::get_current_pid().ok();
            loop {
                tokio::select! {
                    biased;
                    () = crate::STOP_INTERRUPT.cancelled() => break,
                    _ = interval.tick() => {},
                }
                let memory_bytes = pid.and_then(|pid| {
                    system.refresh_processes_specifics(
                        ProcessesToUpdate::Some(&[pid]),
                        true,
                        ProcessRefreshKind::nothing().with_memory().without_tasks(),
                    );
                    system.process(pid).map(sysinfo::Process::memory)
                });
                handle.update_snapshot(&snapshot_server, memory_bytes);
            }
            handle.begin_stop();
        });
    }

    fn update_snapshot(&self, server: &Server, memory_bytes: Option<u64>) {
        let operators = {
            let config = server
                .data
                .operator_config
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            operator_ids(&config.ops)
        };
        let mut players: Vec<_> = server
            .get_all_players()
            .iter()
            .map(|player| GuiPlayer {
                id: player.gameprofile.id,
                name: sanitize_single_line(&player.gameprofile.name),
                edition: match player.client.as_ref() {
                    ClientPlatform::Java(_) => "Java",
                    ClientPlatform::Bedrock(_) => "Bedrock",
                }
                .to_string(),
                is_op: operators.contains(&player.gameprofile.id),
            })
            .collect();
        players.sort_unstable_by(|a, b| a.name.cmp(&b.name).then(a.edition.cmp(&b.edition)));
        let mut snapshot = self
            .state
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(
            snapshot.status,
            ServerStatus::Stopped | ServerStatus::Failed
        ) {
            return;
        }
        snapshot.players = players;
        snapshot.target_tps = f64::from(server.tick_rate_manager.tickrate());
        snapshot.mspt = server.get_mspt().max(0.0);
        snapshot.tps = server.get_tps().clamp(0.0, snapshot.target_tps);
        snapshot.tick_frozen = server.tick_rate_manager.is_frozen();
        // Keep the last successful RSS sample if process statistics are temporarily unavailable.
        if let Some(memory_bytes) = memory_bytes {
            snapshot.record_memory_sample(memory_bytes);
        }
        snapshot.uptime = self.state.started.elapsed();
        snapshot.sample_id = snapshot.sample_id.saturating_add(1);
    }

    /// Marks backend completion. Normal completion follows the save/shutdown sequence;
    /// an error reports backend failure and does not imply that saving completed.
    /// Error summaries belong to the status display, not the server's console log.
    pub fn finish(&self, error: Option<String>) {
        let mut snapshot = self
            .state
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(desired_running) = &self.state.desired_running {
            desired_running.send_replace(false);
        }
        snapshot.status = if error.is_some() {
            ServerStatus::Failed
        } else {
            ServerStatus::Stopped
        };
        snapshot.last_error = error.map(|error| sanitize_single_line(&error));
        snapshot.commands_enabled = false;
        snapshot.players.clear();
        snapshot.tps = 0.0;
        if self.state.desired_running.is_none() {
            snapshot.uptime = self.state.started.elapsed();
        }
    }

    pub fn push_log(&self, level: Level, text: &str) {
        let now = time::OffsetDateTime::now_utc().to_offset(self.state.utc_offset);
        let timestamp = format!("{:02}:{:02}:{:02}", now.hour(), now.minute(), now.second());
        self.append_log(&timestamp, level, text);
    }

    fn append_log(&self, timestamp: &str, level: Level, text: &str) {
        let text = sanitize_log_text(text);
        if text.is_empty() {
            return;
        }
        let mut logs = self
            .state
            .logs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            if logs.len() == MAX_LOG_LINES {
                logs.pop_front();
            }
            logs.push_back(LogLine {
                timestamp: timestamp.to_string(),
                level,
                text: bounded_prefix(line.trim_end(), MAX_LOG_BYTES).to_string(),
            });
        }
    }
}

mod log_level {
    use serde::{Deserialize, Deserializer, Serializer};
    use tracing::Level;

    // Serde's `with` adapter requires a borrowed field.
    #[allow(clippy::trivially_copy_pass_by_ref)]
    pub fn serialize<S: Serializer>(level: &Level, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(level.as_str())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Level, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

#[must_use]
pub fn active() -> Option<&'static GuiHandle> {
    GUI.get()
}

pub fn capture_console_reply(text: &str) {
    if let Some(gui) = active() {
        gui.push_log(Level::INFO, text);
    }
}

#[derive(Clone)]
pub struct GuiLogLayer {
    handle: GuiHandle,
}

impl GuiLogLayer {
    #[must_use]
    pub const fn new(handle: GuiHandle) -> Self {
        Self { handle }
    }
}

impl<S: Subscriber> Layer<S> for GuiLogLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = LogVisitor::default();
        event.record(&mut visitor);
        if !visitor.fields.0.is_empty() {
            let _ = write!(visitor.message, " {}", visitor.fields.0);
        }
        self.handle
            .push_log(*event.metadata().level(), &visitor.message.0);
    }
}

#[derive(Default)]
struct LimitedText(String);

impl fmt::Write for LimitedText {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let mut end = text.len().min(MAX_EVENT_BYTES.saturating_sub(self.0.len()));
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        self.0.push_str(&text[..end]);
        Ok(())
    }
}

#[derive(Default)]
struct LogVisitor {
    message: LimitedText,
    fields: LimitedText,
}

impl tracing::field::Visit for LogVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.fields, "{}={value:?} ", field.name());
        }
    }
}

/// Drops terminal escape/control sequences while preserving lines within a bounded event.
fn sanitize_log_text(text: &str) -> String {
    let mut result = String::new();
    let mut chars = text.chars().take(MAX_EVENT_BYTES);
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.next() {
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']' | 'P' | '^' | '_') => {
                    let mut escape = false;
                    for c in chars.by_ref() {
                        if c == '\x07' || (escape && c == '\\') {
                            break;
                        }
                        escape = c == '\x1b';
                    }
                }
                _ => {}
            }
            continue;
        }
        let c = match c {
            '\n' | '\r' => '\n',
            '\t' => ' ',
            c if c.is_control() => continue,
            c => c,
        };
        if result.len() + c.len_utf8() > MAX_EVENT_BYTES {
            break;
        }
        result.push(c);
    }
    result
}

fn bounded_prefix(text: &str, max_bytes: usize) -> &str {
    let mut end = text.len().min(max_bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn sanitize_single_line(text: &str) -> String {
    let text = sanitize_log_text(text).replace('\n', " ");
    bounded_prefix(text.trim(), MAX_LOG_BYTES).to_string()
}

#[cfg(test)]
mod tests;
