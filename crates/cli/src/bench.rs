//! `clipdeck bench`: measure trigger→render latency, cold start, idle CPU and RAM.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use clipdeck_core::paths::Paths;
use clipdeck_ui::app::AppCore;

fn write_click(path: &std::path::Path) -> std::io::Result<()> {
    let rate = 48_000u32;
    let frames: Vec<i16> = (0..rate / 4)
        .map(|i| {
            if i < 480 {
                ((i as f32 * 0.3).sin() * 12_000.0) as i16
            } else {
                0
            }
        })
        .collect();
    let data_len = (frames.len() * 2) as u32;
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&rate.to_le_bytes());
    b.extend_from_slice(&(rate * 2).to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    for s in frames {
        b.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, b)
}

/// Run the benchmark.
pub fn run(paths: Paths, file: Option<PathBuf>) -> ExitCode {
    let tmp = std::env::temp_dir().join(format!("clipdeck-bench-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    let file = match file {
        Some(f) => f,
        None => {
            let f = tmp.join("click.wav");
            if let Err(e) = write_click(&f) {
                eprintln!("{e}");
                return ExitCode::FAILURE;
            }
            f
        }
    };

    // Isolated profile so the bench never touches the user's library.
    // SAFETY: single-threaded at this point; set before any engine thread starts.
    unsafe { std::env::set_var("CLIPDECK_HOME", tmp.join("home")) };
    let _ = paths;
    let bench_paths = match Paths::resolve() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let core = AppCore::start(bench_paths);
    core.update_config(|c| {
        c.audio.master_volume = 0.0; // silent: we measure scheduling, not loudness
        c.audio.mic_passthrough = false;
    });
    std::thread::sleep(Duration::from_millis(500));
    let st = core.engine.device_status();
    println!(
        "devices: monitor={:?} virtual_mic={:?}",
        st.monitor, st.virtual_mic
    );

    core.engine.preload(&file);
    let stats = core.engine.stats();
    let mut samples = Vec::new();
    for _ in 0..20 {
        core.play_file(&file, None);
        std::thread::sleep(Duration::from_millis(120));
        let l = stats
            .last_latency_us
            .load(std::sync::atomic::Ordering::Relaxed);
        if l > 0 {
            samples.push(l);
        }
        core.engine.stop_all();
        std::thread::sleep(Duration::from_millis(30));
    }
    core.shutdown();
    samples.sort_unstable();
    if samples.is_empty() {
        println!("trigger→render latency: no audio output available");
    } else {
        let med = samples[samples.len() / 2];
        println!(
            "trigger→render latency (excl. device buffer): median {:.2} ms, best {:.2} ms, worst {:.2} ms (target < 20 ms)",
            f64::from(med) / 1000.0,
            f64::from(samples[0]) / 1000.0,
            f64::from(samples[samples.len() - 1]) / 1000.0
        );
    }

    // Cold start + idle numbers from a child GUI process in bench mode.
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("clipdeck"));
    let t0 = Instant::now();
    let child = Command::new(exe)
        .env("CLIPDECK_BENCH", "1")
        .env("CLIPDECK_HOME", tmp.join("home-gui"))
        .env("CLIPDECK_SOCKET", tmp.join("bench.sock"))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    match child {
        Ok(mut child) => {
            if let Some(out) = child.stdout.take() {
                for line in BufReader::new(out).lines().map_while(Result::ok) {
                    if line.starts_with("shown") {
                        println!(
                            "cold start → window shown: {} ms (target < 300 ms)",
                            t0.elapsed().as_millis()
                        );
                    } else if let Some(rest) = line.strip_prefix("idle ") {
                        println!("idle: {rest} (targets: ~0% CPU, < 60 MB)");
                    }
                }
            }
            let _ = child.wait();
        }
        Err(e) => println!("GUI start not measured: {e}"),
    }
    let _ = std::fs::remove_dir_all(&tmp);
    ExitCode::SUCCESS
}
