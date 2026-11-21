//! Hotkey model: `Ctrl+Alt+Shift+Super+Key`, with Win32 virtual-key conversion for `.spl`.

use std::fmt;
use std::str::FromStr;

/// Modifier set. Bit values match Win32 `RegisterHotKey` (`MOD_ALT`=1, `MOD_CONTROL`=2,
/// `MOD_SHIFT`=4, `MOD_WIN`=8), which is also what Soundpad stores in `keyModifiers`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Modifiers(u32);

impl Modifiers {
    /// Alt.
    pub const ALT: Modifiers = Modifiers(1);
    /// Control.
    pub const CTRL: Modifiers = Modifiers(2);
    /// Shift.
    pub const SHIFT: Modifiers = Modifiers(4);
    /// Super / Windows key.
    pub const SUPER: Modifiers = Modifiers(8);

    /// From raw bits (unknown bits dropped).
    pub fn from_bits(b: u32) -> Self {
        Modifiers(b & 0xF)
    }
    /// Raw bits.
    pub fn bits(self) -> u32 {
        self.0
    }
    /// Whether all of `other` is set.
    pub fn contains(self, other: Modifiers) -> bool {
        self.0 & other.0 == other.0
    }
    /// Union.
    pub fn with(self, other: Modifiers) -> Modifiers {
        Modifiers(self.0 | other.0)
    }
    /// No modifiers.
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// A hotkey: modifiers plus one canonical key name (see [`KEYS`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hotkey {
    /// Modifiers.
    pub mods: Modifiers,
    /// Canonical key name, e.g. `A`, `5`, `F1`, `Num3`, `Space`, `Up`.
    pub key: String,
}

/// Canonical key names and their Win32 virtual-key codes.
pub const KEYS: &[(&str, u32)] = &[
    ("Backspace", 0x08),
    ("Tab", 0x09),
    ("Enter", 0x0D),
    ("Pause", 0x13),
    ("CapsLock", 0x14),
    ("Escape", 0x1B),
    ("Space", 0x20),
    ("PageUp", 0x21),
    ("PageDown", 0x22),
    ("End", 0x23),
    ("Home", 0x24),
    ("Left", 0x25),
    ("Up", 0x26),
    ("Right", 0x27),
    ("Down", 0x28),
    ("PrintScreen", 0x2C),
    ("Insert", 0x2D),
    ("Delete", 0x2E),
    ("0", 0x30),
    ("1", 0x31),
    ("2", 0x32),
    ("3", 0x33),
    ("4", 0x34),
    ("5", 0x35),
    ("6", 0x36),
    ("7", 0x37),
    ("8", 0x38),
    ("9", 0x39),
    ("A", 0x41),
    ("B", 0x42),
    ("C", 0x43),
    ("D", 0x44),
    ("E", 0x45),
    ("F", 0x46),
    ("G", 0x47),
    ("H", 0x48),
    ("I", 0x49),
    ("J", 0x4A),
    ("K", 0x4B),
    ("L", 0x4C),
    ("M", 0x4D),
    ("N", 0x4E),
    ("O", 0x4F),
    ("P", 0x50),
    ("Q", 0x51),
    ("R", 0x52),
    ("S", 0x53),
    ("T", 0x54),
    ("U", 0x55),
    ("V", 0x56),
    ("W", 0x57),
    ("X", 0x58),
    ("Y", 0x59),
    ("Z", 0x5A),
    ("Num0", 0x60),
    ("Num1", 0x61),
    ("Num2", 0x62),
    ("Num3", 0x63),
    ("Num4", 0x64),
    ("Num5", 0x65),
    ("Num6", 0x66),
    ("Num7", 0x67),
    ("Num8", 0x68),
    ("Num9", 0x69),
    ("NumMultiply", 0x6A),
    ("NumAdd", 0x6B),
    ("NumSubtract", 0x6D),
    ("NumDecimal", 0x6E),
    ("NumDivide", 0x6F),
    ("F1", 0x70),
    ("F2", 0x71),
    ("F3", 0x72),
    ("F4", 0x73),
    ("F5", 0x74),
    ("F6", 0x75),
    ("F7", 0x76),
    ("F8", 0x77),
    ("F9", 0x78),
    ("F10", 0x79),
    ("F11", 0x7A),
    ("F12", 0x7B),
    ("F13", 0x7C),
    ("F14", 0x7D),
    ("F15", 0x7E),
    ("F16", 0x7F),
    ("F17", 0x80),
    ("F18", 0x81),
    ("F19", 0x82),
    ("F20", 0x83),
    ("F21", 0x84),
    ("F22", 0x85),
    ("F23", 0x86),
    ("F24", 0x87),
    ("ScrollLock", 0x91),
    ("Semicolon", 0xBA),
    ("Equal", 0xBB),
    ("Comma", 0xBC),
    ("Minus", 0xBD),
    ("Period", 0xBE),
    ("Slash", 0xBF),
    ("Backquote", 0xC0),
    ("BracketLeft", 0xDB),
    ("Backslash", 0xDC),
    ("BracketRight", 0xDD),
    ("Quote", 0xDE),
];

