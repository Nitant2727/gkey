//! Physical key identity based on set-1 scancodes.
//!
//! We match bindings on scancodes rather than virtual-key codes so behaviour is
//! independent of the active keyboard layout (the lesson from kanata's winIOv2
//! and warpd's Windows alpha, which broke on shifted/layout-dependent keys).

use std::collections::HashMap;
use std::sync::OnceLock;

/// A physical key: a set-1 scancode plus whether it carries the 0xE0 extended
/// prefix. `(0x1C, false)` is the main Enter, `(0x1C, true)` is numpad Enter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyCode {
    pub scancode: u16,
    pub extended: bool,
}

impl KeyCode {
    pub const fn new(scancode: u16, extended: bool) -> Self {
        Self { scancode, extended }
    }
}

/// Table of the key names we accept in config, mapped to their physical key.
/// Deliberately small for phase 1 — extended as bindings grow.
const NAMES: &[(&str, u16, bool)] = &[
    // Letters (set-1 scancodes).
    ("A", 0x1E, false),
    ("B", 0x30, false),
    ("C", 0x2E, false),
    ("D", 0x20, false),
    ("E", 0x12, false),
    ("F", 0x21, false),
    ("G", 0x22, false),
    ("H", 0x23, false),
    ("I", 0x17, false),
    ("J", 0x24, false),
    ("K", 0x25, false),
    ("L", 0x26, false),
    ("M", 0x32, false),
    ("N", 0x31, false),
    ("O", 0x18, false),
    ("P", 0x19, false),
    ("Q", 0x10, false),
    ("R", 0x13, false),
    ("S", 0x1F, false),
    ("T", 0x14, false),
    ("U", 0x16, false),
    ("V", 0x2F, false),
    ("W", 0x11, false),
    ("X", 0x2D, false),
    ("Y", 0x15, false),
    ("Z", 0x2C, false),
    // Digits.
    ("0", 0x0B, false),
    ("1", 0x02, false),
    ("2", 0x03, false),
    ("3", 0x04, false),
    ("4", 0x05, false),
    ("5", 0x06, false),
    ("6", 0x07, false),
    ("7", 0x08, false),
    ("8", 0x09, false),
    ("9", 0x0A, false),
    // Punctuation used as motions.
    ("Comma", 0x33, false),
    ("Period", 0x34, false),
    ("Semicolon", 0x27, false),
    ("Slash", 0x35, false),
    ("Minus", 0x0C, false),
    ("Equals", 0x0D, false),
    ("Space", 0x39, false),
    ("Tab", 0x0F, false),
    ("Backspace", 0x0E, false),
    ("Enter", 0x1C, false),
    // Control keys.
    ("Escape", 0x01, false),
    ("CapsLock", 0x3A, false),
    ("LeftShift", 0x2A, false),
    ("RightShift", 0x36, false),
    ("LeftControl", 0x1D, false),
    ("LeftAlt", 0x38, false),
    ("RightControl", 0x1D, true),
    ("RightAlt", 0x38, true),
    // Arrows (extended).
    ("Left", 0x4B, true),
    ("Right", 0x4D, true),
    ("Up", 0x48, true),
    ("Down", 0x50, true),
    ("Home", 0x47, true),
    ("End", 0x4F, true),
    ("PageUp", 0x49, true),
    ("PageDown", 0x51, true),
    ("Delete", 0x53, true),
    ("Insert", 0x52, true),
];

fn name_to_key() -> &'static HashMap<String, KeyCode> {
    static MAP: OnceLock<HashMap<String, KeyCode>> = OnceLock::new();
    MAP.get_or_init(|| {
        NAMES
            .iter()
            .map(|&(name, sc, ext)| (name.to_ascii_lowercase(), KeyCode::new(sc, ext)))
            .collect()
    })
}

fn key_to_name() -> &'static HashMap<KeyCode, &'static str> {
    static MAP: OnceLock<HashMap<KeyCode, &'static str>> = OnceLock::new();
    MAP.get_or_init(|| {
        NAMES
            .iter()
            .map(|&(name, sc, ext)| (KeyCode::new(sc, ext), name))
            .collect()
    })
}

/// Parse a config key name (case-insensitive) into a physical key.
pub fn parse(name: &str) -> Option<KeyCode> {
    name_to_key()
        .get(&name.trim().to_ascii_lowercase())
        .copied()
}

/// All accepted key names, in table order (for GUI dropdowns).
pub fn all_names() -> Vec<&'static str> {
    NAMES.iter().map(|&(name, _, _)| name).collect()
}

/// Human-readable name for a physical key, for logging.
pub fn name_of(key: KeyCode) -> String {
    key_to_name()
        .get(&key)
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            format!(
                "sc{:#04x}{}",
                key.scancode,
                if key.extended { "e" } else { "" }
            )
        })
}
