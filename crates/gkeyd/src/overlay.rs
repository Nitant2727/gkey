//! Hint overlay: a single click-through, top-most layered window spanning the
//! whole virtual desktop, onto which hint labels are painted.
//!
//! Transparency uses a colour key (`LWA_COLORKEY`) rather than per-pixel alpha,
//! because plain GDI text/fill calls don't write an alpha channel — with a
//! per-pixel-alpha layered window the labels would come out invisible. The key
//! colour is painted as the background and shows through as transparent; the
//! label boxes are opaque.
//!
//! The window is `WS_EX_TRANSPARENT`, so it never steals the mouse; we also hide
//! it before synthesising a click, so the click lands on the app beneath.

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, Sender};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateSolidBrush, DeleteObject, EndPaint, FillRect,
    GetTextExtentPoint32W, InvalidateRect, SelectObject, SetBkMode, SetTextColor, TextOutW,
    FW_BOLD, HFONT, HGDIOBJ, PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetSystemMetrics, RegisterClassW, SetLayeredWindowAttributes,
    ShowWindow, HMENU, LWA_COLORKEY, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN, SW_HIDE, SW_SHOWNOACTIVATE, WINDOW_EX_STYLE, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::hints::Hint;

/// Colour key painted as the transparent background (unlikely magenta).
const KEY: u32 = 0x00FF_00FF;
/// Label box fill (BGR): warm yellow.
const BOX_FILL: u32 = 0x0091_F0FF;
/// Label text colour (BGR): near-black.
const TEXT: u32 = 0x0020_2020;

fn colorref(bgr: u32) -> COLORREF {
    COLORREF(bgr)
}

pub enum UiCmd {
    /// Show these hints (also used to redraw a filtered subset).
    Show(Vec<Hint>),
    Hide,
    /// Briefly show a centered text toast at a physical screen point.
    Indicator {
        text: String,
        cx: i32,
        cy: i32,
    },
}

struct Indicator {
    text: String,
    cx: i32,
    cy: i32,
    expiry: Instant,
}

