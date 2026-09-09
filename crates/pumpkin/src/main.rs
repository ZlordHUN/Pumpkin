#![deny(clippy::unwrap_used)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
// Don't warn on event sending macros
#![recursion_limit = "512"]

#[cfg(target_os = "wasi")]
compile_error!("Compiling for WASI targets is not supported!");

mod cli;
// Keep desktop rendering in the executable, with its source alongside the GUI bridge.
#[cfg(all(
    feature = "gui",
    any(target_os = "linux", target_os = "windows", target_os = "macos")
))]
#[path = "gui/native/mod.rs"]
mod native_gui;

use pumpkin_data::packet::CURRENT_MC_VERSION;
use pumpkin_world::{CURRENT_BEDROCK_MC_PROTOCOL, CURRENT_BEDROCK_MC_VERSION};
use std::{
    backtrace::{Backtrace, BacktraceStatus},
    io::{self},
    panic::PanicHookInfo,
    process::exit,
    sync::{OnceLock, atomic::Ordering},
    thread::{self, ThreadId},
};
#[cfg(not(unix))]
use tokio::signal::ctrl_c;
#[cfg(unix)]
use tokio::signal::unix::{SignalKind, signal};

use pumpkin::{
    CRASH_REPORT, SERVER_EXIT_CODE, SERVER_IS_STOPPING,
    crash::{CrashReport, FullBacktrace},
    data::VanillaData,
    stop_or_exit_server,
};
use pumpkin::{PumpkinServer, stop_server};

use pumpkin_config::{LoadConfiguration, PumpkinConfig};
use pumpkin_util::text::{
    TextComponent,
    color::{Color, NamedColor},
};
use std::time::Instant;
use tracing::{debug, info, warn};

const CARGO_PKG_VERSION: &str = env!("CARGO_PKG_VERSION");

static MAIN_THREAD: OnceLock<ThreadId> = OnceLock::new();

// WARNING: All rayon calls from the tokio runtime must be non-blocking! This includes things
// like `par_iter`. These should be spawned in the the rayon pool and then passed to the tokio
// runtime with a channel! See `Level::fetch_chunks` as an example!
#[allow(clippy::print_stdout, clippy::print_stderr)]
fn main() {
    let mode = cli::LaunchMode::parse(std::env::args().skip(1)).unwrap_or_else(|error| {
        eprintln!("{error}");
        exit(2);
    });
    match mode {
        cli::LaunchMode::Help => {
            println!(
                "Pumpkin {CARGO_PKG_VERSION}\n\nUsage: pumpkin [--gui | --nogui]\n\n  --gui       Open the native server window (requires a GUI-enabled desktop build)\n  --nogui     Run without a window; also accepts vanilla's 'nogui'\n  --help      Show this help\n  --version   Show the server version\n\nGUI-enabled builds open a window automatically when a desktop display is available.\nBuild one with: cargo build -p pumpkin --features gui"
            );
            return;
        }
        cli::LaunchMode::Version => {
            println!("Pumpkin {CARGO_PKG_VERSION}");
            return;
        }
        _ => {}
    }
    let use_gui = mode
        .use_gui(
            cfg!(all(
                feature = "gui",
                any(
                    target_os = "linux",
                    target_os = "windows",
                    target_os = "macos"
                )
            )),
            cli::display_available(),
        )
        .unwrap_or_else(|error| {
            eprintln!("{error}");
            exit(2);
        });

    let _ = MAIN_THREAD.set(thread::current().id());

    if !use_gui {
        // Server services live in the backend, not the desktop supervisor.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let _ = rayon::ThreadPoolBuilder::new()
            .thread_name(|i| format!("Rayon-Worker-{i}"))
            .build_global();
    }

    // Set the panic handler.
    std::panic::set_hook(Box::new(handle_panic));

    #[cfg(feature = "console-subscriber")]
    if !use_gui {
        console_subscriber::init();
    }

    let mut runtime_builder = tokio::runtime::Builder::new_multi_thread();
    if use_gui {
        runtime_builder.worker_threads(2);
    }
    let runtime = runtime_builder
        .enable_all()
        .build()
        .unwrap_or_else(|error| {
            eprintln!("Failed to create the server runtime: {error}");
            exit(1);
        });

    #[cfg(all(
        feature = "gui",
        any(target_os = "linux", target_os = "windows", target_os = "macos")
    ))]
    if mode == cli::LaunchMode::GuiBackend {
        runtime.block_on(run_gui_backend());
    } else if use_gui {
        run_desktop(&runtime);
    } else {
        runtime.block_on(run_server());
    }
    #[cfg(not(all(
        feature = "gui",
        any(target_os = "linux", target_os = "windows", target_os = "macos")
    )))]
    {
        debug_assert!(!use_gui);
        runtime.block_on(run_server());
    }

    exit(SERVER_EXIT_CODE.load(Ordering::Acquire));
}

