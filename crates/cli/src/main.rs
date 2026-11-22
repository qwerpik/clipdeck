//! `clipdeck`: the app (no arguments) and its command-line remote.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use clipdeck_core::paths::Paths;
use clipdeck_platform::ipc;
use clipdeck_ui::app::{AppCore, UiSignal};

/// A fast soundboard that plays clips into your microphone.
#[derive(Parser, Debug)]
#[command(name = "clipdeck", version, about, long_about = None)]
struct Cli {
    /// Start hidden in the tray.
    #[arg(long, global = false)]
    minimized: bool,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Play a sound (name, id, #position or file path). Without a running app the file plays
    /// standalone until it ends.
    Play {
        /// Sound reference(s).
        #[arg(required = true)]
        target: Vec<String>,
        /// Only into the virtual microphone.
        #[arg(long, conflicts_with = "monitor")]
        mic: bool,
        /// Only to your own headphones.
        #[arg(long)]
        monitor: bool,
    },
    /// Stop one sound, or everything.
    Stop {
        /// Sound reference; omit to stop all.
        target: Vec<String>,
    },
    /// Pause / resume everything.
    Pause,
    /// Play a random sound.
    Random,
    /// Get or set the master volume (0-100).
    Volume {
        /// New volume.
        value: Option<u8>,
    },
    /// List the library.
    List,
    /// Show what is playing and which devices are in use.
    Status,
    /// Show the window (optionally on a page: library, favorites, recent, categories, settings).
    Show {
        /// Page to open.
        page: Option<String>,
    },
    /// Hide the window.
    Hide,
    /// Toggle the window.
    Toggle,
    /// Toggle microphone passthrough.
    Mic,
    /// Toggle hear-it-yourself monitoring.
    Monitor,
    /// Quit the running app.
    Quit,
    /// Send a raw command, Clipdeck or Soundpad syntax (e.g. `DoPlaySound(3)`).
    Rc {
        /// Command text.
        command: Vec<String>,
    },
    /// Add files or folders to the library.
    Add {
        /// Paths.
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Import a Soundpad sound list (.spl); stats.spl next to it is used for play counts.
    Import {
        /// The .spl file.
        file: PathBuf,
    },
    /// Export the library as a Soundpad sound list (.spl).
    Export {
        /// Output file.
        file: PathBuf,
    },
    /// List audio devices.
    Devices,
    /// Run without a window: engine, hotkeys and IPC only.
    Serve,
    /// Measure latency, startup and memory.
    Bench {
        /// Audio file to use (defaults to a generated click).
        file: Option<PathBuf>,
    },
    /// Print where settings and the library live.
    Paths,
    /// Windows: put Clipdeck into your real microphone (no virtual cable needed).
    Micfx {
        #[command(subcommand)]
        action: micfx::MicfxCmd,
    },
}

fn init_logging(
    paths: &Paths,
    to_file: bool,
) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::prelude::*;
    let filter = tracing_subscriber::EnvFilter::try_from_env("CLIPDECK_LOG").unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new(
            "info,symphonia=warn,symphonia_core=warn,symphonia_bundle_mp3=warn,zbus=warn",
        )
    });
    if to_file {
        let _ = std::fs::create_dir_all(paths.log_dir());
        let appender = tracing_appender::rolling::never(paths.log_dir(), "clipdeck.log");
        let (nb, guard) = tracing_appender::non_blocking(appender);
        let _ = tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(nb)
                    .with_ansi(false),
            )
            .try_init();
        Some(guard)
    } else {
        let _ = tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
            .try_init();
        None
    }
}