static OVERLAY_HWND: AtomicIsize = AtomicIsize::new(0);
static HINTS: Mutex<Vec<Hint>> = Mutex::new(Vec::new());
static INDICATOR: Mutex<Option<Indicator>> = Mutex::new(None);
/// Virtual-desktop origin, subtracted from physical hint coords when painting.
static ORIGIN: Mutex<(i32, i32)> = Mutex::new((0, 0));

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    const WM_PAINT: u32 = 0x000F;
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);

            // Background = key colour (transparent).
            let bg = CreateSolidBrush(colorref(KEY));
            FillRect(hdc, &ps.rcPaint, bg);
            let _ = DeleteObject(HGDIOBJ(bg.0));

            let (ox, oy) = *ORIGIN.lock().unwrap();
            let box_brush = CreateSolidBrush(colorref(BOX_FILL));
            let font: HFONT = CreateFontW(
                -18,
                0,
                0,
                0,
                FW_BOLD.0 as i32,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                PCWSTR(wide("Segoe UI").as_ptr()),
            );
            let old_font = SelectObject(hdc, HGDIOBJ(font.0));
            SetBkMode(hdc, TRANSPARENT);
            let _ = SetTextColor(hdc, colorref(TEXT));

            if let Ok(hints) = HINTS.lock() {
                for h in hints.iter() {
                    let text = wide(&h.label.to_uppercase());
                    let text_no_nul = &text[..text.len() - 1];
                    let mut sz = windows::Win32::Foundation::SIZE::default();
                    let _ = GetTextExtentPoint32W(hdc, text_no_nul, &mut sz);
                    let bx = h.x - ox;
                    let by = h.y - oy;
                    let rect = RECT {
                        left: bx - 3,
                        top: by - 2,
                        right: bx + sz.cx + 5,
                        bottom: by + sz.cy + 3,
                    };
                    FillRect(hdc, &rect, box_brush);
                    let _ = TextOutW(hdc, bx, by, text_no_nul);
                }
            }

            if let Ok(ind) = INDICATOR.lock() {
                if let Some(ind) = ind.as_ref() {
                    let text = wide(&ind.text);
                    let t = &text[..text.len() - 1];
                    let mut sz = windows::Win32::Foundation::SIZE::default();
                    let _ = GetTextExtentPoint32W(hdc, t, &mut sz);
                    let cx = ind.cx - ox;
                    let cy = ind.cy - oy;
                    let (pad_x, pad_y) = (14, 8);
                    let rect = RECT {
                        left: cx - sz.cx / 2 - pad_x,
                        top: cy - sz.cy / 2 - pad_y,
                        right: cx + sz.cx / 2 + pad_x,
                        bottom: cy + sz.cy / 2 + pad_y,
                    };
                    FillRect(hdc, &rect, box_brush);
                    let _ = TextOutW(hdc, cx - sz.cx / 2, cy - sz.cy / 2, t);
                }
            }

            SelectObject(hdc, old_font);
            let _ = DeleteObject(HGDIOBJ(font.0));
            let _ = DeleteObject(HGDIOBJ(box_brush.0));
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn create_window() -> HWND {
    unsafe {
        let hinst = GetModuleHandleW(None).expect("module handle");
        let class = wide("gkey_overlay");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        RegisterClassW(&wc);

        let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        *ORIGIN.lock().unwrap() = (vx, vy);

        let ex =
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW;
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(ex.0),
            PCWSTR(class.as_ptr()),
            PCWSTR(wide("gkey overlay").as_ptr()),
            WS_POPUP,
            vx,
            vy,
            vw,
            vh,
            None,
            HMENU::default(),
            hinst,
            None,
        )
        .expect("create overlay window");

        // Colour-key transparency.
        let _ = SetLayeredWindowAttributes(hwnd, colorref(KEY), 0, LWA_COLORKEY);
        let _ = ShowWindow(hwnd, SW_HIDE);
        hwnd
    }
}

fn pump(hwnd: HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
    };
    unsafe {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, hwnd, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Start the overlay UI thread; returns a channel to command it.
pub fn spawn() -> Sender<UiCmd> {
    let (tx, rx): (Sender<UiCmd>, Receiver<UiCmd>) = unbounded();
    std::thread::Builder::new()
        .name("overlay".into())
        .spawn(move || {
            let hwnd = create_window();
            OVERLAY_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
            loop {
                let mut dirty = false;
                while let Ok(cmd) = rx.try_recv() {
                    match cmd {
                        UiCmd::Show(hints) => {
                            *HINTS.lock().unwrap() = hints;
                            dirty = true;
                        }
                        UiCmd::Hide => {
                            HINTS.lock().unwrap().clear();
                            dirty = true;
                        }
                        UiCmd::Indicator { text, cx, cy } => {
                            *INDICATOR.lock().unwrap() = Some(Indicator {
                                text,
                                cx,
                                cy,
                                expiry: Instant::now() + Duration::from_millis(1200),
                            });
                            dirty = true;
                        }
                    }
                }
                // Expire the indicator.
                {
                    let mut ind = INDICATOR.lock().unwrap();
                    if ind.as_ref().is_some_and(|i| Instant::now() >= i.expiry) {
                        *ind = None;
                        dirty = true;
                    }
                }
                if dirty {
                    let show =
                        !HINTS.lock().unwrap().is_empty() || INDICATOR.lock().unwrap().is_some();
                    unsafe {
                        let _ = ShowWindow(hwnd, if show { SW_SHOWNOACTIVATE } else { SW_HIDE });
                        if show {
                            let _ = InvalidateRect(hwnd, None, true);
                        }
                    }
                }
                pump(hwnd);
                std::thread::sleep(Duration::from_millis(4));
            }
        })
        .expect("spawn overlay thread");
    tx
}
