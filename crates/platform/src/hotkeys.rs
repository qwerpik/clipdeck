//! Global hotkeys.
//!
//! * Windows and X11: `global-hotkey` (`RegisterHotKey` / XGrabKey).
//! * Wayland: the XDG `GlobalShortcuts` portal (KDE, GNOME 48+, Hyprland via
//!   xdg-desktop-portal-hyprland). Compositors that lack the portal can bind keys to the CLI
//!   instead (`clipdeck play …`), see the README.

use std::collections::HashMap;
use std::sync::Arc;

use clipdeck_core::hotkey::{Hotkey, Modifiers};
use parking_lot::Mutex;

/// One registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// Action id, e.g. `sound-12` or `stop-all`.
    pub id: String,
    /// Key combination.
    pub hotkey: Hotkey,
    /// Shown in the portal's dialog.
    pub description: String,
}

/// Called with the binding id when a hotkey is pressed (on an arbitrary thread).
pub type Callback = Arc<dyn Fn(&str) + Send + Sync>;

/// Which mechanism is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    /// `RegisterHotKey` / XGrabKey.
    Native,
    /// XDG GlobalShortcuts portal.
    Portal,
    /// Nothing works here; use the CLI.
    Unavailable,
}

/// The hotkey service. Create it on the UI thread (Windows needs that thread's message loop).
pub struct Hotkeys {
    kind: BackendKind,
    native: Option<Native>,
    #[cfg(target_os = "linux")]
    portal: Option<portal::Portal>,
    #[cfg(target_os = "linux")]
    x11: Option<X11Worker>,
    note: String,
}

/// X11: registration runs on its own thread so a slow or partly failing grab (global-hotkey's
/// X11 backend answers on a 50 ms poll) never blocks the UI.
#[cfg(target_os = "linux")]
struct X11Worker {
    tx: crossbeam_channel::Sender<Vec<Binding>>,
    status: Arc<Mutex<String>>,
}

