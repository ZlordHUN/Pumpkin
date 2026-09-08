//! A private, structured control channel for fresh server processes.

use std::{io, path::PathBuf, process::ExitStatus, process::Stdio, time::Duration};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream, tcp::OwnedWriteHalf},
    process::{Child, Command},
    sync::{mpsc, watch},
    time::{MissedTickBehavior, timeout},
};
use tokio_util::sync::CancellationToken;
use tracing::Level;
use uuid::Uuid;

use super::{GuiHandle, LogLine, ServerSnapshot, ServerStatus};

const CHANNEL_ENV: &str = "PUMPKIN_GUI_CHANNEL";
const PROTOCOL_VERSION: u32 = 1;
const MAX_FRAME_BYTES: usize = 1_024 * 1_024;
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const UPDATE_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Serialize, Deserialize)]
struct Hello {
    version: u32,
    token: Uuid,
}

#[derive(Debug, Serialize, Deserialize)]
struct Ready {
    version: u32,
}

#[derive(Debug, Serialize, Deserialize)]
enum Control {
    Command(String),
    Stop,
}

#[derive(Debug, Serialize, Deserialize)]
struct Update {
    snapshot: ServerSnapshot,
    logs: Vec<LogLine>,
}

/// Supervises one child at a time, retaining the GUI after each completed run.
pub async fn run_supervisor(
    gui: GuiHandle,
    mut desired: watch::Receiver<bool>,
    shutdown: CancellationToken,
    executable: PathBuf,
    working_dir: PathBuf,
) -> io::Result<()> {
    let mut commands = gui
        .take_command_receiver()
        .ok_or_else(|| io::Error::other("The GUI command receiver is already in use."))?;
    loop {
        let should_run = *desired.borrow_and_update();
        if shutdown.is_cancelled() {
            if !matches!(
                gui.snapshot().status,
                ServerStatus::Stopped | ServerStatus::Failed
            ) {
                gui.request_stop();
                drain_commands(&mut commands);
                gui.finish(None);
            }
            return Ok(());
        }
        if !should_run {
            // Stop can arrive before a child has been spawned.
            if gui.snapshot().status == ServerStatus::Stopping {
                drain_commands(&mut commands);
                gui.finish(None);
            }
            tokio::select! {
                () = shutdown.cancelled() => return Ok(()),
                changed = desired.changed() => if changed.is_err() { return Ok(()); },
            }
            continue;
        }

        let result = run_session(
            &gui,
            &mut desired,
            &shutdown,
            &mut commands,
            &executable,
            &working_dir,
        )
        .await;
        // Close command admission before draining, and do not expose Start until
        // the previous process and its IPC reader have both finished.
        gui.request_stop();
        drain_commands(&mut commands);
        match result {
            Ok(error) => gui.finish(error),
            Err(error) => {
                // A failed OS wait cannot prove that the child was reaped.
                gui.push_log(
                    Level::ERROR,
                    &format!("Unable to supervise the server: {error}"),
                );
                return Err(error);
            }
        }
    }
}

fn drain_commands(commands: &mut mpsc::Receiver<String>) {
    while commands.try_recv().is_ok() {}
}

enum Bootstrap {
    Connected(TcpStream),
    Stopped,
    Exited(ExitStatus),
}

