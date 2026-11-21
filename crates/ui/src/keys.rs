//! Map Slint key events to canonical hotkey strings.

use clipdeck_core::hotkey::{Hotkey, Modifiers, canonical_key};
use slint::platform::Key;

fn special(text: &str) -> Option<&'static str> {
    let table: &[(Key, &str)] = &[
        (Key::F1, "F1"),
        (Key::F2, "F2"),
        (Key::F3, "F3"),
        (Key::F4, "F4"),
        (Key::F5, "F5"),
        (Key::F6, "F6"),
        (Key::F7, "F7"),
        (Key::F8, "F8"),
        (Key::F9, "F9"),
        (Key::F10, "F10"),
        (Key::F11, "F11"),
        (Key::F12, "F12"),
        (Key::F13, "F13"),
        (Key::F14, "F14"),
        (Key::F15, "F15"),
        (Key::F16, "F16"),
        (Key::F17, "F17"),
        (Key::F18, "F18"),
        (Key::F19, "F19"),
        (Key::F20, "F20"),
        (Key::F21, "F21"),
        (Key::F22, "F22"),
        (Key::F23, "F23"),
        (Key::F24, "F24"),
        (Key::UpArrow, "Up"),
        (Key::DownArrow, "Down"),
        (Key::LeftArrow, "Left"),
        (Key::RightArrow, "Right"),
        (Key::Home, "Home"),
        (Key::End, "End"),
        (Key::PageUp, "PageUp"),
        (Key::PageDown, "PageDown"),
        (Key::Insert, "Insert"),
        (Key::Delete, "Delete"),
        (Key::Pause, "Pause"),
        (Key::ScrollLock, "ScrollLock"),
        (Key::SysReq, "PrintScreen"),
        (Key::Tab, "Tab"),
        (Key::Return, "Enter"),
        (Key::Space, "Space"),
        (Key::Backspace, "Backspace"),
        (Key::CapsLock, "CapsLock"),
    ];
    let ch = text.chars().next()?;
    table
        .iter()
        .find(|(k, _)| char::from(*k) == ch)
        .map(|(_, n)| *n)
}

/// Build a hotkey string from a Slint key event; `None` when the key is not bindable.
pub fn from_event(text: &str, ctrl: bool, alt: bool, shift: bool, meta: bool) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    let key = match special(text) {
        Some(k) => k,
        None => {
            // Shift changes the produced character (e.g. `!`); map back to the base key when we can.
            let base = match text {
                "!" => "1",
                "@" => "2",
                "#" => "3",
                "$" => "4",
                "%" => "5",
                "^" => "6",
                "&" => "7",
                "*" => "8",
                "(" => "9",
                ")" => "0",
                "_" => "-",
                "+" => "=",
                ":" => ";",
                "\"" => "'",
                "<" => ",",
                ">" => ".",
                "?" => "/",
                "~" => "`",
                "{" => "[",
                "}" => "]",
                "|" => "\\",
                other => other,
            };
            canonical_key(&base.to_uppercase())?
        }
    };
    let mut mods = Modifiers::default();
    if ctrl {
        mods = mods.with(Modifiers::CTRL);
    }
    if alt {
        mods = mods.with(Modifiers::ALT);
    }
    if shift {
        mods = mods.with(Modifiers::SHIFT);
    }
    if meta {
        mods = mods.with(Modifiers::SUPER);
    }
    Some(
        Hotkey {
            mods,
            key: key.to_string(),
        }
        .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_letters_function_keys_and_shifted_symbols() {
        assert_eq!(
            from_event("a", true, false, false, false).as_deref(),
            Some("Ctrl+A")
        );
        let f5: String = char::from(Key::F5).into();
        assert_eq!(
            from_event(&f5, false, true, false, false).as_deref(),
            Some("Alt+F5")
        );
        assert_eq!(
            from_event("!", false, false, true, false).as_deref(),
            Some("Shift+1")
        );
        assert_eq!(from_event("", false, false, false, false), None);
    }
}
