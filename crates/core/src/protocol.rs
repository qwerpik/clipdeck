//! Remote-control protocol shared by the app's IPC server and the `clipdeck` CLI.
//!
//! One request line in, one response out. Two grammars are accepted:
//!
//! * Clipdeck: `play <ref> [--mic|--monitor]`, `stop [<ref>]`, `stop-all`, `pause`,
//!   `volume <0-100>`, `list`, `status`, `show`, `hide`, `toggle`, `reload`, `quit`, `ping`
//! * Soundpad (subset of `\\.\pipe\sp_remote_control`, see `re/findings.md` Q9):
//!   `DoPlaySound(i[,spk,mic])`, `DoStopSound()`, `DoTogglePause()`, `DoPlayRandomSound()`,
//!   `SetVolume(v)`, `GetVolume()`, `GetSoundFileCount()`, `GetPlayStatus()`, `IsAlive()`,
//!   `GetVersion()`, `GetSoundlist()`.
//!
//! Responses start with `OK` / `ERR` for the Clipdeck grammar and with Soundpad's `R-200`-style
//! codes or raw values for the Soundpad grammar.

use crate::model::Route;

/// A remote-control request.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// Play a sound by reference (id, `#position`, name or fuzzy query).
    Play {
        /// Reference.
        target: String,
        /// Optional route override.
        route: Option<Route>,
    },
    /// Play the sound at a 1-based position (Soundpad index).
    PlayIndex {
        /// 1-based index.
        index: usize,
        /// Route override.
        route: Option<Route>,
    },
    /// Play a random sound.
    PlayRandom,
    /// Play the next (+1) or previous (-1) sound in the list.
    Step(i32),
    /// Stop one sound, or everything when `None`.
    Stop(Option<String>),
    /// Toggle pause of everything.
    TogglePause,
    /// Master volume 0–100.
    SetVolume(u8),
    /// Query master volume.
    GetVolume,
    /// List sounds.
    List,
    /// Count sounds.
    Count,
    /// Playback status line.
    Status,
    /// Show / focus the window, optionally on a page
    /// (`library`, `favorites`, `recent`, `categories`, `settings`).
    Show(Option<String>),
    /// Hide to tray.
    Hide,
    /// Toggle window visibility.
    Toggle,
    /// Reload library and config from disk.
    Reload,
    /// Quit the app.
    Quit,
    /// Liveness check.
    Ping,
    /// Version string.
    Version,
    /// Soundpad `GetSoundlist()` XML.
    SoundlistXml,
    /// Toggle microphone passthrough.
    ToggleMic,
    /// Toggle monitoring.
    ToggleMonitor,
    /// Start recording (`None` = the configured source).
    StartRecording(Option<crate::config::RecordSource>),
    /// Stop recording and save.
    StopRecording,
    /// Recording position in ms (0 when not recording).
    RecordingPosition,
    /// Move the playing sound to `ms` (or by `ms` when `relative`).
    Seek {
        /// Milliseconds.
        ms: i64,
        /// Relative jump instead of an absolute position.
        relative: bool,
    },
    /// Position of the playing sound in ms.
    PlaybackPosition,
    /// Length of the playing sound in ms.
    PlaybackDuration,
}

/// Which grammar the request used, so the response can match it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    /// Clipdeck text commands.
    Clipdeck,
    /// Soundpad `DoXxx()` calls.
    Soundpad,
}

/// Parse error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProtocolError {
    /// Unknown command.
    #[error("unknown command `{0}`")]
    Unknown(String),
    /// Bad arguments.
    #[error("{0}")]
    BadArgs(String),
}

/// Parse one request line.
pub fn parse(line: &str) -> Result<(Request, Dialect), ProtocolError> {
    let line = line.trim();
    if let Some(open) = line.find('(')
        && line.ends_with(')')
        && line[..open].chars().all(|c| c.is_ascii_alphanumeric())
        && !line[..open].is_empty()
    {
        return parse_soundpad(&line[..open], &line[open + 1..line.len() - 1])
            .map(|r| (r, Dialect::Soundpad));
    }
    parse_clipdeck(line).map(|r| (r, Dialect::Clipdeck))
}

fn parse_route(words: &mut Vec<String>) -> Option<Route> {
    let mut route = None;
    words.retain(|w| match w.as_str() {
        "--mic" | "--mic-only" => {
            route = Some(Route::MicOnly);
            false
        }
        "--monitor" | "--me" | "--monitor-only" => {
            route = Some(Route::MonitorOnly);
            false
        }
        "--both" => {
            route = Some(Route::Both);
            false
        }
        _ => true,
    });
    route
}