async fn run_session(
    gui: &GuiHandle,
    desired: &mut watch::Receiver<bool>,
    shutdown: &CancellationToken,
    commands: &mut mpsc::Receiver<String>,
    executable: &std::path::Path,
    working_dir: &std::path::Path,
) -> io::Result<Option<String>> {
    let listener = match TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await {
        Ok(listener) => listener,
        Err(error) => {
            return Ok(Some(format!(
                "Unable to open the server control channel: {error}"
            )));
        }
    };
    if shutdown.is_cancelled() || !*desired.borrow() {
        return Ok(None);
    }
    let token = Uuid::new_v4();
    let mut child = match Command::new(executable)
        .arg("--gui-backend")
        .env(
            CHANNEL_ENV,
            format!("{}:{token}", listener.local_addr()?.port()),
        )
        .current_dir(working_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(false)
        .spawn()
    {
        Ok(child) => child,
        Err(error) => return Ok(Some(format!("Unable to start the server process: {error}"))),
    };
    if let Some(pid) = child.id() {
        gui.push_log(Level::INFO, &format!("Started server process (PID {pid})."));
    }

    let mut stream = match await_backend(&listener, token, &mut child, desired, shutdown).await {
        Ok(Bootstrap::Connected(stream)) => stream,
        Ok(Bootstrap::Exited(status)) => {
            return Ok(Some(format!(
                "The server exited before connecting to its window: {status}"
            )));
        }
        result => {
            // No ACK has been sent, so the child cannot have opened any worlds.
            if child.try_wait()?.is_none() {
                child.start_kill()?;
            }
            child.wait().await?;
            return Ok(result
                .err()
                .map(|error| format!("Unable to connect to the server process: {error}")));
        }
    };
    drop(listener);
    if shutdown.is_cancelled() || !*desired.borrow() {
        drop(stream);
        if child.try_wait()?.is_none() {
            child.start_kill()?;
        }
        child.wait().await?;
        return Ok(None);
    }

    // From the first ACK byte onward, assume the server might have started.
    // A failed write closes the socket and requests shutdown through EOF;
    // never force-kill a possibly running server.
    if let Err(error) = write_frame(
        &mut stream,
        &Ready {
            version: PROTOCOL_VERSION,
        },
    )
    .await
    {
        gui.request_stop();
        drop(stream);
        child.wait().await?;
        return Ok(Some(format!(
            "Unable to acknowledge the server connection: {error}"
        )));
    }
    supervise_connected(gui, child, stream, desired, shutdown, commands).await
}

async fn await_backend(
    listener: &TcpListener,
    token: Uuid,
    child: &mut Child,
    desired: &mut watch::Receiver<bool>,
    shutdown: &CancellationToken,
) -> io::Result<Bootstrap> {
    let accept = timeout(HANDSHAKE_TIMEOUT, async {
        loop {
            let (mut stream, _) = listener.accept().await?;
            let hello = timeout(Duration::from_secs(1), read_frame::<_, Hello>(&mut stream)).await;
            if let Ok(Ok(hello)) = hello
                && hello.version == PROTOCOL_VERSION
                && hello.token == token
            {
                return Ok::<_, io::Error>(stream);
            }
        }
    });
    tokio::pin!(accept);
    loop {
        if shutdown.is_cancelled() || !*desired.borrow() {
            return Ok(Bootstrap::Stopped);
        }
        tokio::select! {
            biased;
            status = child.wait() => return status.map(Bootstrap::Exited),
            () = shutdown.cancelled() => return Ok(Bootstrap::Stopped),
            changed = desired.changed() => if changed.is_err() { return Ok(Bootstrap::Stopped); },
            stream = &mut accept => return stream.map_err(|_| timed_out("Server handshake timed out."))?.map(Bootstrap::Connected),
        }
    }
}

// Keep process ownership and its mandatory cleanup in the same state machine.
#[allow(clippy::too_many_lines)]
async fn supervise_connected(
    gui: &GuiHandle,
    mut child: Child,
    stream: TcpStream,
    desired: &mut watch::Receiver<bool>,
    shutdown: &CancellationToken,
    commands: &mut mpsc::Receiver<String>,
) -> io::Result<Option<String>> {
    let (mut reader, writer) = stream.into_split();
    let mut writer = Some(writer);
    let (updates_tx, mut updates) = mpsc::channel(4);
    let reader_task = tokio::spawn(async move {
        loop {
            let result = read_frame::<_, Update>(&mut reader).await;
            let failed = result.is_err();
            if updates_tx.send(result).await.is_err() || failed {
                break;
            }
        }
    });
    let mut stopping = false;
    let mut disconnected = false;
    let mut final_update = false;
    let mut error = None;
    let status = loop {
        if !stopping && (shutdown.is_cancelled() || !*desired.borrow()) {
            stopping = true;
            stop_child(gui, &mut writer, &mut error).await;
        }
        tokio::select! {
            biased;
            status = child.wait() => match status {
                Ok(status) => break status,
                Err(error) => {
                    gui.request_stop();
                    drop(writer);
                    reader_task.abort();
                    let _ = reader_task.await;
                    return Err(error);
                }
            },
            () = shutdown.cancelled(), if !stopping => {},
            changed = desired.changed(), if !stopping => {
                if changed.is_err() {
                    stopping = true;
                    stop_child(gui, &mut writer, &mut error).await;
                }
            },
            update = updates.recv(), if !disconnected => {
                match update {
                    Some(Ok(update)) => apply_update(gui, update, &mut final_update),
                    result => {
                        if !final_update {
                            error = Some(result.and_then(Result::err).map_or_else(
                                || "The server control channel closed unexpectedly.".to_owned(),
                                |error| format!("The server control channel failed: {error}"),
                            ));
                        }
                        disconnected = true;
                        stopping = true;
                        gui.request_stop();
                        writer = None;
                    }
                }
            },
            command = commands.recv(), if !stopping && writer.is_some() => {
                if let Some(command) = command
                    && let Some(stream) = &mut writer
                    && let Err(write_error) = write_frame(stream, &Control::Command(command)).await
                {
                    error = Some(format!("Unable to send a server command: {write_error}"));
                    gui.request_stop();
                    stopping = true;
                    writer = None;
                }
            },
        }
    };
    gui.request_stop();
    drop(writer);
    // Process exit can win the select before the reader delivers its last logs.
    let drain = async {
        while let Some(update) = updates.recv().await {
            match update {
                Ok(update) => apply_update(gui, update, &mut final_update),
                Err(read_error) => {
                    if !final_update {
                        error.get_or_insert_with(|| {
                            format!("The server control channel failed: {read_error}")
                        });
                    }
                }
            }
        }
    };
    if timeout(IO_TIMEOUT, drain).await.is_err() {
        error.get_or_insert_with(|| "Timed out receiving the final server logs.".to_owned());
        reader_task.abort();
    }
    let _ = reader_task.await;
    if !status.success() {
        error = Some(
            gui.snapshot()
                .last_error
                .unwrap_or_else(|| format!("The server process exited with {status}.")),
        );
    }
    Ok(error)
}

fn apply_update(gui: &GuiHandle, update: Update, final_update: &mut bool) {
    *final_update |= matches!(
        update.snapshot.status,
        ServerStatus::Stopped | ServerStatus::Failed
    );
    gui.apply_backend_update(update.snapshot, update.logs);
}

async fn stop_child(
    gui: &GuiHandle,
    writer: &mut Option<OwnedWriteHalf>,
    error: &mut Option<String>,
) {
    gui.request_stop();
    if let Some(stream) = writer
        && let Err(write_error) = write_frame(stream, &Control::Stop).await
    {
        error.get_or_insert_with(|| format!("Unable to request server shutdown: {write_error}"));
        *writer = None;
    }
}

/// Connects and authenticates before the child initializes its server or worlds.
pub async fn connect_backend() -> io::Result<TcpStream> {
    let channel = std::env::var(CHANNEL_ENV)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Missing GUI control channel."))?;
    let (port, token) = channel.split_once(':').ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "Invalid GUI control channel.")
    })?;
    let port: u16 = port
        .parse()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let token = Uuid::parse_str(token)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    timeout(HANDSHAKE_TIMEOUT, async {
        let mut stream = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).await?;
        write_frame(
            &mut stream,
            &Hello {
                version: PROTOCOL_VERSION,
                token,
            },
        )
        .await?;
        let ready: Ready = read_frame(&mut stream).await?;
        if ready.version != PROTOCOL_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Incompatible GUI control protocol.",
            ));
        }
        Ok(stream)
    })
    .await
    .map_err(|_| timed_out("GUI connection timed out."))?
}

