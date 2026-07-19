//! Synthetic input via SendInput.
//!
//! Every event we inject is tagged with [`INJECT_SIGNATURE`] in `dwExtraInfo`
//! so our own keyboard hook can recognise and skip it (avoids infinite
//! recursion when a remap re-injects keys). This is the mousemaster approach;
//! it is finer-grained than filtering all `LLKHF_INJECTED` events, because it
//! lets us still observe input injected by *other* tools.

use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, MapVirtualKeyW, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE,
    KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE,
    MAPVK_VSC_TO_VK_EX, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEEVENTF_WHEEL, MOUSEINPUT, MOUSE_EVENT_FLAGS,
};
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos};
use windows::Win32::Foundation::POINT;

use gkey_core::keys::KeyCode;

/// Marker written to `dwExtraInfo` on every event we synthesise ("GKEY").
pub const INJECT_SIGNATURE: usize = 0x_47_4B_45_59;

/// Marker for watchdog heartbeat events ("GKHB"): the hook recognises these,
/// bumps its liveness counter, and swallows them (they never reach the OS).
pub const HEARTBEAT_SIGNATURE: usize = 0x_47_4B_48_42;

#[derive(Clone, Copy)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

fn send(inputs: &[INPUT]) {
    // SendInput returns the number of events inserted; a short count means the
    // input stream was blocked (e.g. by a higher-integrity foreground window).
    let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        tracing::warn!(
            "SendInput inserted {}/{} events (blocked?)",
            sent,
            inputs.len()
        );
    }
}

/// Press or release a physical key by scancode.
pub fn key(code: KeyCode, up: bool) {
    let mut flags = KEYEVENTF_SCANCODE;
    if code.extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: Default::default(),
                wScan: code.scancode,
                dwFlags: KEYBD_EVENT_FLAGS(flags.0),
                time: 0,
                dwExtraInfo: INJECT_SIGNATURE,
            },
        },
    };
    send(&[input]);
}

/// Tap a key (press then release).
#[allow(dead_code)] // used once the remap grammar emits key sequences
pub fn tap(code: KeyCode) {
    key(code, false);
    key(code, true);
}

/// Inject a watchdog heartbeat: a tagged scancode-0 down/up pair. A live hook
/// recognises the signature, bumps its counter, and swallows it; a dead hook
/// lets it reach the OS, where scancode 0 maps to nothing and is harmless.
pub fn heartbeat() {
    let make = |up: bool| {
        let mut flags = KEYEVENTF_SCANCODE;
        if up {
            flags |= KEYEVENTF_KEYUP;
        }
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: Default::default(),
                    wScan: 0,
                    dwFlags: KEYBD_EVENT_FLAGS(flags.0),
                    time: 0,
                    dwExtraInfo: HEARTBEAT_SIGNATURE,
                },
            },
        }
    };
    send(&[make(false), make(true)]);
}

fn mouse(flags: MOUSE_EVENT_FLAGS, data: i32) {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: data as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: INJECT_SIGNATURE,
            },
        },
    };
    send(&[input]);
}

/// Is this physical key currently held down, per the OS? Used to reconcile
/// stuck state (e.g. a key-up we missed because Win+L locked the desktop first).
pub fn is_physically_down(code: KeyCode) -> bool {
    unsafe {
        let vk = MapVirtualKeyW(code.scancode as u32, MAPVK_VSC_TO_VK_EX);
        if vk == 0 {
            return false;
        }
        (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0
    }
}

pub fn cursor_pos() -> (i32, i32) {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    (p.x, p.y)
}

/// Move the cursor by a pixel delta. Uses SetCursorPos (absolute) so the
/// system's pointer-acceleration curve does not distort the motion.
pub fn move_cursor_by(dx: i32, dy: i32) {
    let (x, y) = cursor_pos();
    unsafe {
        let _ = SetCursorPos(x + dx, y + dy);
    }
}

/// Move the cursor to an absolute screen position (physical pixels).
pub fn set_cursor(x: i32, y: i32) {
    unsafe {
        let _ = SetCursorPos(x, y);
    }
}

pub fn click(button: MouseButton) {
    let (down, up) = match button {
        MouseButton::Left => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
        MouseButton::Middle => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
        MouseButton::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
    };
    mouse(down, 0);
    mouse(up, 0);
}

/// Vertical wheel; positive scrolls up. `notches` are in wheel clicks.
pub fn scroll_vertical(notches: i32) {
    mouse(MOUSEEVENTF_WHEEL, notches * 120);
}

/// Horizontal wheel; positive scrolls right.
#[allow(dead_code)] // wired to a binding in a later milestone
pub fn scroll_horizontal(notches: i32) {
    mouse(MOUSEEVENTF_HWHEEL, notches * 120);
}
