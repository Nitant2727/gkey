//! Low-level keyboard hook (WH_KEYBOARD_LL).
//!
//! The callback runs on the thread that installs it and is on the critical
//! input path: it must return well within `LowLevelHooksTimeout` (capped at
//! 1000 ms since Win10 1709, after which Windows *silently* removes the hook).
//! So it does the minimum — classify the key, decide grab vs pass-through from a
//! lockless snapshot, and hand grabbed events to the engine over a bounded
//! channel. All real work happens on the engine thread.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use crossbeam_channel::Sender;
use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, SetWindowsHookExW, UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT, WH_KEYBOARD_LL,
    WM_KEYUP, WM_SYSKEYUP,
};

use gkey_core::keys::KeyCode;

use crate::input::{HEARTBEAT_SIGNATURE, INJECT_SIGNATURE};
use crate::state;

const LLKHF_EXTENDED: u32 = 0x01;
const HC_ACTION: i32 = 0;

/// Incremented on every callback invocation. The watchdog injects a heartbeat
/// and checks this advances; if not, the hook was silently removed.
static HEARTBEAT: AtomicU64 = AtomicU64::new(0);

pub fn heartbeat_count() -> u64 {
    HEARTBEAT.load(Ordering::Relaxed)
}

/// A grabbed physical key event forwarded to the engine.
#[derive(Debug, Clone, Copy)]
pub struct KeyEvent {
    pub key: KeyCode,
    pub up: bool,
}

thread_local! {
    static SENDER: RefCell<Option<Sender<KeyEvent>>> = const { RefCell::new(None) };
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code != HC_ACTION {
        return CallNextHookEx(HHOOK::default(), code, wparam, lparam);
    }

    // Proof of life for the watchdog: this runs iff the hook is still installed.
    HEARTBEAT.fetch_add(1, Ordering::Relaxed);

    let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);

    // Watchdog heartbeat: counted above; swallow so it never reaches the OS.
    if kb.dwExtraInfo == HEARTBEAT_SIGNATURE {
        return LRESULT(1);
    }

    // Skip events we injected ourselves (identified by our dwExtraInfo marker)
    // to prevent the remap output from re-entering this hook.
    if kb.dwExtraInfo == INJECT_SIGNATURE {
        return CallNextHookEx(HHOOK::default(), code, wparam, lparam);
    }

    let extended = kb.flags.0 & LLKHF_EXTENDED != 0;
    let key = KeyCode::new(kb.scanCode as u16, extended);
    let up = wparam.0 as u32 == WM_KEYUP || wparam.0 as u32 == WM_SYSKEYUP;

    if state::should_grab(key) {
        SENDER.with(|s| {
            if let Some(tx) = s.borrow().as_ref() {
                // Non-blocking: never stall the input path if the engine is behind.
                let _ = tx.try_send(KeyEvent { key, up });
            }
        });
        return LRESULT(1); // swallow the key
    }

    CallNextHookEx(HHOOK::default(), code, wparam, lparam)
}

/// Install the hook on the current thread. The returned handle keeps it alive;
/// this thread must run a message loop for the callback to fire.
pub fn install(tx: Sender<KeyEvent>) -> Result<HHOOK> {
    SENDER.with(|s| *s.borrow_mut() = Some(tx));
    let hmod = unsafe { GetModuleHandleW(None)? };
    let hook =
        unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), HINSTANCE(hmod.0), 0)? };
    Ok(hook)
}

/// Reinstall the hook after a suspected silent removal. Must run on the same
/// thread that called [`install`] (the SENDER thread-local is already set there).
/// Returns the new handle, or the old one if reinstalling fails.
pub fn reinstall(old: HHOOK) -> HHOOK {
    unsafe {
        let _ = UnhookWindowsHookEx(old);
        let Ok(hmod) = GetModuleHandleW(None) else {
            return old;
        };
        SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), HINSTANCE(hmod.0), 0).unwrap_or(old)
    }
}