/// Runs independently of server-tracked tasks so final save logs are forwarded.
pub async fn serve_backend(
    stream: TcpStream,
    gui: GuiHandle,
    finished: CancellationToken,
) -> io::Result<()> {
    let (mut reader, mut writer) = stream.into_split();
    let commands = async {
        loop {
            match read_frame(&mut reader).await? {
                Control::Stop => gui.request_stop(),
                Control::Command(command) => {
                    if let Err(error) = gui.submit_command(&command) {
                        gui.push_log(Level::WARN, &error);
                    }
                }
            }
        }
    };
    let updates = async {
        let mut interval = tokio::time::interval(UPDATE_INTERVAL);
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                () = finished.cancelled() => break,
                _ = interval.tick() => {},
            }
            let (snapshot, logs) = gui.backend_update();
            write_frame(&mut writer, &Update { snapshot, logs }).await?;
        }
        timeout(IO_TIMEOUT, async {
            loop {
                let (snapshot, logs) = gui.backend_update();
                let drained = logs.is_empty();
                write_frame(&mut writer, &Update { snapshot, logs }).await?;
                if drained {
                    return Ok::<_, io::Error>(());
                }
            }
        })
        .await
        .map_err(|_| timed_out("Sending the final server logs timed out."))?
    };
    let result: io::Result<()> = tokio::select! {
        result = commands => result,
        result = updates => result,
    };
    if result.is_err() {
        // The window disappeared or its channel failed. This is always a
        // graceful request, even if a previous Stop is already being handled.
        gui.request_stop();
    }
    result
}

