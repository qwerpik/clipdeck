//! `clipdeck micfx …`: manage the Windows in-microphone effect (no virtual cable needed).

#[cfg(not(windows))]
use std::process::ExitCode;

use clap::Subcommand;

/// `clipdeck micfx` actions.
#[derive(Subcommand, Debug)]
pub enum MicfxCmd {
    /// Show microphones, where the effect is installed and what is running.
    Status,
    /// Put the effect on these microphones (names, parts of names, endpoint GUIDs or
    /// "default") and remove it from all others. Asks for administrator rights.
    Install {
        /// Microphones; defaults to the system default microphone.
        devices: Vec<String>,
        /// Internal: where the elevated process reports its result.
        #[arg(long, hide = true)]
        result_file: Option<std::path::PathBuf>,
    },
    /// Remove the effect from every microphone and undo all system changes.
    Uninstall {
        /// Internal: where the elevated process reports its result.
        #[arg(long, hide = true)]
        result_file: Option<std::path::PathBuf>,
    },
    /// Play a 440 Hz tone through the effect and check that it comes out of the microphone.
    Test {
        /// Microphone to test (defaults to the first one with the effect).
        device: Option<String>,
        /// Seconds to listen.
        #[arg(long, default_value_t = 3)]
        seconds: u64,
    },
}

#[cfg(not(windows))]
pub fn run(_cmd: MicfxCmd) -> ExitCode {
    println!(
        "The in-microphone effect is Windows only. On Linux, select \"Clipdeck Mic\" as the \
         microphone in Discord."
    );
    ExitCode::SUCCESS
}

#[cfg(windows)]
pub use imp::run;

#[cfg(windows)]
mod imp {
    use std::path::Path;
    use std::process::ExitCode;
    use std::time::{Duration, Instant};

    use clipdeck_platform::ipc;
    use clipdeck_platform::windows::micfx::{self, MicEndpoint};

    use super::MicfxCmd;

    pub fn run(cmd: MicfxCmd) -> ExitCode {
        match cmd {
            MicfxCmd::Status => status(),
            MicfxCmd::Install {
                devices,
                result_file,
            } => elevated(
                &format!("micfx install {}", quote(&devices)),
                result_file.as_deref(),
                || install(&devices),
            ),
            MicfxCmd::Uninstall { result_file } => {
                if !micfx::is_installed() && result_file.is_none() {
                    println!("The in-mic effect is not installed.");
                    return ExitCode::SUCCESS;
                }
                elevated("micfx uninstall", result_file.as_deref(), micfx::uninstall)
            }
            MicfxCmd::Test { device, seconds } => test(device.as_deref(), seconds),
        }
    }