fn parse_clipdeck(line: &str) -> Result<Request, ProtocolError> {
    let (cmd, rest) = match line.split_once(char::is_whitespace) {
        Some((c, r)) => (c, r.trim()),
        None => (line, ""),
    };
    let mut words: Vec<String> = split_args(rest);
    let req = match cmd.to_ascii_lowercase().as_str() {
        "play" => {
            let route = parse_route(&mut words);
            let target = words.join(" ");
            if target.is_empty() {
                return Err(ProtocolError::BadArgs(
                    "play needs a sound name, id or #position".into(),
                ));
            }
            Request::Play { target, route }
        }
        "random" => Request::PlayRandom,
        "next" => Request::Step(1),
        "previous" | "prev" => Request::Step(-1),
        "stop" => Request::Stop((!words.is_empty()).then(|| words.join(" "))),
        "stop-all" | "stopall" => Request::Stop(None),
        "pause" | "resume" | "toggle-pause" => Request::TogglePause,
        "volume" | "vol" => match words.first() {
            None => Request::GetVolume,
            Some(v) => Request::SetVolume(parse_percent(v)?),
        },
        "list" | "ls" => Request::List,
        "count" => Request::Count,
        "status" => Request::Status,
        "show" => Request::Show(words.first().cloned()),
        "hide" => Request::Hide,
        "toggle" => Request::Toggle,
        "reload" => Request::Reload,
        "quit" | "exit" => Request::Quit,
        "ping" => Request::Ping,
        "version" => Request::Version,
        "mic" => Request::ToggleMic,
        "monitor" => Request::ToggleMonitor,
        "record" | "rec" => match words.first().map(|w| w.to_ascii_lowercase()).as_deref() {
            None => Request::StartRecording(None),
            Some("stop") => Request::StopRecording,
            Some("status" | "position") => Request::RecordingPosition,
            Some("speakers" | "output" | "hear" | "loopback") => {
                Request::StartRecording(Some(crate::config::RecordSource::Speakers))
            }
            Some("mic" | "microphone" | "input") => {
                Request::StartRecording(Some(crate::config::RecordSource::Microphone))
            }
            Some(w) => {
                return Err(ProtocolError::BadArgs(format!(
                    "record what? `{w}` (use speakers, mic or stop)"
                )));
            }
        },
        "stop-recording" => Request::StopRecording,
        "seek" => {
            let w = words.first().ok_or_else(|| {
                ProtocolError::BadArgs("seek <seconds> or seek +/-<seconds>".into())
            })?;
            let relative = w.starts_with('+') || w.starts_with('-');
            let secs: f64 = w
                .parse()
                .map_err(|_| ProtocolError::BadArgs(format!("not a number of seconds: `{w}`")))?;
            Request::Seek {
                ms: (secs * 1000.0) as i64,
                relative,
            }
        }
        "" => return Err(ProtocolError::Unknown(String::new())),
        other => return Err(ProtocolError::Unknown(other.into())),
    };
    Ok(req)
}

fn parse_percent(v: &str) -> Result<u8, ProtocolError> {
    let n: u32 = v
        .trim_end_matches('%')
        .parse()
        .map_err(|_| ProtocolError::BadArgs(format!("volume must be 0-100, got `{v}`")))?;
    if n > 100 {
        return Err(ProtocolError::BadArgs(
            "Volume must be between 0 and 100.".into(),
        ));
    }
    Ok(n as u8)
}

fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for ch in s.chars() {
        match (quote, ch) {
            (None, '"' | '\'') => quote = Some(ch),
            (Some(q), c) if c == q => quote = None,
            (None, c) if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            (_, c) => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn parse_bool(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

fn route_from(spk: bool, mic: bool) -> Option<Route> {
    match (spk, mic) {
        (true, true) => Some(Route::Both),
        (true, false) => Some(Route::MonitorOnly),
        (false, true) => Some(Route::MicOnly),
        (false, false) => None,
    }
}

fn parse_soundpad(name: &str, args: &str) -> Result<Request, ProtocolError> {
    let args: Vec<&str> = if args.trim().is_empty() {
        Vec::new()
    } else {
        args.split(',').collect()
    };
    let num = |i: usize| -> Result<i64, ProtocolError> {
        args.get(i)
            .and_then(|a| a.trim().parse::<i64>().ok())
            .ok_or_else(|| ProtocolError::BadArgs("Expects param to be a number.".into()))
    };
    let req = match name {
        "DoPlaySound" => {
            if args.len() != 1 && args.len() != 3 {
                return Err(ProtocolError::BadArgs("Expects 1 or 3 params.".into()));
            }
            let index = num(0)?;
            if index < 1 {
                return Err(ProtocolError::BadArgs("Index must be 1 or higher.".into()));
            }
            let route = if args.len() == 3 {
                let spk = parse_bool(args[1]).unwrap_or(true);
                let mic = parse_bool(args[2]).unwrap_or(true);
                route_from(spk, mic)
            } else {
                None
            };
            Request::PlayIndex {
                index: index as usize,
                route,
            }
        }
        "DoPlayRandomSound" => Request::PlayRandom,
        "DoPlayNextSound" => Request::Step(1),
        "DoPlayPreviousSound" => Request::Step(-1),
        "DoStopSound" => Request::Stop(None),
        "DoTogglePause" => Request::TogglePause,
        "SetVolume" => {
            let v = num(0)?;
            if !(0..=100).contains(&v) {
                return Err(ProtocolError::BadArgs(
                    "Volume must be between 0 and 100.".into(),
                ));
            }
            Request::SetVolume(v as u8)
        }
        "GetVolume" => Request::GetVolume,
        "GetSoundFileCount" => Request::Count,
        "GetPlayStatus" => Request::Status,
        "IsAlive" => Request::Ping,
        "GetVersion" | "GetRemoteControlVersion" => Request::Version,
        "GetSoundlist" => Request::SoundlistXml,
        "DoStartRecording" => Request::StartRecording(None),
        "DoStartRecordingSpeakers" => {
            Request::StartRecording(Some(crate::config::RecordSource::Speakers))
        }
        "DoStartRecordingMicrophone" => {
            Request::StartRecording(Some(crate::config::RecordSource::Microphone))
        }
        "DoStopRecording" => Request::StopRecording,
        "GetRecordingPositionInMs" => Request::RecordingPosition,
        "DoSeekMs" => Request::Seek {
            ms: num(0)?,
            relative: false,
        },
        "DoJumpMs" => Request::Seek {
            ms: num(0)?,
            relative: true,
        },
        "GetPlaybackPositionInMs" => Request::PlaybackPosition,
        "GetPlaybackDurationInMs" => Request::PlaybackDuration,
        other => return Err(ProtocolError::Unknown(other.into())),
    };
    Ok(req)
}

/// Format an error response for the dialect.
pub fn error_response(dialect: Dialect, err: &str) -> String {
    match dialect {
        Dialect::Clipdeck => format!("ERR {err}"),
        Dialect::Soundpad => format!("R-400: {err}"),
    }
}

/// Socket / pipe name used by the app.
pub const SOCKET_NAME: &str = "clipdeck.sock";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipdeck_grammar() {
        assert_eq!(
            parse("play air horn --mic").unwrap().0,
            Request::Play {
                target: "air horn".into(),
                route: Some(Route::MicOnly)
            }
        );
        assert_eq!(
            parse("play \"#3\"").unwrap().0,
            Request::Play {
                target: "#3".into(),
                route: None
            }
        );
        assert_eq!(parse("stop").unwrap().0, Request::Stop(None));
        assert_eq!(parse("volume 40%").unwrap().0, Request::SetVolume(40));
        assert!(parse("volume 400").is_err());
        assert!(parse("play").is_err());
        assert!(matches!(parse("explode"), Err(ProtocolError::Unknown(_))));
    }

    #[test]
    fn next_and_previous() {
        assert_eq!(parse("next").unwrap().0, Request::Step(1));
        assert_eq!(parse("prev").unwrap().0, Request::Step(-1));
        assert_eq!(parse("DoPlayNextSound()").unwrap().0, Request::Step(1));
        assert_eq!(parse("DoPlayPreviousSound()").unwrap().0, Request::Step(-1));
    }

    #[test]
    fn recording_and_seek() {
        use crate::config::RecordSource;
        assert_eq!(parse("record").unwrap().0, Request::StartRecording(None));
        assert_eq!(
            parse("record mic").unwrap().0,
            Request::StartRecording(Some(RecordSource::Microphone))
        );
        assert_eq!(parse("record stop").unwrap().0, Request::StopRecording);
        assert_eq!(
            parse("DoStartRecordingSpeakers()").unwrap().0,
            Request::StartRecording(Some(RecordSource::Speakers))
        );
        assert_eq!(
            parse("DoStopRecording()").unwrap().0,
            Request::StopRecording
        );
        assert_eq!(
            parse("seek 1.5").unwrap().0,
            Request::Seek {
                ms: 1500,
                relative: false
            }
        );
        assert_eq!(
            parse("seek -5").unwrap().0,
            Request::Seek {
                ms: -5000,
                relative: true
            }
        );
        assert_eq!(
            parse("DoJumpMs(-2000)").unwrap().0,
            Request::Seek {
                ms: -2000,
                relative: true
            }
        );
        assert!(parse("record banana").is_err());
    }

    #[test]
    fn soundpad_grammar() {
        assert_eq!(
            parse("DoPlaySound(5,true,false)").unwrap(),
            (
                Request::PlayIndex {
                    index: 5,
                    route: Some(Route::MonitorOnly)
                },
                Dialect::Soundpad
            )
        );
        assert_eq!(
            parse("DoPlaySound(1)").unwrap().0,
            Request::PlayIndex {
                index: 1,
                route: None
            }
        );
        assert_eq!(parse("DoStopSound()").unwrap().0, Request::Stop(None));
        assert_eq!(parse("SetVolume(30)").unwrap().0, Request::SetVolume(30));
        assert!(parse("SetVolume(300)").is_err());
        assert!(parse("DoPlaySound(1,2)").is_err());
        assert_eq!(parse("IsAlive()").unwrap().0, Request::Ping);
    }
}
