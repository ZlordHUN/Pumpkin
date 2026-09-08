#![cfg(all(
    feature = "gui",
    any(target_os = "linux", target_os = "windows", target_os = "macos")
))]

use std::{io, path::PathBuf, time::Duration};

use pumpkin::gui::{GuiHandle, ServerStatus, process::run_supervisor};
use pumpkin_config::{LoadConfiguration, PumpkinConfig};
use tempfile::TempDir;
use tokio::{task::JoinHandle, time::timeout};
use tokio_util::sync::CancellationToken;

const PHASE_TIMEOUT: Duration = Duration::from_secs(60);
const CASE_TIMEOUT: Duration = Duration::from_secs(180);
const LIST_REPLY: &str = "There are 0 of a max of 8 players online:";
const TEST_CONFIG: &str = r#"
allow_nether = false
allow_end = false
allow_chat_reports = false
use_favicon = false
default_level_name = "world"

[logging]
color = false

[world]
autosave_ticks = 0

[networking.java]
enabled = true
address = "127.0.0.1:0"
encryption = false
online_mode = false
max_players = 8
view_distance = 2
simulation_distance = 2

[networking.bedrock]
enabled = false
online_mode = false

[networking.bedrock.authentication]
enabled = false

[networking.bedrock.nethernet]
enabled = false
address = "127.0.0.1:0"

[networking.rcon]
enabled = false
address = "127.0.0.1:0"

[networking.query]
enabled = false
address = "127.0.0.1:0"

[networking.lan_broadcast]
enabled = false

[plugins]
enabled = false

[telemetry]
enabled = false
"#;

struct ManagedServer {
    gui: GuiHandle,
    shutdown: CancellationToken,
    supervisor: JoinHandle<io::Result<()>>,
    directory: Option<TempDir>,
    reaped: bool,
    logs: Vec<String>,
}

impl ManagedServer {
    fn new(use_console: bool) -> Result<Self, String> {
        let directory = tempfile::Builder::new()
            .prefix("pumpkin-gui-restart-")
            .tempdir()
            .map_err(|error| error.to_string())?;
        std::fs::write(
            directory.path().join("pumpkin.toml"),
            format!("{TEST_CONFIG}\n[commands]\nuse_console = {use_console}\nuse_tty = false\n"),
        )
        .map_err(|error| error.to_string())?;

        // The loader falls back to defaults on malformed TOML. Check the actual
        // merged configuration before a child can bind sockets or open a world.
        let config = PumpkinConfig::load(directory.path());
        let networking = &config.advanced.networking;
        if !networking.java.enabled
            || !networking.java.address.ip().is_loopback()
            || networking.java.address.port() != 0
            || networking.java.online_mode
            || networking.java.max_players != 8
            || networking.bedrock.enabled
            || networking.rcon.enabled
            || networking.query.enabled
            || networking.lan_broadcast.enabled
            || config.telemetry.enabled
            || config.advanced.plugins.enabled
            || config.basic.allow_nether
            || config.basic.allow_end
            || config.basic.allow_chat_reports
            || config.basic.default_level_name != "world"
            || config.advanced.commands.use_console != use_console
        {
            return Err(
                "The isolated GUI test configuration was not loaded correctly.".to_string(),
            );
        }

        let (gui, desired) = GuiHandle::new_managed();
        let shutdown = CancellationToken::new();
        let supervisor = tokio::spawn(run_supervisor(
            gui.clone(),
            desired,
            shutdown.clone(),
            PathBuf::from(env!("CARGO_BIN_EXE_pumpkin")),
            directory.path().to_path_buf(),
        ));
        Ok(Self {
            gui,
            shutdown,
            supervisor,
            directory: Some(directory),
            reaped: false,
            logs: Vec::new(),
        })
    }

    fn collect_logs(&mut self) {
        self.logs
            .extend(self.gui.drain_logs().into_iter().map(|line| line.text));
    }