    fn quote(args: &[String]) -> String {
        args.iter()
            .map(|a| format!("\"{}\"", a.replace('"', "")))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Run `work` here when elevated; otherwise relaunch `clipdeck <args>` through UAC and
    /// relay its result.
    fn elevated(
        args: &str,
        result_file: Option<&Path>,
        work: impl FnOnce() -> Result<(), String>,
    ) -> ExitCode {
        if micfx::is_admin() {
            let r = work();
            let msg = match &r {
                Ok(()) => "OK".to_string(),
                Err(e) => format!("ERR {e}"),
            };
            if let Some(f) = result_file {
                let _ = std::fs::write(f, &msg);
            }
            return match r {
                Ok(()) => {
                    println!("Done. Windows Audio was restarted.");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("{e}");
                    ExitCode::FAILURE
                }
            };
        }
        let file = std::env::temp_dir().join(format!("clipdeck-micfx-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&file);
        let full = format!("{args} --result-file \"{}\"", file.display());
        match micfx::run_elevated(&full) {
            Ok(code) => {
                let msg = std::fs::read_to_string(&file).unwrap_or_default();
                let _ = std::fs::remove_file(&file);
                if code == 0 && msg.starts_with("OK") {
                    println!("Done. Windows Audio was restarted.");
                    ExitCode::SUCCESS
                } else {
                    eprintln!(
                        "{}",
                        msg.strip_prefix("ERR ")
                            .unwrap_or("the administrator process failed")
                    );
                    ExitCode::FAILURE
                }
            }
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        }
    }

    fn default_mic_name() -> Option<String> {
        clipdeck_audio::output::input_devices()
            .into_iter()
            .find(|d| d.is_default)
            .map(|d| d.name)
    }

    /// Resolve user words to endpoints.
    pub fn resolve(words: &[String], all: &[MicEndpoint]) -> Result<Vec<MicEndpoint>, String> {
        let words: Vec<String> = if words.is_empty() {
            vec!["default".into()]
        } else {
            words.to_vec()
        };
        let mut out: Vec<MicEndpoint> = Vec::new();
        for w in &words {
            let needle = if w.eq_ignore_ascii_case("default") {
                default_mic_name().ok_or("Windows has no default microphone")?
            } else {
                w.clone()
            };
            let n = needle.to_lowercase();
            let found = all
                .iter()
                .find(|e| e.guid.eq_ignore_ascii_case(&needle) || e.name.to_lowercase() == n)
                .or_else(|| all.iter().find(|e| e.name.to_lowercase().contains(&n)))
                .or_else(|| {
                    all.iter()
                        .find(|e| n.contains(&e.name.to_lowercase()) && !e.name.is_empty())
                })
                .ok_or_else(|| format!("no microphone matches \"{needle}\""))?;
            if !out.iter().any(|e| e.guid == found.guid) {
                out.push(found.clone());
            }
        }
        Ok(out)
    }

    fn install(words: &[String]) -> Result<(), String> {
        let dll = micfx::bundled_dll()
            .ok_or_else(|| format!("{} is missing next to clipdeck.exe", micfx::DLL_NAME))?;
        let chosen = resolve(words, &micfx::endpoints())?;
        for e in &chosen {
            println!("installing on {}", e.name);
        }
        let guids: Vec<String> = chosen.into_iter().map(|e| e.guid).collect();
        micfx::install(&guids, &dll)
    }

    fn status() -> ExitCode {
        let eps = micfx::endpoints();
        if eps.is_empty() {
            println!("No microphones found.");
        }
        let live = micfx::live_instances();
        for e in &eps {
            let running = live
                .iter()
                .find(|l| l.info.endpoint.eq_ignore_ascii_case(&e.guid));
            println!(
                "{} {}{}{}",
                if e.installed { "[x]" } else { "[ ]" },
                e.name,
                if e.active { "" } else { " (unplugged)" },
                match running {
                    Some(l) => format!(
                        "  running: {} Hz, {} ch{}",
                        l.info.rate,
                        l.info.channels,
                        if l.status & clipdeck_micfx_proto::STATUS_ORIGINAL_LOADED != 0 {
                            ", device effect chained"
                        } else if l.status & clipdeck_micfx_proto::STATUS_ORIGINAL_FAILED != 0 {
                            ", device effect failed to load"
                        } else {
                            ""
                        }
                    ),
                    None if e.installed => "  idle (no app is using this microphone)".into(),
                    None => String::new(),
                }
            );
        }
        println!(
            "\n{}",
            if micfx::is_admin() {
                "(running as administrator)"
            } else {
                "Change with: clipdeck micfx install <microphone>"
            }
        );
        ExitCode::SUCCESS
    }

    /// Goertzel amplitude of `freq` in `x` (0..1 for a full-scale sine).
    fn tone_amplitude(x: &[f32], rate: f32, freq: f32) -> f32 {
        let w = 2.0 * std::f32::consts::PI * freq / rate;
        let c = 2.0 * w.cos();
        let (mut s1, mut s2) = (0f32, 0f32);
        for &v in x {
            let s = v + c * s1 - s2;
            s2 = s1;
            s1 = s;
        }
        let power = s1 * s1 + s2 * s2 - c * s1 * s2;
        2.0 * power.max(0.0).sqrt() / x.len().max(1) as f32
    }

    fn test(device: Option<&str>, seconds: u64) -> ExitCode {
        use clipdeck_audio::output::VirtualMic;
        if ipc::is_running() {
            eprintln!("Quit Clipdeck first (clipdeck quit): it is feeding the effect already.");
            return ExitCode::FAILURE;
        }
        let eps = micfx::endpoints();
        let target = match device {
            Some(d) => match resolve(&[d.to_string()], &eps) {
                Ok(mut v) => v.remove(0),
                Err(e) => {
                    eprintln!("{e}");
                    return ExitCode::FAILURE;
                }
            },
            None => match eps.iter().find(|e| e.installed && e.active) {
                Some(e) => e.clone(),
                None => {
                    eprintln!("The effect is not installed. Run: clipdeck micfx install");
                    return ExitCode::FAILURE;
                }
            },
        };
        if !target.installed {
            eprintln!("The effect is not installed on {}.", target.name);
            return ExitCode::FAILURE;
        }
        println!("testing {}", target.name);

        let mut phase = 0f32;
        let step = 2.0 * std::f32::consts::PI * 440.0 / 48_000.0;
        let mut mic = micfx::MicFxMic::new();
        let render: clipdeck_audio::output::RenderFn = Box::new(move |out, ch| {
            for f in out.chunks_mut(ch.max(1)) {
                let v = 0.25 * phase.sin();
                phase = (phase + step) % (2.0 * std::f32::consts::PI);
                for s in f {
                    *s = v;
                }
            }
        });
        if let Err(e) = mic.start(render) {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
        // Recording from the microphone makes Windows build its pipeline and load the effect.
        let (input, mut ring) = match clipdeck_audio::output::open_input(Some(&target.name), 48_000)
        {
            Ok(v) => v,
            Err(e) => {
                eprintln!("cannot record from {}: {}", target.name, e.user_message());
                return ExitCode::FAILURE;
            }
        };
        println!("recording from {}", input.device);
        let started = Instant::now();
        let mut live = false;
        let mut samples: Vec<f32> = Vec::new();
        while started.elapsed() < Duration::from_secs(seconds + 2) {
            while let Ok(l) = ring.pop() {
                let _ = ring.pop();
                if live {
                    samples.push(l);
                }
            }
            if !live
                && micfx::live_instances()
                    .iter()
                    .any(|l| l.info.endpoint.eq_ignore_ascii_case(&target.guid))
            {
                live = true;
                println!("effect is running in the microphone");
            }
            if live && samples.len() as u64 >= seconds * 48_000 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        mic.stop();
        drop(input);
        if !live {
            eprintln!(
                "FAIL: Windows did not load the effect into {}. Check `clipdeck micfx status`; a \
                 restart of Windows can help.",
                target.name
            );
            return ExitCode::FAILURE;
        }
        // Skip the first 300 ms (pipeline start, ring fill).
        let tail = &samples[samples.len().min(14_400)..];
        let amp = tone_amplitude(tail, 48_000.0, 440.0);
        let off = tone_amplitude(tail, 48_000.0, 1234.0);
        let db = 20.0 * amp.max(1e-9).log10();
        println!("440 Hz in the microphone: {db:.1} dBFS (sent at -12.0 dBFS)");
        if amp > 0.05 && amp > off * 10.0 {
            println!("PASS");
            ExitCode::SUCCESS
        } else {
            eprintln!("FAIL: the tone did not come out of the microphone");
            ExitCode::FAILURE
        }
    }
}