/// Canonicalize a user-typed key name (`esc` → `Escape`, `pgup` → `PageUp`, `kp5` → `Num5`).
pub fn canonical_key(name: &str) -> Option<&'static str> {
    let n = name.trim();
    if let Some((k, _)) = KEYS.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)) {
        return Some(k);
    }
    let lower = n.to_ascii_lowercase();
    let alias = match lower.as_str() {
        "esc" => "Escape",
        "return" => "Enter",
        "del" => "Delete",
        "ins" => "Insert",
        "pgup" | "prior" => "PageUp",
        "pgdn" | "pgdown" | "next" => "PageDown",
        "spacebar" | " " => "Space",
        "arrowup" => "Up",
        "arrowdown" => "Down",
        "arrowleft" => "Left",
        "arrowright" => "Right",
        ";" => "Semicolon",
        "=" => "Equal",
        "," => "Comma",
        "-" => "Minus",
        "." => "Period",
        "/" => "Slash",
        "`" => "Backquote",
        "[" => "BracketLeft",
        "\\" => "Backslash",
        "]" => "BracketRight",
        "'" => "Quote",
        _ => {
            for prefix in ["kp", "numpad", "keypad"] {
                if let Some(rest) = lower.strip_prefix(prefix)
                    && rest.len() == 1
                    && rest.as_bytes()[0].is_ascii_digit()
                {
                    return KEYS
                        .iter()
                        .find(|(k, _)| k.strip_prefix("Num") == Some(rest))
                        .map(|(k, _)| *k);
                }
            }
            if let Some(rest) = lower.strip_prefix("key")
                && rest.len() == 1
            {
                return canonical_key(rest);
            }
            if let Some(rest) = lower.strip_prefix("digit") {
                return canonical_key(rest);
            }
            return None;
        }
    };
    Some(alias)
}

/// Parse error for hotkeys.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid hotkey `{0}`")]
pub struct HotkeyParseError(pub String);

impl FromStr for Hotkey {
    type Err = HotkeyParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut mods = Modifiers::default();
        let mut key: Option<&'static str> = None;
        let parts: Vec<&str> = if s.trim() == "+" {
            vec!["+"]
        } else {
            s.split('+').collect()
        };
        for raw in parts {
            let p = raw.trim();
            match p.to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "ctl" => mods = mods.with(Modifiers::CTRL),
                "alt" | "option" => mods = mods.with(Modifiers::ALT),
                "shift" => mods = mods.with(Modifiers::SHIFT),
                "super" | "win" | "meta" | "cmd" | "logo" => mods = mods.with(Modifiers::SUPER),
                "" => return Err(HotkeyParseError(s.into())),
                _ => {
                    if key.is_some() {
                        return Err(HotkeyParseError(s.into()));
                    }
                    key = Some(canonical_key(p).ok_or_else(|| HotkeyParseError(s.into()))?);
                }
            }
        }
        let key = key.ok_or_else(|| HotkeyParseError(s.into()))?;
        Ok(Hotkey {
            mods,
            key: key.to_string(),
        })
    }
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.mods.contains(Modifiers::CTRL) {
            f.write_str("Ctrl+")?;
        }
        if self.mods.contains(Modifiers::ALT) {
            f.write_str("Alt+")?;
        }
        if self.mods.contains(Modifiers::SHIFT) {
            f.write_str("Shift+")?;
        }
        if self.mods.contains(Modifiers::SUPER) {
            f.write_str("Super+")?;
        }
        f.write_str(&self.key)
    }
}

impl Hotkey {
    /// Win32 `(modifiers, virtual-key)`.
    pub fn to_win32(&self) -> Option<(Modifiers, u32)> {
        KEYS.iter()
            .find(|(k, _)| *k == self.key)
            .map(|(_, vk)| (self.mods, *vk))
    }

    /// From Win32 modifier bits and virtual-key code.
    pub fn from_win32(mods: Modifiers, vk: u32) -> Option<Hotkey> {
        KEYS.iter().find(|(_, v)| *v == vk).map(|(k, _)| Hotkey {
            mods,
            key: (*k).to_string(),
        })
    }

    /// Whether a bare key without modifiers would steal normal typing.
    pub fn is_risky(&self) -> bool {
        self.mods.is_empty() && (self.key.len() == 1 || self.key == "Space" || self.key == "Enter")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_display_roundtrip() {
        for s in [
            "Ctrl+F1",
            "Ctrl+Alt+Shift+Super+A",
            "Num5",
            "Alt+Space",
            "Shift+Slash",
        ] {
            assert_eq!(s.parse::<Hotkey>().unwrap().to_string(), s);
        }
        assert_eq!(
            "control + alt + kp3".parse::<Hotkey>().unwrap().to_string(),
            "Ctrl+Alt+Num3"
        );
        assert_eq!(
            "alt+shift+KeyQ".parse::<Hotkey>().unwrap().to_string(),
            "Alt+Shift+Q"
        );
        assert!("Ctrl+".parse::<Hotkey>().is_err());
        assert!("Ctrl+A+B".parse::<Hotkey>().is_err());
        assert!("Hyper+A".parse::<Hotkey>().is_err());
    }

    #[test]
    fn win32_matches_soundpad_observation() {
        // Soundpad: keyModifiers="2" key="112" → "Ctrl+F1" (findings Q1).
        let h = Hotkey::from_win32(Modifiers::from_bits(2), 112).unwrap();
        assert_eq!(h.to_string(), "Ctrl+F1");
        assert_eq!(h.to_win32(), Some((Modifiers::CTRL, 112)));
        assert_eq!(
            Hotkey::from_win32(Modifiers::from_bits(4), 113)
                .unwrap()
                .to_string(),
            "Shift+F2"
        );
    }

    #[test]
    fn risky_keys() {
        assert!("A".parse::<Hotkey>().unwrap().is_risky());
        assert!(!"F5".parse::<Hotkey>().unwrap().is_risky());
        assert!(!"Ctrl+A".parse::<Hotkey>().unwrap().is_risky());
    }
}