    async fn wait_for(
        &mut self,
        description: &str,
        condition: impl Fn(&Self) -> bool,
    ) -> Result<(), String> {
        timeout(PHASE_TIMEOUT, async {
            loop {
                self.collect_logs();
                if condition(self) {
                    return Ok(());
                }
                let snapshot = self.gui.snapshot();
                if snapshot.status == ServerStatus::Failed || self.supervisor.is_finished() {
                    return Err(format!(
                        "Server ended while waiting for {description}: {:?}",
                        snapshot.last_error
                    ));
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .map_err(|_| format!("Timed out waiting for {description}."))?
    }

    async fn wait_for_stopped(&mut self) -> Result<(), String> {
        self.wait_for("Stopped", |server| {
            server.gui.snapshot().status == ServerStatus::Stopped
        })
        .await?;
        if self.supervisor.is_finished() || self.gui.close_requested() {
            return Err("Stop closed the managed GUI session instead of leaving it usable.".into());
        }
        Ok(())
    }

    async fn list_players(&mut self) -> Result<(), String> {
        self.collect_logs();
        let previous_logs = self.logs.len();
        self.gui.submit_command("list")?;
        self.wait_for("a new list command reply", |server| {
            server.logs[previous_logs..]
                .iter()
                .any(|line| line.starts_with(LIST_REPLY))
        })
        .await
    }

    fn process_ids(&self) -> Vec<u32> {
        self.logs
            .iter()
            .filter_map(|line| {
                line.strip_prefix("Started server process (PID ")?
                    .strip_suffix(").")?
                    .parse()
                    .ok()
            })
            .collect()
    }

    async fn finish(mut self, result: Result<(), String>) -> Result<(), String> {
        self.shutdown.cancel();
        let cleanup = timeout(PHASE_TIMEOUT, &mut self.supervisor)
            .await
            .map_or_else(
                |_| {
                    Err("Supervisor did not finish graceful shutdown; its temporary world is retained."
                        .to_string())
                },
                |joined| {
                    self.reaped = matches!(&joined, Ok(Ok(())));
                    joined
                        .map_err(|error| format!("Supervisor task failed: {error}"))
                        .and_then(|result| result.map_err(|error| error.to_string()))
                },
            );
        self.collect_logs();
        match (result, cleanup) {
            (Ok(()), Ok(())) => Ok(()),
            (result, cleanup) => Err(format!(
                "Test result: {result:?}\nCleanup result: {cleanup:?}\nWorking directory: {:?}\nRecent GUI logs:\n{}",
                self.directory.as_ref().map(TempDir::path),
                self.logs[self.logs.len().saturating_sub(80)..].join("\n")
            )),
        }
    }
}

impl Drop for ManagedServer {
    fn drop(&mut self) {
        self.shutdown.cancel();
        // Normal failures are reported only after finish has awaited the child.
        // If that deadline expires (or the test panics), do not delete a world
        // while its child may still be saving it. IPC EOF still requests Stop.
        if !self.reaped
            && let Some(directory) = self.directory.take()
        {
            let _ = directory.keep();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_gui_restarts_real_server_and_keeps_commands_and_logs() -> Result<(), String> {
    let mut server = ManagedServer::new(true)?;
    let result = timeout(CASE_TIMEOUT, async {
        for cycle in 0..2 {
            if cycle != 0 {
                server.gui.request_start()?;
            }
            server
                .wait_for("Running", |server| {
                    server.gui.snapshot().status == ServerStatus::Running
                })
                .await?;
            server.list_players().await?;
            if cycle == 0 {
                server.gui.request_stop();
            } else {
                server.gui.submit_command("stop")?;
            }
            server.wait_for_stopped().await?;
        }

        let process_ids = server.process_ids();
        if process_ids.len() != 2 || process_ids[0] == process_ids[1] {
            return Err(format!(
                "Restart must launch a different real child process: {process_ids:?}"
            ));
        }
        if server
            .logs
            .iter()
            .filter(|line| line.starts_with(LIST_REPLY))
            .count()
            != 2
        {
            return Err("The same GUI log history must contain both command replies.".into());
        }
        Ok(())
    })
    .await
    .unwrap_or_else(|_| Err("Timed out exercising two real server lifecycles.".into()));
    server.finish(result).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_gui_stop_works_when_console_commands_are_disabled() -> Result<(), String> {
    let mut server = ManagedServer::new(false)?;
    let result = timeout(CASE_TIMEOUT, async {
        server
            .wait_for("Running with console disabled", |server| {
                server.gui.snapshot().status == ServerStatus::Running
            })
            .await?;
        if server.gui.snapshot().commands_enabled || server.gui.submit_command("list").is_ok() {
            return Err("The GUI accepted a command with use_console=false.".into());
        }
        server.gui.request_stop();
        server.wait_for_stopped().await
    })
    .await
    .unwrap_or_else(|_| Err("Timed out exercising Stop with console disabled.".into()));
    server.finish(result).await
}