fn remote(line: &str) -> ExitCode {
    match ipc::request(line) {
        Ok(resp) => {
            let failed =
                resp.starts_with("ERR") || resp.starts_with("R-4") || resp.starts_with("R-5");
            let text = resp
                .strip_prefix("OK")
                .map(str::trim_start)
                .unwrap_or(&resp);
            if !text.is_empty() {
                if failed {
                    eprintln!("{text}");
                } else {
                    println!("{text}");
                }
            }
            if failed {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(ipc::IpcError::NotRunning) => {
            eprintln!(
                "Clipdeck is not running. Start it with `clipdeck` (or `clipdeck serve` for no window)."
            );
            ExitCode::from(3)
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn quote(parts: &[String]) -> String {
    parts
        .iter()
        .map(|p| {
            if p.contains(char::is_whitespace) {
                format!("\"{p}\"")
            } else {
                p.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn main() -> ExitCode {
    #[cfg(windows)]
    if std::env::args().len() > 1 {
        clipdeck_platform::windows::attach_parent_console();
    }
    let cli = Cli::parse();
    let paths = match Paths::resolve() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let gui = cli.cmd.is_none();
    let _guard = init_logging(&paths, gui || matches!(cli.cmd, Some(Cmd::Serve)));

    match cli.cmd {
        None => run_app(paths, cli.minimized),
        Some(Cmd::Serve) => serve(paths),
        Some(Cmd::Play {
            target,
            mic,
            monitor,
        }) => {
            let flag = if mic {
                " --mic"
            } else if monitor {
                " --monitor"
            } else {
                ""
            };
            if ipc::is_running() {
                let mut code = ExitCode::SUCCESS;
                for t in &target {
                    let c = remote(&format!("play {}{flag}", quote(std::slice::from_ref(t))));
                    if c != ExitCode::SUCCESS {
                        code = c;
                    }
                }
                code
            } else {
                standalone_play(paths, &target, mic, monitor)
            }
        }
        Some(Cmd::Stop { target }) => remote(&if target.is_empty() {
            "stop".into()
        } else {
            format!("stop {}", quote(&target))
        }),
        Some(Cmd::Pause) => remote("pause"),
        Some(Cmd::Random) => remote("random"),
        Some(Cmd::Volume { value }) => remote(&match value {
            Some(v) => format!("volume {v}"),
            None => "volume".into(),
        }),
        Some(Cmd::List) => {
            if ipc::is_running() {
                remote("list")
            } else {
                let lib = clipdeck_core::Library::load(&paths.library_file()).unwrap_or_default();
                for (i, s) in lib.sounds.iter().enumerate() {
                    println!(
                        "{:>4}  {:<12}  {}",
                        i + 1,
                        s.hotkey.clone().unwrap_or_default(),
                        s.name
                    );
                }
                ExitCode::SUCCESS
            }
        }
        Some(Cmd::Status) => remote("status"),
        Some(Cmd::Show { page }) => remote(&format!("show {}", page.unwrap_or_default())),
        Some(Cmd::Hide) => remote("hide"),
        Some(Cmd::Toggle) => remote("toggle"),
        Some(Cmd::Mic) => remote("mic"),
        Some(Cmd::Monitor) => remote("monitor"),
        Some(Cmd::Quit) => remote("quit"),
        Some(Cmd::Rc { command }) => remote(&command.join(" ")),
        Some(Cmd::Add { paths: files }) => offline_library_op(&paths, |lib| {
            let files: Vec<PathBuf> = files
                .iter()
                .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()))
                .collect();
            let r = lib.add_paths(&files, None);
            format!(
                "added {} sound(s), {} already present, {} unsupported",
                r.added.len(),
                r.duplicates,
                r.unsupported
            )
        }),
        Some(Cmd::Import { file }) => offline_library_op(&paths, |lib| {
            let doc = match clipdeck_core::spl::SplDocument::load(&file) {
                Ok(d) => d,
                Err(e) => return format!("ERR {}", e.user_message()),
            };
            let stats = std::fs::read_to_string(file.with_file_name("stats.spl"))
                .ok()
                .and_then(|t| clipdeck_core::spl::parse_stats(&t).ok())
                .unwrap_or_default();
            let r = doc.import_into(lib, Some(&file), &stats);
            format!(
                "imported {} sound(s) ({} merged, {} missing on disk), {} categories",
                r.added, r.merged, r.missing, r.categories
            )
        }),
        Some(Cmd::Export { file }) => {
            let lib = clipdeck_core::Library::load(&paths.library_file()).unwrap_or_default();
            match clipdeck_core::spl::SplDocument::from_library(&lib, Some(&file)).save(&file) {
                Ok(()) => {
                    println!(
                        "exported {} sound(s) to {}",
                        lib.sounds.len(),
                        file.display()
                    );
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("{}", e.user_message());
                    ExitCode::FAILURE
                }
            }
        }
        Some(Cmd::Devices) => {
            println!("audio host: {}", clipdeck_audio::output::host_name());
            println!("outputs:");
            for d in clipdeck_audio::output::output_devices() {
                println!("  {} {}", if d.is_default { "*" } else { " " }, d.name);
            }
            println!("inputs:");
            for d in clipdeck_audio::output::input_devices() {
                println!("  {} {}", if d.is_default { "*" } else { " " }, d.name);
            }
            println!("virtual microphone: {}", clipdeck_platform::mic_hint());
            ExitCode::SUCCESS
        }
        Some(Cmd::Bench { file }) => bench::run(paths, file),
        Some(Cmd::Micfx { action }) => micfx::run(action),
        Some(Cmd::Paths) => {
            println!("config:  {}", paths.config_file().display());
            println!("library: {}", paths.library_file().display());
            println!("logs:    {}", paths.log_dir().display());
            ExitCode::SUCCESS
        }
    }
}

/// Run a library edit: through the running app (reload) or directly on disk.
fn offline_library_op(
    paths: &Paths,
    f: impl FnOnce(&mut clipdeck_core::Library) -> String,
) -> ExitCode {
    let file = paths.library_file();
    let mut lib = match clipdeck_core::Library::load(&file) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{}", e.user_message());
            return ExitCode::FAILURE;
        }
    };
    let msg = f(&mut lib);
    if let Some(e) = msg.strip_prefix("ERR ") {
        eprintln!("{e}");
        return ExitCode::FAILURE;
    }
    let _ = paths.ensure();
    if let Err(e) = lib.save(&file, Some(&paths.backup_dir())) {
        eprintln!("{}", e.user_message());
        return ExitCode::FAILURE;
    }
    println!("{msg}");
    if ipc::is_running() {
        let _ = ipc::request("reload");
    }
    ExitCode::SUCCESS
}

fn standalone_play(paths: Paths, targets: &[String], mic: bool, monitor: bool) -> ExitCode {
    let core = AppCore::start(paths);
    // Give the virtual mic a moment to appear so apps can connect.
    std::thread::sleep(Duration::from_millis(300));
    let route = if mic {
        Some(clipdeck_core::Route::MicOnly)
    } else if monitor {
        Some(clipdeck_core::Route::MonitorOnly)
    } else {
        None
    };
    for t in targets {
        let p = PathBuf::from(t);
        if p.is_file() {
            core.play_file(&p, route);
        } else {
            let resp = core.handle_line(&format!(
                "play {}{}",
                quote(std::slice::from_ref(t)),
                match route {
                    Some(clipdeck_core::Route::MicOnly) => " --mic",
                    Some(clipdeck_core::Route::MonitorOnly) => " --monitor",
                    _ => "",
                }
            ));
            if resp.starts_with("ERR") {
                eprintln!("{}", resp.trim_start_matches("ERR "));
                core.shutdown();
                return ExitCode::FAILURE;
            }
        }
    }
    let st = core.engine.device_status();
    if let Err(e) = &st.virtual_mic {
        eprintln!("virtual mic: {e}");
    }
    // Wait for playback to start and finish (max 10 minutes).
    let start = Instant::now();
    let mut seen = false;
    while start.elapsed() < Duration::from_secs(600) {
        std::thread::sleep(Duration::from_millis(50));
        let playing = !core.engine.now_playing().is_empty();
        seen |= playing;
        if (seen && !playing) || (!seen && start.elapsed() > Duration::from_secs(10)) {
            break;
        }
    }
    core.shutdown();
    if seen {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn serve(paths: Paths) -> ExitCode {
    let core = AppCore::start(paths);
    let c2 = core.clone();
    let server = match ipc::serve(Box::new(move |l| c2.handle_line(l))) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            core.shutdown();
            return ExitCode::FAILURE;
        }
    };
    let c3: Arc<AppCore> = core.clone();
    let mut hotkeys =
        clipdeck_platform::hotkeys::Hotkeys::new(Arc::new(move |id: &str| c3.on_hotkey(id)));
    for (id, e) in hotkeys.set_bindings(&core.bindings()) {
        tracing::warn!(id, e, "hotkey not registered");
    }
    if !hotkeys.note().is_empty() {
        eprintln!("{}", hotkeys.note());
    }
    core.warm_cache();
    eprintln!(
        "Clipdeck is running without a window. Virtual mic: {}. Stop with `clipdeck quit`.",
        clipdeck_platform::mic_hint()
    );
    let signals = core.signals();
    while let Ok(s) = signals.recv() {
        match s {
            UiSignal::Quit => break,
            UiSignal::HotkeysChanged => {
                for (id, e) in hotkeys.set_bindings(&core.bindings()) {
                    tracing::warn!(id, e, "hotkey not registered");
                }
            }
            UiSignal::Notice(n) => eprintln!("{n}"),
            _ => {}
        }
    }
    drop(server);
    core.shutdown();
    ExitCode::SUCCESS
}

mod bench;
mod micfx;

fn run_app(paths: Paths, minimized: bool) -> ExitCode {
    if ipc::is_running() {
        let _ = ipc::request("show");
        return ExitCode::SUCCESS;
    }
    match clipdeck_ui::run(paths, minimized) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(%e, "fatal");
            eprintln!("{e:#}");
            ExitCode::FAILURE
        }
    }
}