struct BoundedJson(Vec<u8>);

impl io::Write for BoundedJson {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_FRAME_BYTES.saturating_sub(self.0.len()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "GUI message is too large.",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

async fn write_frame<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    value: &T,
) -> io::Result<()> {
    let mut encoded = BoundedJson(Vec::new());
    serde_json::to_writer(&mut encoded, value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    timeout(IO_TIMEOUT, async {
        writer.write_u32(encoded.0.len() as u32).await?;
        writer.write_all(&encoded.0).await?;
        writer.flush().await
    })
    .await
    .map_err(|_| timed_out("Writing a GUI message timed out."))?
}

async fn read_frame<R: AsyncRead + Unpin, T: DeserializeOwned>(reader: &mut R) -> io::Result<T> {
    // Idle connections are allowed; once a frame starts, its header and payload
    // must complete within one deadline. Keep this future alive while selecting.
    let first = reader.read_u8().await?;
    timeout(IO_TIMEOUT, async {
        let mut header = [first, 0, 0, 0];
        reader.read_exact(&mut header[1..]).await?;
        let length = u32::from_be_bytes(header) as usize;
        if length == 0 || length > MAX_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid GUI message length.",
            ));
        }
        let mut payload = vec![0; length];
        reader.read_exact(&mut payload).await?;
        serde_json::from_slice(&payload)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    })
    .await
    .map_err(|_| timed_out("Reading a GUI message timed out."))?
}

