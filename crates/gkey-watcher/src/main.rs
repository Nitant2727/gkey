//! gkey crash-restore watcher.
//!
//! Launched by the daemon with the daemon's PID as its argument. It opens the
//! daemon process and blocks until it exits, then reads the daemon's
//! hidden-window state file and un-hides any windows still hidden — so windows
//! parked on inactive workspaces are never orphaned by a daemon crash or hard
//! kill. On a clean daemon shutdown the state file is emptied first, so the
//! watcher restores nothing.

#![windows_subsystem = "windows"] // no console window

use std::ffi::c_void;

use windows::Win32::Foundation::{CloseHandle, FALSE, HWND};
use windows::Win32::System::Threading::{
    OpenProcess, WaitForSingleObject, INFINITE, PROCESS_ACCESS_RIGHTS,
};
use windows::Win32::UI::WindowsAndMessaging::{IsWindow, ShowWindow, SW_SHOW};

const SYNCHRONIZE: PROCESS_ACCESS_RIGHTS = PROCESS_ACCESS_RIGHTS(0x0010_0000);

fn main() {
    let pid: u32 = match std::env::args().nth(1).and_then(|s| s.parse().ok()) {
        Some(p) if p != 0 => p,
        _ => return,
    };

    // Wait for the daemon to exit.
    unsafe {
        let Ok(handle) = OpenProcess(SYNCHRONIZE, FALSE, pid) else {
            return;
        };
        WaitForSingleObject(handle, INFINITE);
        let _ = CloseHandle(handle);
    }

    // Restore any windows the daemon left hidden.
    let path = gkey_core::hidden_state_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let mut restored = 0;
    for line in text.lines() {
        let Ok(hi) = line.trim().parse::<isize>() else {
            continue;
        };
        let hwnd = HWND(hi as *mut c_void);
        unsafe {
            if IsWindow(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_SHOW);
                restored += 1;
            }
        }
    }
    let _ = restored;
}
