//! System tray icon for the daemon.
//!
//! Creates a hidden window on the caller's thread (which must pump messages)
//! and adds a notification-area icon. Right-click opens a menu (Settings /
//! Restore windows / Quit); double-click opens the settings GUI. Quit restores
//! any workspace-hidden windows and posts WM_QUIT so the message loop ends.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    GetCursorPos, LoadIconW, PostQuitMessage, RegisterClassW, SetForegroundWindow, TrackPopupMenu,
    HMENU, IDI_APPLICATION, MF_SEPARATOR, MF_STRING, TPM_RIGHTBUTTON, WINDOW_EX_STYLE, WM_COMMAND,
    WM_DESTROY, WM_LBUTTONDBLCLK, WM_RBUTTONUP, WNDCLASSW, WS_OVERLAPPED,
};

use crate::tiling;

const WM_APP_TRAY: u32 = 0x8000 + 20;
const TRAY_UID: u32 = 1;
const ID_SETTINGS: usize = 1;
const ID_RESTORE: usize = 2;
const ID_QUIT: usize = 3;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn base_nid(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_UID,
        ..Default::default()
    }
}

fn open_settings() {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("gkey-settings.exe")));
    if let Some(exe) = exe {
        if exe.exists() {
            let _ = std::process::Command::new(exe).spawn();
        }
    }
}

unsafe fn show_menu(hwnd: HWND) {
    let menu = match CreatePopupMenu() {
        Ok(m) => m,
        Err(_) => return,
    };
    let _ = AppendMenuW(menu, MF_STRING, ID_SETTINGS, w!("Settings"));
    let _ = AppendMenuW(menu, MF_STRING, ID_RESTORE, w!("Restore windows"));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, MF_STRING, ID_QUIT, w!("Quit gkey"));

    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    // Required so the menu dismisses correctly when clicking elsewhere.
    let _ = SetForegroundWindow(hwnd);
    let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, None);
    let _ = DestroyMenu(menu);
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_APP_TRAY => {
            let event = (lp.0 as u32) & 0xFFFF;
            match event {
                WM_RBUTTONUP => show_menu(hwnd),
                WM_LBUTTONDBLCLK => open_settings(),
                _ => {}
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            match (wp.0 & 0xFFFF) as usize {
                ID_SETTINGS => open_settings(),
                ID_RESTORE => tiling::restore_all(),
                ID_QUIT => {
                    tiling::restore_all();
                    let _ = DestroyWindow(hwnd);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = Shell_NotifyIconW(NIM_DELETE, &base_nid(hwnd));
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// Create the hidden tray window and add the icon. Call on the message-pumping
/// thread. Returns false if setup failed (daemon still runs, just no tray).
pub fn install() -> bool {
    unsafe {
        let Ok(hinst) = GetModuleHandleW(None) else {
            return false;
        };
        let class = wide("gkey_tray");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        RegisterClassW(&wc);

        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class.as_ptr()),
            w!("gkey"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            HMENU::default(),
            hinst,
            None,
        ) else {
            return false;
        };

        // Our embedded icon is resource id 1; fall back to the stock app icon.
        let hicon = LoadIconW(HINSTANCE(hinst.0), PCWSTR(1 as *const u16))
            .or_else(|_| LoadIconW(None, IDI_APPLICATION))
            .unwrap_or_default();
        let mut nid = base_nid(hwnd);
        nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        nid.uCallbackMessage = WM_APP_TRAY;
        nid.hIcon = hicon;
        let tip = wide("gkey — keyboard control");
        nid.szTip[..tip.len()].copy_from_slice(&tip);

        Shell_NotifyIconW(NIM_ADD, &nid).as_bool()
    }
}
