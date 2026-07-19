//! State shared between the hook thread and the engine thread.
//!
//! The hook callback runs on the system input path and must decide *synchronously*
//! whether to swallow each key. It reads this state locklessly: the current mode
//! is an atomic, and the set of keys to grab in idle mode is an `ArcSwap`
//! snapshot the engine can replace on config reload without blocking the hook.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;

use arc_swap::ArcSwap;
use once_cell_shim::Lazy;

use gkey_core::keys::KeyCode;

// Minimal Lazy shim so we don't add a dependency just for one static.
mod once_cell_shim {
    use std::sync::OnceLock;
    pub struct Lazy<T> {
        cell: OnceLock<T>,
        init: fn() -> T,
    }
    impl<T> Lazy<T> {
        pub const fn new(init: fn() -> T) -> Self {
            Self {
                cell: OnceLock::new(),
                init,
            }
        }
    }
    impl<T> std::ops::Deref for Lazy<T> {
        type Target = T;
        fn deref(&self) -> &T {
            self.cell.get_or_init(self.init)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Keys pass through untouched except configured remaps and the activation key.
    Idle = 0,
    /// Pointer-control mode: motions, clicks and scrolling; most keys swallowed.
    Normal = 1,
    /// Hint mode: an overlay shows labels; typed keys filter and select a target.
    Hint = 2,
}

static MODE: AtomicU8 = AtomicU8::new(Mode::Idle as u8);

/// Keys to grab while in idle mode (remap sources + the activation key).
static IDLE_GRAB: Lazy<ArcSwap<HashSet<KeyCode>>> =
    Lazy::new(|| ArcSwap::from_pointee(HashSet::new()));

/// Whether window events auto-retile the desktop.
static AUTO_TILING: AtomicBool = AtomicBool::new(false);

pub fn auto_tiling() -> bool {
    AUTO_TILING.load(Ordering::Relaxed)
}

pub fn set_auto_tiling(on: bool) {
    AUTO_TILING.store(on, Ordering::Relaxed);
}

pub fn mode() -> Mode {
    match MODE.load(Ordering::Relaxed) {
        0 => Mode::Idle,
        1 => Mode::Normal,
        _ => Mode::Hint,
    }
}

pub fn set_mode(mode: Mode) {
    MODE.store(mode as u8, Ordering::Relaxed);
}

pub fn set_idle_grab(keys: HashSet<KeyCode>) {
    IDLE_GRAB.store(Arc::new(keys));
}

/// Should the hook swallow this key (and forward it to the engine)?
pub fn should_grab(key: KeyCode) -> bool {
    match mode() {
        // In normal/hint mode everything is captured; unbound keys are dropped.
        Mode::Normal | Mode::Hint => true,
        // In idle mode only remap sources and the activation key are captured.
        Mode::Idle => IDLE_GRAB.load().contains(&key),
    }
}