#[cfg(target_os = "linux")]
impl X11Worker {
    fn start(callback: Callback) -> Result<X11Worker, String> {
        let (tx, rx) = crossbeam_channel::unbounded::<Vec<Binding>>();
        let (ready_tx, ready_rx) = crossbeam_channel::bounded::<Result<(), String>>(1);
        let status = Arc::new(Mutex::new(String::new()));
        let st = status.clone();
        std::thread::Builder::new()
            .name("clipdeck-x11-hotkeys".into())
            .spawn(move || {
                let mut native = match Native::new(callback) {
                    Ok(n) => {
                        let _ = ready_tx.send(Ok(()));
                        n
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                while let Ok(mut b) = rx.recv() {
                    // Coalesce bursts of updates.
                    while let Ok(newer) = rx.try_recv() {
                        b = newer;
                    }
                    let errors = native.set(&b);
                    *st.lock() = match errors.as_slice() {
                        [] => String::new(),
                        [(_, e)] => e.clone(),
                        [(_, e), rest @ ..] => format!("{e} (+{} more)", rest.len()),
                    };
                    for (id, e) in errors {
                        tracing::warn!(id, e, "hotkey not registered");
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        ready_rx.recv().map_err(|e| e.to_string())??;
        Ok(X11Worker { tx, status })
    }
}

impl std::fmt::Debug for Hotkeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hotkeys").field("kind", &self.kind).finish()
    }
}

/// Whether we run inside a Wayland session.
pub fn is_wayland() -> bool {
    cfg!(target_os = "linux")
        && (std::env::var_os("WAYLAND_DISPLAY").is_some()
            || std::env::var("XDG_SESSION_TYPE").is_ok_and(|v| v == "wayland"))
}

impl Hotkeys {
    /// Pick a backend and route presses to `callback`.
    pub fn new(callback: Callback) -> Hotkeys {
        #[cfg(target_os = "linux")]
        if is_wayland() && std::env::var_os("CLIPDECK_X11_HOTKEYS").is_none() {
            return match portal::Portal::start(callback) {
                Ok(p) => Hotkeys {
                    kind: BackendKind::Portal,
                    native: None,
                    portal: Some(p),
                    x11: None,
                    note: "Wayland: hotkeys go through the GlobalShortcuts portal".into(),
                },
                Err(e) => Hotkeys {
                    kind: BackendKind::Unavailable,
                    native: None,
                    portal: None,
                    x11: None,
                    note: format!(
                        "No GlobalShortcuts portal ({e}). Bind keys to `clipdeck play …` in your compositor."
                    ),
                },
            };
        }
        #[cfg(target_os = "linux")]
        return match X11Worker::start(callback) {
            Ok(w) => Hotkeys {
                kind: BackendKind::Native,
                native: None,
                portal: None,
                x11: Some(w),
                note: String::new(),
            },
            Err(e) => Hotkeys {
                kind: BackendKind::Unavailable,
                native: None,
                portal: None,
                x11: None,
                note: format!("Global hotkeys unavailable: {e}"),
            },
        };
        #[cfg(not(target_os = "linux"))]
        match Native::new(callback) {
            Ok(n) => Hotkeys {
                kind: BackendKind::Native,
                native: Some(n),
                note: String::new(),
            },
            Err(e) => Hotkeys {
                kind: BackendKind::Unavailable,
                native: None,
                note: format!("Global hotkeys unavailable: {e}"),
            },
        }
    }

    /// Active backend.
    pub fn kind(&self) -> BackendKind {
        self.kind
    }

    /// Human-readable note about the backend (empty when all is well).
    pub fn note(&self) -> String {
        #[cfg(target_os = "linux")]
        if let Some(p) = self.portal.as_ref() {
            return p.status();
        }
        #[cfg(target_os = "linux")]
        if let Some(w) = self.x11.as_ref() {
            return w.status.lock().clone();
        }
        self.note.clone()
    }

    /// Replace all registrations. Returns `(binding id, error)` for those that failed
    /// (usually: the key is taken by another app).
    pub fn set_bindings(&mut self, bindings: &[Binding]) -> Vec<(String, String)> {
        if let Some(n) = self.native.as_mut() {
            return n.set(bindings);
        }
        #[cfg(target_os = "linux")]
        if let Some(p) = self.portal.as_ref() {
            p.set(bindings.to_vec());
            return Vec::new();
        }
        #[cfg(target_os = "linux")]
        if let Some(w) = self.x11.as_ref() {
            let _ = w.tx.send(bindings.to_vec());
            return Vec::new();
        }
        Vec::new()
    }
}

/// `global-hotkey` code name for a canonical key.
pub fn code_name(key: &str) -> String {
    let k = key;
    if k.len() == 1 && k.as_bytes()[0].is_ascii_uppercase() {
        return format!("Key{k}");
    }
    if k.len() == 1 && k.as_bytes()[0].is_ascii_digit() {
        return format!("Digit{k}");
    }
    if let Some(rest) = k.strip_prefix("Num") {
        return format!("Numpad{rest}");
    }
    match k {
        "Up" | "Down" | "Left" | "Right" => format!("Arrow{k}"),
        other => other.to_string(),
    }
}

fn to_native(h: &Hotkey) -> Result<global_hotkey::hotkey::HotKey, String> {
    use global_hotkey::hotkey::Modifiers as M;
    let mut mods = M::empty();
    if h.mods.contains(Modifiers::CTRL) {
        mods |= M::CONTROL;
    }
    if h.mods.contains(Modifiers::ALT) {
        mods |= M::ALT;
    }
    if h.mods.contains(Modifiers::SHIFT) {
        mods |= M::SHIFT;
    }
    if h.mods.contains(Modifiers::SUPER) {
        mods |= M::META;
    }
    let code: global_hotkey::hotkey::Code = code_name(&h.key)
        .parse()
        .map_err(|_| format!("key {} is not supported", h.key))?;
    Ok(global_hotkey::hotkey::HotKey::new(
        (!mods.is_empty()).then_some(mods),
        code,
    ))
}

struct Native {
    manager: global_hotkey::GlobalHotKeyManager,
    registered: Vec<global_hotkey::hotkey::HotKey>,
    map: Arc<Mutex<HashMap<u32, String>>>,
}

impl Native {
    fn new(callback: Callback) -> Result<Native, String> {
        let manager = global_hotkey::GlobalHotKeyManager::new().map_err(|e| e.to_string())?;
        let map: Arc<Mutex<HashMap<u32, String>>> = Arc::default();
        let m2 = map.clone();
        global_hotkey::GlobalHotKeyEvent::set_event_handler(Some(
            move |ev: global_hotkey::GlobalHotKeyEvent| {
                if ev.state == global_hotkey::HotKeyState::Pressed {
                    let id = m2.lock().get(&ev.id).cloned();
                    if let Some(id) = id {
                        callback(&id);
                    }
                }
            },
        ));
        Ok(Native {
            manager,
            registered: Vec::new(),
            map,
        })
    }

    fn set(&mut self, bindings: &[Binding]) -> Vec<(String, String)> {
        if !self.registered.is_empty() {
            let _ = self.manager.unregister_all(&self.registered);
        }
        self.registered.clear();
        let mut map = self.map.lock();
        map.clear();
        let mut errors = Vec::new();
        let mut keys = Vec::new();
        for b in bindings {
            match to_native(&b.hotkey) {
                Ok(hk) => keys.push((hk, b)),
                Err(e) => errors.push((b.id.clone(), e)),
            }
        }
        // One batched call (the X11 backend answers each request on a 50 ms poll); only when
        // something is taken do we fall back to one-by-one to report exactly which keys failed.
        let all: Vec<_> = keys.iter().map(|(hk, _)| *hk).collect();
        if self.manager.register_all(&all).is_ok() {
            for (hk, b) in &keys {
                map.insert(hk.id(), b.id.clone());
            }
            self.registered = all;
            return errors;
        }
        let _ = self.manager.unregister_all(&all);
        for (hk, b) in keys {
            match self.manager.register(hk) {
                Ok(()) => {
                    map.insert(hk.id(), b.id.clone());
                    self.registered.push(hk);
                }
                Err(e) => errors.push((b.id.clone(), format!("{} is taken: {e}", b.hotkey))),
            }
        }
        errors
    }
}

/// XDG shortcut trigger string (`CTRL+SHIFT+a`) for portal preferred triggers.
pub fn portal_trigger(h: &Hotkey) -> String {
    let mut s = String::new();
    for (m, n) in [
        (Modifiers::CTRL, "CTRL+"),
        (Modifiers::ALT, "ALT+"),
        (Modifiers::SHIFT, "SHIFT+"),
        (Modifiers::SUPER, "LOGO+"),
    ] {
        if h.mods.contains(m) {
            s.push_str(n);
        }
    }
    let k = &h.key;
    let key = if k.len() == 1 {
        k.to_ascii_lowercase()
    } else if let Some(rest) = k.strip_prefix("Num") {
        match rest {
            "Multiply" => "KP_Multiply".into(),
            "Add" => "KP_Add".into(),
            "Subtract" => "KP_Subtract".into(),
            "Decimal" => "KP_Decimal".into(),
            "Divide" => "KP_Divide".into(),
            d => format!("KP_{d}"),
        }
    } else {
        match k.as_str() {
            "Space" => "space".into(),
            "Enter" => "Return".into(),
            "PageUp" => "Prior".into(),
            "PageDown" => "Next".into(),
            "Backquote" => "grave".into(),
            "Minus" => "minus".into(),
            "Equal" => "equal".into(),
            "Comma" => "comma".into(),
            "Period" => "period".into(),
            "Slash" => "slash".into(),
            "Semicolon" => "semicolon".into(),
            "Quote" => "apostrophe".into(),
            "BracketLeft" => "bracketleft".into(),
            "BracketRight" => "bracketright".into(),
            "Backslash" => "backslash".into(),
            "PrintScreen" => "Print".into(),
            "ScrollLock" => "Scroll_Lock".into(),
            "CapsLock" => "Caps_Lock".into(),
            other => other.to_string(),
        }
    };
    s.push_str(&key);
    s
}

#[cfg(target_os = "linux")]
mod portal {
    use super::{Binding, Callback, portal_trigger};
    use ashpd::desktop::CreateSessionOptions;
    use ashpd::desktop::global_shortcuts::{BindShortcutsOptions, GlobalShortcuts, NewShortcut};
    use futures_util::StreamExt;

    pub struct Portal {
        tx: tokio::sync::mpsc::UnboundedSender<Vec<Binding>>,
        status: std::sync::Arc<parking_lot::Mutex<String>>,
    }

    const REFUSED: &str = "The desktop's GlobalShortcuts portal refused Clipdeck's hotkeys (it needs Clipdeck installed with its .desktop file). You can still bind keys in your compositor, e.g. Hyprland: bind = CTRL, F1, exec, clipdeck play \"air horn\"";

    impl Portal {
        pub fn start(callback: Callback) -> Result<Portal, String> {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<Binding>>();
            let status = std::sync::Arc::new(parking_lot::Mutex::new(String::from(
                "Wayland: hotkeys go through the desktop's GlobalShortcuts portal.",
            )));
            let st = status.clone();
            let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
            std::thread::Builder::new()
                .name("clipdeck-portal".into())
                .spawn(move || {
                    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                        Ok(rt) => rt,
                        Err(e) => {
                            let _ = ready_tx.send(Err(e.to_string()));
                            return;
                        }
                    };
                    rt.block_on(async move {
                        // Host (non-Flatpak) apps must tell the portal who they are.
                        if let Ok(id) = ashpd::AppID::try_from(crate::APP_ID)
                            && let Err(e) = ashpd::register_host_app(id).await
                        {
                            tracing::debug!(%e, "portal host registration failed");
                        }
                        let portal = match GlobalShortcuts::new().await {
                            Ok(p) => p,
                            Err(e) => {
                                let _ = ready_tx.send(Err(e.to_string()));
                                return;
                            }
                        };
                        let mut activated = match portal.receive_activated().await {
                            Ok(s) => s,
                            Err(e) => {
                                let _ = ready_tx.send(Err(e.to_string()));
                                return;
                            }
                        };
                        let _ = ready_tx.send(Ok(()));
                        let mut session = None;
                        loop {
                            tokio::select! {
                                update = rx.recv() => {
                                    let Some(bindings) = update else { break };
                                    if let Some(s) = session.take() {
                                        let s: ashpd::desktop::Session<GlobalShortcuts> = s;
                                        let _ = s.close().await;
                                    }
                                    if bindings.is_empty() {
                                        continue;
                                    }
                                    let s = match portal.create_session(CreateSessionOptions::default()).await {
                                        Ok(s) => s,
                                        Err(e) => {
                                            tracing::warn!(%e, "portal session failed");
                                            *st.lock() = REFUSED.into();
                                            continue;
                                        }
                                    };
                                    let shortcuts: Vec<NewShortcut> = bindings
                                        .iter()
                                        .map(|b| {
                                            let trig = portal_trigger(&b.hotkey);
                                            NewShortcut::new(b.id.clone(), b.description.clone()).preferred_trigger(Some(trig.as_str()))
                                        })
                                        .collect();
                                    match portal.bind_shortcuts(&s, &shortcuts, None, BindShortcutsOptions::default()).await {
                                        Ok(req) => match req.response() {
                                            Ok(r) => {
                                                *st.lock() = format!(
                                                    "Wayland: {} hotkey(s) registered with the desktop portal. On Hyprland, bind them with `bind = CTRL, F1, global, {}:<id>` (see `hyprctl globalshortcuts`).",
                                                    r.shortcuts().len(),
                                                    crate::APP_ID
                                                );
                                            }
                                            Err(e) => {
                                                tracing::warn!(%e, "portal refused shortcuts");
                                                *st.lock() = REFUSED.into();
                                            }
                                        },
                                        Err(e) => {
                                            tracing::warn!(%e, "binding shortcuts failed");
                                            *st.lock() = REFUSED.into();
                                        }
                                    }
                                    session = Some(s);
                                }
                                ev = activated.next() => {
                                    match ev {
                                        Some(a) => callback(a.shortcut_id()),
                                        None => break,
                                    }
                                }
                            }
                        }
                    });
                })
                .map_err(|e| e.to_string())?;
            match ready_rx.recv_timeout(std::time::Duration::from_secs(3)) {
                Ok(Ok(())) => Ok(Portal { tx, status }),
                Ok(Err(e)) => Err(e),
                Err(_) => Err("portal did not answer".into()),
            }
        }

        pub fn set(&self, bindings: Vec<Binding>) {
            let _ = self.tx.send(bindings);
        }

        pub fn status(&self) -> String {
            self.status.lock().clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_names_parse_in_global_hotkey() {
        for (k, _) in clipdeck_core::hotkey::KEYS {
            let h = Hotkey {
                mods: Modifiers::CTRL,
                key: (*k).to_string(),
            };
            assert!(to_native(&h).is_ok(), "{k} → {}", code_name(k));
        }
    }

    #[test]
    fn triggers() {
        let h: Hotkey = "Ctrl+Shift+A".parse().unwrap();
        assert_eq!(portal_trigger(&h), "CTRL+SHIFT+a");
        let h: Hotkey = "Super+Num5".parse().unwrap();
        assert_eq!(portal_trigger(&h), "LOGO+KP_5");
    }
}