#[cfg(all(
    feature = "gui",
    any(target_os = "linux", target_os = "windows", target_os = "macos")
))]
#[allow(clippy::print_stderr)]
fn run_desktop(runtime: &tokio::runtime::Runtime) {
    use futures::FutureExt;
    use pumpkin::gui::GuiHandle;
    use tokio_util::sync::CancellationToken;

    let (gui, desired) = GuiHandle::new_managed();
    assert!(gui.clone().install().is_ok(), "GUI already initialized");
    let shutdown = CancellationToken::new();
    let supervisor_gui = gui.clone();
    let supervisor_shutdown = shutdown.clone();
    let backend = runtime.spawn(async move {
        let task = async {
            pumpkin::gui::process::run_supervisor(
                supervisor_gui.clone(),
                desired,
                supervisor_shutdown,
                std::env::current_exe()?,
                std::env::current_dir()?,
            )
            .await
        };
        let error = match std::panic::AssertUnwindSafe(task).catch_unwind().await {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(format!("Server supervisor failed: {error}")),
            Err(_) => Some("The server supervisor encountered a fatal error.".to_owned()),
        };
        if let Some(error) = error {
            supervisor_gui.push_log(tracing::Level::ERROR, &error);
            supervisor_gui.finish(Some(error));
            SERVER_EXIT_CODE.store(1, Ordering::Release);
        }
    });
    let signal_gui = gui.clone();
    let signal_task = runtime.spawn(async move {
        match wait_for_signal().await {
            Ok(()) => signal_gui.request_close(),
            Err(error) => signal_gui.push_log(
                tracing::Level::WARN,
                &format!("Unable to set up signal handlers: {error}"),
            ),
        }
    });

    // Keep the OS event loop alive between backend runs. Do not abandon a child
    // while it is saving, including if opening or rendering the window fails.
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        native_gui::run(gui.clone())
    })) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            eprintln!("Unable to open the server window: {error}");
            SERVER_EXIT_CODE.store(1, Ordering::Release);
        }
        Err(_) => {
            eprintln!("The server window failed; waiting for the server to save and stop.");
            SERVER_EXIT_CODE.store(1, Ordering::Release);
        }
    }
    gui.request_close();
    shutdown.cancel();
    if let Err(error) = runtime.block_on(backend) {
        eprintln!("Server task failed: {error}");
        SERVER_EXIT_CODE.store(1, Ordering::Release);
    }
    signal_task.abort();
}

#[cfg(all(
    feature = "gui",
    any(target_os = "linux", target_os = "windows", target_os = "macos")
))]
#[allow(clippy::print_stderr)]
async fn run_gui_backend() {
    use futures::FutureExt;
    use pumpkin::gui::{GuiHandle, process};
    use tokio_util::sync::CancellationToken;

    // Authenticate before loading a world or opening server ports.
    let connection = match process::connect_backend().await {
        Ok(connection) => connection,
        Err(error) => {
            eprintln!("Unable to connect to the server window: {error}");
            SERVER_EXIT_CODE.store(1, Ordering::Release);
            return;
        }
    };
    let gui = GuiHandle::new();
    assert!(
        gui.clone().install().is_ok(),
        "GUI bridge already initialized"
    );
    let finished = CancellationToken::new();
    let transport = tokio::spawn(process::serve_backend(
        connection,
        gui.clone(),
        finished.clone(),
    ));
    let result = std::panic::AssertUnwindSafe(run_server())
        .catch_unwind()
        .await;
    let error = if result.is_err() {
        SERVER_EXIT_CODE.store(1, Ordering::Release);
        if let Some(report) = CRASH_REPORT.get() {
            report.print_to_console();
            report.save_and_log();
        }
        Some("The server encountered a fatal error. See the crash report and log.".to_owned())
    } else if SERVER_EXIT_CODE.load(Ordering::Acquire) != 0 {
        Some("The server stopped with an error. See the log for details.".to_owned())
    } else {
        None
    };
    gui.finish(error);
    finished.cancel();
    match transport.await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            eprintln!("Server window connection ended: {error}");
            SERVER_EXIT_CODE.store(1, Ordering::Release);
        }
        Err(error) => {
            eprintln!("Server window transport failed: {error}");
            SERVER_EXIT_CODE.store(1, Ordering::Release);
        }
    }
}

