//! Hook watchdog.
//!
//! A low-level keyboard hook whose callback exceeds `LowLevelHooksTimeout` is
//! *silently removed* by Windows with no notification (Win7+). Our callback is
//! tiny, but a transient system stall could still trip it, leaving the daemon
//! alive but deaf until restart. This thread guards against that: every couple
//! of seconds it injects a heartbeat and checks the hook's liveness counter
//! advanced; if not, it asks the hook thread (which owns the message loop) to
//! reinstall the hook via a posted thread message.

use std::time::Duration;

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW;

use crate::{hook, input};

/// Thread message asking the hook thread to reinstall the hook.
pub const WM_APP_REINSTALL: u32 = 0x8000 + 1;

pub fn spawn(hook_thread_id: u32) {
    std::thread::Builder::new()
        .name("watchdog".into())
        .spawn(move || loop {
            std::thread::sleep(Duration::from_secs(2));
            let before = hook::heartbeat_count();
            input::heartbeat();
            std::thread::sleep(Duration::from_millis(60));
            if hook::heartbeat_count() == before {
                tracing::warn!("keyboard hook unresponsive; requesting reinstall");
                unsafe {
                    let _ =
                        PostThreadMessageW(hook_thread_id, WM_APP_REINSTALL, WPARAM(0), LPARAM(0));
                }
            }
        })
        .expect("spawn watchdog thread");
}