fn timed_out(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn wait_for_status(gui: &GuiHandle, status: ServerStatus) {
        timeout(Duration::from_secs(5), async {
            while gui.snapshot().status != status {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("the supervisor should publish the expected status");
    }

    #[tokio::test]
    async fn missing_executable_can_be_retried_after_failure() {
        let directory = tempfile::tempdir().unwrap();
        let (gui, desired) = GuiHandle::new_managed();
        let shutdown = CancellationToken::new();
        let supervisor = tokio::spawn(run_supervisor(
            gui.clone(),
            desired,
            shutdown.clone(),
            directory.path().join("missing-pumpkin-executable"),
            directory.path().to_path_buf(),
        ));
        wait_for_status(&gui, ServerStatus::Failed).await;
        gui.request_start().unwrap();
        wait_for_status(&gui, ServerStatus::Failed).await;
        let errors = gui
            .drain_logs()
            .iter()
            .filter(|line| line.text.contains("Unable to start the server process"))
            .count();
        let still_supervising = !supervisor.is_finished();
        shutdown.cancel();
        supervisor.await.unwrap().unwrap();
        assert_eq!(errors, 2);
        assert!(still_supervising);
    }

    #[tokio::test]
    async fn stop_before_spawn_does_not_attempt_to_launch_a_process() {
        let directory = tempfile::tempdir().unwrap();
        let (gui, desired) = GuiHandle::new_managed();
        gui.request_stop();
        let shutdown = CancellationToken::new();
        let supervisor = tokio::spawn(run_supervisor(
            gui.clone(),
            desired,
            shutdown.clone(),
            directory.path().join("missing-pumpkin-executable"),
            directory.path().join("missing-working-directory"),
        ));
        wait_for_status(&gui, ServerStatus::Stopped).await;
        let logs = gui.drain_logs();
        shutdown.cancel();
        supervisor.await.unwrap().unwrap();
        assert!(
            logs.is_empty(),
            "even an attempted spawn would have reported an error"
        );
    }

    #[tokio::test]
    async fn disconnected_parent_requests_graceful_backend_stop() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let mut parent = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (backend, _) = listener.accept().await.unwrap();
        // Managed mode observes the same stop request without cancelling the
        // process-global server token shared by other library tests.
        let (gui, desired) = GuiHandle::new_managed();
        let task = tokio::spawn(serve_backend(
            backend,
            gui.clone(),
            CancellationToken::new(),
        ));
        let _: Update = read_frame(&mut parent).await.unwrap();
        drop(parent);
        let result = timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
        assert!(result.is_err());
        assert_eq!(gui.snapshot().status, ServerStatus::Stopping);
        assert!(!*desired.borrow());
    }

    #[tokio::test]
    async fn framed_commands_and_structured_logs_round_trip() {
        let (mut sender, mut receiver) = tokio::io::duplex(16_384);
        write_frame(&mut sender, &Control::Command("say hráč".to_owned()))
            .await
            .unwrap();
        assert!(
            matches!(read_frame(&mut receiver).await.unwrap(), Control::Command(command) if command == "say hráč")
        );
        let gui = GuiHandle::new();
        gui.push_log(Level::WARN, "structured warning");
        let (snapshot, logs) = gui.backend_update();
        write_frame(&mut sender, &Update { snapshot, logs })
            .await
            .unwrap();
        let update: Update = read_frame(&mut receiver).await.unwrap();
        assert_eq!(update.logs.len(), 1);
        assert_eq!(update.logs[0].level, Level::WARN);
        assert_eq!(update.logs[0].text, "structured warning");
    }

    #[tokio::test]
    async fn invalid_lengths_and_truncated_payloads_are_rejected() {
        for length in [0, MAX_FRAME_BYTES as u32 + 1] {
            let (mut sender, mut receiver) = tokio::io::duplex(32);
            sender.write_u32(length).await.unwrap();
            let result = read_frame::<_, Control>(&mut receiver).await;
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
        }
        let (mut sender, mut receiver) = tokio::io::duplex(32);
        sender.write_u32(8).await.unwrap();
        sender.write_all(b"{}").await.unwrap();
        drop(sender);
        assert_eq!(
            read_frame::<_, Ready>(&mut receiver)
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[tokio::test]
    async fn oversized_outgoing_frames_do_not_write_a_header() {
        let (mut sender, mut receiver) = tokio::io::duplex(32);
        let result = write_frame(&mut sender, &"x".repeat(MAX_FRAME_BYTES)).await;
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
        drop(sender);
        assert_eq!(receiver.read(&mut [0; 4]).await.unwrap(), 0);
    }
}