#[allow(clippy::too_many_lines)]
async fn run_server() {
    let time = Instant::now();

    let exec_dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

    let config = PumpkinConfig::load(&exec_dir);

    let vanilla_data = VanillaData::load();

    pumpkin::init_logger(&config.advanced);

    info!(
        "{}",
        TextComponent::text(format!(
            "Starting {} {} Java Minecraft (Protocol {}) | {} Bedrock (Protocol {})",
            TextComponent::text("Pumpkin")
                .color_named(NamedColor::Gold)
                .to_pretty_console(),
            TextComponent::text(CARGO_PKG_VERSION.to_string())
                .color_named(NamedColor::Green)
                .to_pretty_console(),
            TextComponent::text(CURRENT_MC_VERSION.protocol_version().to_string())
                .color_named(NamedColor::DarkBlue)
                .to_pretty_console(),
            TextComponent::text(CURRENT_BEDROCK_MC_VERSION)
                .color_named(NamedColor::Gold)
                .to_pretty_console(),
            TextComponent::text(CURRENT_BEDROCK_MC_PROTOCOL.to_string())
                .color_named(NamedColor::DarkBlue)
                .to_pretty_console()
        ))
        .to_pretty_console(),
    );

    debug!(
        "Build info: FAMILY: \"{}\", OS: \"{}\", ARCH: \"{}\", BUILD: \"{}\"",
        std::env::consts::FAMILY,
        std::env::consts::OS,
        std::env::consts::ARCH,
        if cfg!(debug_assertions) {
            "Debug"
        } else {
            "Release"
        }
    );
    if cfg!(debug_assertions) {
        warn!(
            "Pumpkin is running an unoptimized debug build. Do not use this build for performance testing; run `cargo run --release` or use a release binary."
        );
    }
    print_support_links_and_warning();

    tokio::spawn(async {
        if let Err(err) = setup_sighandler().await {
            tracing::error!("Unable to setup signal handlers: {err}");
        }
    });

    let Ok(pumpkin_server) = PumpkinServer::new(
        config.basic,
        config.advanced,
        config.telemetry,
        vanilla_data,
    )
    .await
    else {
        // Startup already logged the detailed cause. In GUI mode, return
        // through run_gui_backend so its final log batch is sent before exit.
        SERVER_EXIT_CODE.store(1, Ordering::Release);
        return;
    };
    let plugin_wait_time = pumpkin_server.init_plugins().await;

    let time_elapsed = time.elapsed().saturating_sub(plugin_wait_time);

    info!(
        "Started server; took {}",
        TextComponent::text(format!("{}ms", time_elapsed.as_millis()))
            .color_named(NamedColor::Gold)
            .to_pretty_console()
    );
    let advanced_config = &pumpkin_server.server.advanced_config;
    info!(
        "Server is now running. Connect using port: {}{}{}",
        if advanced_config.networking.java.enabled {
            format!(
                "{} {}",
                TextComponent::text("Java Edition:")
                    .color_named(NamedColor::Yellow)
                    .to_pretty_console(),
                TextComponent::text(format!("{}", advanced_config.networking.java.address))
                    .color_named(NamedColor::DarkBlue)
                    .to_pretty_console()
            )
        } else {
            TextComponent::text(String::new()).to_pretty_console()
        },
        if advanced_config.networking.java.enabled && advanced_config.networking.bedrock.enabled {
            " | " // Separator if both are enabled
        } else {
            ""
        },
        if advanced_config.networking.bedrock.enabled {
            format!(
                "{} {}",
                TextComponent::text("Bedrock Edition:")
                    .color_named(NamedColor::Gold)
                    .to_pretty_console(),
                TextComponent::text(format!(
                    "{}",
                    advanced_config.networking.bedrock.nethernet.address
                ))
                .color_named(NamedColor::DarkBlue)
                .to_pretty_console()
            )
        } else {
            TextComponent::text(String::new()).to_pretty_console()
        }
    );

    #[cfg(feature = "gui")]
    if let Some(gui) = pumpkin::gui::active() {
        gui.attach_server(&pumpkin_server.server);
    }

    pumpkin_server.start().await;

    info!(
        "{}",
        TextComponent::text("The server has stopped.")
            .color_named(NamedColor::Red)
            .to_pretty_console()
    );
}
fn print_support_links_and_warning() {
    warn!(
        "{}",
        TextComponent::text("Pumpkin is currently under heavy development!")
            .color_named(NamedColor::DarkRed)
            .to_pretty_console(),
    );
    info!(
        "Report issues on {}",
        TextComponent::text("https://github.com/Pumpkin-MC/Pumpkin/issues")
            .color_named(NamedColor::DarkAqua)
            .to_pretty_console()
    );
    info!(
        "Join our {} for community support: {}",
        TextComponent::text("Discord")
            .color_named(NamedColor::DarkBlue)
            .to_pretty_console(),
        TextComponent::text("https://discord.gg/wT8XjrjKkf")
            .color_named(NamedColor::Aqua)
            .to_pretty_console()
    );
    info!(
        "Consider {} to {}",
        TextComponent::text("Donating")
            .color_named(NamedColor::DarkPurple)
            .to_pretty_console(),
        TextComponent::text("https://pumpkinmc.org/donate/")
            .color_named(NamedColor::Gold)
            .to_pretty_console()
    );
}

fn handle_interrupt() {
    warn!(
        "{}",
        TextComponent::text("Received interrupt signal; stopping server...")
            .color_named(NamedColor::Red)
            .to_pretty_console()
    );
    #[cfg(feature = "gui")]
    if pumpkin::gui::active().is_some() {
        // One terminal signal can reach both the window and its child. The
        // window's IPC Stop may already be saving; never force-exit that child.
        stop_server();
        return;
    }
    stop_or_exit_server();
}

fn handle_panic(panic_info: &PanicHookInfo<'_>) {
    // Generate a crash report.
    let crash_report = {
        // We capture the backtraces here, and not in the
        // crash report, so that the backtrace doesn't show
        // the CrashReport's `new` function.
        let captured_backtrace = Backtrace::capture();
        let full_backtrace = if captured_backtrace.status() == BacktraceStatus::Captured {
            FullBacktrace::Captured
        } else {
            FullBacktrace::ForceCaptured(Backtrace::force_capture())
        };

        CrashReport::new(panic_info, captured_backtrace, full_backtrace)
    };

    let payload = panic_info.payload();

    #[cfg(feature = "gui")]
    let desktop_active = pumpkin::gui::active().is_some();
    #[cfg(not(feature = "gui"))]
    let desktop_active = false;

    // A desktop-window panic is caught by run_desktop, which can still wait for
    // the backend to save. A panic in the headless main future cannot do that.
    if is_main_thread() && !desktop_active {
        // It's the first panic;
        // We cannot gracefully shut down as the main thread
        // has panicked. However, we can still generate the crash report.

        if let Some(crash_report) = try_set_crash_report(crash_report) {
            crash_report.print_to_console();
            crash_report.save_and_log();

            tracing::error!(
                "{}",
                TextComponent::text("Aborting due to the main thread panicking.")
                    .color(Color::Named(NamedColor::Red))
                    .to_pretty_console()
            );
        } else {
            // It's a subsequent panic.
            tracing::error!(
                "{}: {}",
                TextComponent::text(
                    "The main thread panicked while stopping the server; aborting."
                )
                .color(Color::Named(NamedColor::Red))
                .bold()
                .to_pretty_console(),
                payload
                    .downcast_ref::<&str>()
                    .copied()
                    .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
                    .unwrap_or("<unknown>")
            );
        }

        exit(1);
    }

    if try_set_crash_report(crash_report).is_some() {
        // It's the first panic; let's stop the server.
        stop_server();
    } else {
        // It's a subsequent panic; let's just alert about it.
        tracing::error!(
            "{}: {}",
            TextComponent::text("Encountered panic while shutting down")
                .color(Color::Named(NamedColor::Red))
                .bold()
                .to_pretty_console(),
            payload
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
                .unwrap_or("<unknown>")
        );
    }
}

fn is_main_thread() -> bool {
    Some(&thread::current().id()) == MAIN_THREAD.get()
}

/// Returns `Some` if the crash report was successfully set. That
/// means it is the first panic, and it must be logged and saved later.
///
/// Returns `None` otherwise as the panic is subsequent.
fn try_set_crash_report(crash_report: CrashReport) -> Option<&'static CrashReport> {
    if !SERVER_IS_STOPPING.load(Ordering::Acquire) && CRASH_REPORT.set(crash_report).is_ok() {
        CRASH_REPORT.get()
    } else {
        None
    }
}

async fn setup_sighandler() -> io::Result<()> {
    wait_for_signal().await?;
    handle_interrupt();
    Ok(())
}

#[cfg(not(unix))]
async fn wait_for_signal() -> io::Result<()> {
    ctrl_c().await
}

#[cfg(unix)]
async fn wait_for_signal() -> io::Result<()> {
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut hangup = signal(SignalKind::hangup())?;
    let mut terminate = signal(SignalKind::terminate())?;

    let received = tokio::select! {
        received = interrupt.recv() => received,
        received = hangup.recv() => received,
        received = terminate.recv() => received,
    };

    if received.is_none() {
        return Err(io::Error::other("Signal stream closed"));
    }

    Ok(())
}
