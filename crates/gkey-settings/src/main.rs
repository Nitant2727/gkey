//! gkey settings GUI — native Win32 (windows-rs).
//!
//! Edits gkey's TOML config with labelled dropdowns for every binding. Writes to
//! `%APPDATA%\gkey\config.toml`; a running `gkeyd` hot-reloads it, so changes
//! apply live. Save validates via `gkey_core` and refuses to write invalid
//! config. Native Win32 is used deliberately: it is the only GUI stack that
//! builds under this machine's Smart App Control policy (no new build scripts).

#![windows_subsystem = "windows"]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, Ordering};
use std::sync::{Mutex, OnceLock};

use gkey_core::config::{FloatRule, RawConfig};
use gkey_core::keys::{self, KeyCode};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{BOOL, CloseHandle, FALSE, HINSTANCE, HWND, LPARAM, LRESULT, TRUE, WPARAM};
use windows::Win32::Graphics::Gdi::{GetStockObject, DEFAULT_GUI_FONT};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    EnumChildWindows, GetDlgItem, GetDlgItemTextW, GetMessageW, KillTimer, LoadCursorW,
    PostMessageW, PostQuitMessage, RegisterClassW, SendMessageW, SetTimer, SetWindowsHookExW,
    SetWindowTextW, ShowWindow, TranslateMessage, UnhookWindowsHookEx, HHOOK, HMENU, IDC_ARROW,
    KBDLLHOOKSTRUCT, MSG, SW_SHOW, WH_KEYBOARD_LL, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE,
    WM_COMMAND, WM_DESTROY, WM_KEYDOWN, WM_SYSKEYDOWN, WM_TIMER, WNDCLASSW, WS_BORDER, WS_CAPTION,
    WS_CHILD, WS_MINIMIZEBOX, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

// Control-message/style constants (kept local to avoid feature-gated imports).
const CB_ADDSTRING: u32 = 0x0143;
const CB_SETCURSEL: u32 = 0x014E;
const CBS_DROPDOWNLIST: u32 = 0x0003;
const ES_AUTOHSCROLL: u32 = 0x0080;
const ES_NUMBER: u32 = 0x2000;
const BS_PUSHBUTTON: u32 = 0x0000;
const BS_AUTOCHECKBOX: u32 = 0x0003;
const BM_GETCHECK: u32 = 0x00F0;
const BM_SETCHECK: u32 = 0x00F5;
const WM_SETFONT: u32 = 0x0030;

// Control IDs.
const ID_ACT: i32 = 1000;
const ID_STEP: i32 = 1001;
const ID_FASTMUL: i32 = 1002;
const ID_SCROLL: i32 = 1003;
const ID_ALTGR: i32 = 1004;
const ID_AUTOTILE: i32 = 1005;
const ID_GAP: i32 = 1006;
const ID_OUTERGAP: i32 = 1007;
const ID_NUMKEYS: i32 = 1008;
const ID_ML: i32 = 1010;
const ID_MD: i32 = 1011;
const ID_MU: i32 = 1012;
const ID_MR: i32 = 1013;
const ID_FAST: i32 = 1014;
const ID_CL: i32 = 1015;
const ID_CM: i32 = 1016;
const ID_CR: i32 = 1017;
const ID_SU: i32 = 1018;
const ID_SD: i32 = 1019;
const ID_SL: i32 = 1020;
const ID_SRR: i32 = 1021;
const ID_HINT: i32 = 1022;
const ID_GRID: i32 = 1023;
const ID_NEXIT: i32 = 1024;
const ID_TILE: i32 = 1025;
const ID_TILECOL: i32 = 1026;
const ID_FOCUSN: i32 = 1027;
const ID_FOCUSP: i32 = 1028;
const ID_TOGGLETILE: i32 = 1029;
const ID_CHARS: i32 = 1030;
const ID_HEXIT: i32 = 1031;
const ID_HBS: i32 = 1032;
const ID_RGROW: i32 = 1033;
const ID_RSHRINK: i32 = 1034;
const ID_SWAPN: i32 = 1035;
const ID_SWAPP: i32 = 1036;
const ID_WSN: i32 = 1037;
const ID_WSP: i32 = 1038;
const ID_MWSN: i32 = 1039;
const ID_MWSP: i32 = 1040;
const ID_PROMOTE: i32 = 1041;
const ID_RM_FROM: i32 = 1040; // row i: from = ID_RM_FROM + i*2, to = +1
const ID_RM_DEL: i32 = 2000; // row i remove button = ID_RM_DEL + i
const ID_RM_ADD: i32 = 2100;
const ID_SAVE: i32 = 2101;
const ID_RELOAD: i32 = 2102;
const ID_DAEMON: i32 = 2103;
const ID_FLOAT_EXE: i32 = 3000; // row i: exe/class/title = ID_FLOAT_EXE + i*3 (+0/+1/+2)
const ID_FLOAT_DEL: i32 = 3500; // row i remove button = ID_FLOAT_DEL + i
const ID_FLOAT_ADD: i32 = 3600;
const MAX_REMAPS: usize = 40;
const MAX_FLOATS: usize = 30;
const DAEMON_TIMER: usize = 1;

/// A key field's "Set" (capture) button has id = field id + this offset.
const SET_OFFSET: i32 = 500;
/// Posted to the main window when a key is captured (wparam=scancode, lparam=extended).
const WM_APP_CAPTURED: u32 = 0x8000 + 1;
const LLKHF_EXTENDED: u32 = 0x01;
const ESC_SCANCODE: u16 = 0x01;

// Capture state (a temporary low-level hook feeds the main window one keypress).
static CAPTURING: AtomicBool = AtomicBool::new(false);
static CAPTURE_HOOK: AtomicIsize = AtomicIsize::new(0);
static CAPTURE_FIELD: AtomicI32 = AtomicI32::new(-1);
static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);

/// A key-name dropdown field bound to a config slot.
struct KField {
    id: i32,
    label: &'static str,
    get: fn(&RawConfig) -> String,
    set: fn(&mut RawConfig, String),
}

/// A text/number edit field bound to a config slot.
struct VField {
    id: i32,
    label: &'static str,
    numeric: bool,
    get: fn(&RawConfig) -> String,
    set: fn(&mut RawConfig, String),
}

/// A checkbox field bound to a boolean config slot.
struct CField {
    id: i32,
    label: &'static str,
    get: fn(&RawConfig) -> bool,
    set: fn(&mut RawConfig, bool),
}

enum Row {
    Section(&'static str),
    Key(KField),
    Val(VField),
    Check(CField),
}

fn layout() -> Vec<Row> {
    vec![
        Row::Section("General"),
        Row::Key(KField { id: ID_ACT, label: "Activation", get: |c| c.general.activation.clone(), set: |c, v| c.general.activation = v }),
        Row::Val(VField { id: ID_STEP, label: "Move step (px)", numeric: true, get: |c| c.general.move_step.to_string(), set: |c, v| c.general.move_step = v.parse().unwrap_or(c.general.move_step) }),
        Row::Val(VField { id: ID_FASTMUL, label: "Fast multiplier", numeric: true, get: |c| c.general.fast_multiplier.to_string(), set: |c, v| c.general.fast_multiplier = v.parse().unwrap_or(c.general.fast_multiplier) }),
        Row::Val(VField { id: ID_SCROLL, label: "Scroll amount", numeric: true, get: |c| c.general.scroll_amount.to_string(), set: |c, v| c.general.scroll_amount = v.parse().unwrap_or(c.general.scroll_amount) }),
        Row::Check(CField { id: ID_ALTGR, label: "Handle AltGr (swallow fake Ctrl)", get: |c| c.general.altgr, set: |c, v| c.general.altgr = v }),
        Row::Section("Normal mode"),
        Row::Key(KField { id: ID_ML, label: "Move left", get: |c| c.normal.move_left.clone(), set: |c, v| c.normal.move_left = v }),
        Row::Key(KField { id: ID_MD, label: "Move down", get: |c| c.normal.move_down.clone(), set: |c, v| c.normal.move_down = v }),
        Row::Key(KField { id: ID_MU, label: "Move up", get: |c| c.normal.move_up.clone(), set: |c, v| c.normal.move_up = v }),
        Row::Key(KField { id: ID_MR, label: "Move right", get: |c| c.normal.move_right.clone(), set: |c, v| c.normal.move_right = v }),
        Row::Key(KField { id: ID_FAST, label: "Faster (hold)", get: |c| c.normal.faster.clone(), set: |c, v| c.normal.faster = v }),
        Row::Key(KField { id: ID_CL, label: "Click left", get: |c| c.normal.click_left.clone(), set: |c, v| c.normal.click_left = v }),
        Row::Key(KField { id: ID_CM, label: "Click middle", get: |c| c.normal.click_middle.clone(), set: |c, v| c.normal.click_middle = v }),
        Row::Key(KField { id: ID_CR, label: "Click right", get: |c| c.normal.click_right.clone(), set: |c, v| c.normal.click_right = v }),
        Row::Key(KField { id: ID_SU, label: "Scroll up", get: |c| c.normal.scroll_up.clone(), set: |c, v| c.normal.scroll_up = v }),
        Row::Key(KField { id: ID_SD, label: "Scroll down", get: |c| c.normal.scroll_down.clone(), set: |c, v| c.normal.scroll_down = v }),
        Row::Key(KField { id: ID_SL, label: "Scroll left", get: |c| c.normal.scroll_left.clone(), set: |c, v| c.normal.scroll_left = v }),
        Row::Key(KField { id: ID_SRR, label: "Scroll right", get: |c| c.normal.scroll_right.clone(), set: |c, v| c.normal.scroll_right = v }),
        Row::Key(KField { id: ID_HINT, label: "Hint (elements)", get: |c| c.normal.hint.clone(), set: |c, v| c.normal.hint = v }),
        Row::Key(KField { id: ID_GRID, label: "Hint (grid)", get: |c| c.normal.grid.clone(), set: |c, v| c.normal.grid = v }),
        Row::Key(KField { id: ID_TILE, label: "Tile (BSP)", get: |c| c.normal.tile.clone(), set: |c, v| c.normal.tile = v }),
        Row::Key(KField { id: ID_TILECOL, label: "Tile (columns)", get: |c| c.normal.tile_columns.clone(), set: |c, v| c.normal.tile_columns = v }),
        Row::Key(KField { id: ID_FOCUSN, label: "Focus next", get: |c| c.normal.focus_next.clone(), set: |c, v| c.normal.focus_next = v }),
        Row::Key(KField { id: ID_FOCUSP, label: "Focus prev", get: |c| c.normal.focus_prev.clone(), set: |c, v| c.normal.focus_prev = v }),
        Row::Key(KField { id: ID_TOGGLETILE, label: "Toggle auto-tile", get: |c| c.normal.toggle_tiling.clone(), set: |c, v| c.normal.toggle_tiling = v }),
        Row::Key(KField { id: ID_RGROW, label: "Resize grow", get: |c| c.normal.resize_grow.clone(), set: |c, v| c.normal.resize_grow = v }),
        Row::Key(KField { id: ID_RSHRINK, label: "Resize shrink", get: |c| c.normal.resize_shrink.clone(), set: |c, v| c.normal.resize_shrink = v }),
        Row::Key(KField { id: ID_SWAPN, label: "Swap next", get: |c| c.normal.swap_next.clone(), set: |c, v| c.normal.swap_next = v }),
        Row::Key(KField { id: ID_SWAPP, label: "Swap prev", get: |c| c.normal.swap_prev.clone(), set: |c, v| c.normal.swap_prev = v }),
        Row::Key(KField { id: ID_WSN, label: "Workspace next", get: |c| c.normal.workspace_next.clone(), set: |c, v| c.normal.workspace_next = v }),
        Row::Key(KField { id: ID_WSP, label: "Workspace prev", get: |c| c.normal.workspace_prev.clone(), set: |c, v| c.normal.workspace_prev = v }),
        Row::Key(KField { id: ID_MWSN, label: "Move to ws next", get: |c| c.normal.move_workspace_next.clone(), set: |c, v| c.normal.move_workspace_next = v }),
        Row::Key(KField { id: ID_MWSP, label: "Move to ws prev", get: |c| c.normal.move_workspace_prev.clone(), set: |c, v| c.normal.move_workspace_prev = v }),
        Row::Key(KField { id: ID_PROMOTE, label: "Promote to master", get: |c| c.normal.promote.clone(), set: |c, v| c.normal.promote = v }),
        Row::Key(KField { id: ID_NEXIT, label: "Exit to idle", get: |c| c.normal.exit.clone(), set: |c, v| c.normal.exit = v }),
        Row::Section("Hint mode"),
        Row::Val(VField { id: ID_CHARS, label: "Label alphabet", numeric: false, get: |c| c.hint.chars.clone(), set: |c, v| c.hint.chars = v }),
        Row::Key(KField { id: ID_HEXIT, label: "Cancel", get: |c| c.hint.exit.clone(), set: |c, v| c.hint.exit = v }),
        Row::Key(KField { id: ID_HBS, label: "Backspace", get: |c| c.hint.backspace.clone(), set: |c, v| c.hint.backspace = v }),
        Row::Section("Tiling"),
        Row::Check(CField { id: ID_AUTOTILE, label: "Auto-tile on window open/close", get: |c| c.tiling.auto, set: |c, v| c.tiling.auto = v }),
        Row::Val(VField { id: ID_GAP, label: "Gap (px)", numeric: true, get: |c| c.tiling.gap.to_string(), set: |c, v| c.tiling.gap = v.parse().unwrap_or(c.tiling.gap) }),
        Row::Val(VField { id: ID_OUTERGAP, label: "Outer gap (px)", numeric: true, get: |c| c.tiling.outer_gap.to_string(), set: |c, v| c.tiling.outer_gap = v.parse().unwrap_or(c.tiling.outer_gap) }),
        Row::Check(CField { id: ID_NUMKEYS, label: "Keys 1-9 switch workspaces", get: |c| c.tiling.number_keys, set: |c, v| c.tiling.number_keys = v }),
    ]
}

struct AppState {
    raw: RawConfig,
    path: PathBuf,
    key_names: Vec<String>,
    remap_names: Vec<String>,
    /// Editable, ordered remap rows (RawConfig stores them unordered).
    remaps: Vec<(String, String)>,
}

static APP: OnceLock<Mutex<AppState>> = OnceLock::new();

fn remaps_from_raw(raw: &RawConfig) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> =
        raw.remap.iter().map(|(a, b)| (a.clone(), b.clone())).collect();
    v.sort();
    v
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn config_path() -> PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("gkey").join("config.toml")
}

// --- control helpers -------------------------------------------------------

unsafe fn make(
    class: &str,
    text: &str,
    style: u32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    parent: HWND,
    id: i32,
    hinst: windows::Win32::Foundation::HINSTANCE,
) -> HWND {
    let class_w = wide(class);
    let text_w = wide(text);
    CreateWindowExW(
        WINDOW_EX_STYLE(0),
        PCWSTR(class_w.as_ptr()),
        PCWSTR(text_w.as_ptr()),
        WINDOW_STYLE(style),
        x,
        y,
        w,
        h,
        parent,
        HMENU(id as isize as *mut core::ffi::c_void),
        hinst,
        None,
    )
    .unwrap_or_default()
}

unsafe fn combo_add(hwnd: HWND, names: &[String]) {
    for n in names {
        let nw = wide(n);
        SendMessageW(hwnd, CB_ADDSTRING, WPARAM(0), LPARAM(nw.as_ptr() as isize));
    }
}

unsafe fn combo_select(parent: HWND, id: i32, names: &[String], value: &str) {
    if let Ok(h) = GetDlgItem(parent, id) {
        let idx = names.iter().position(|n| n == value).unwrap_or(0);
        SendMessageW(h, CB_SETCURSEL, WPARAM(idx), LPARAM(0));
    }
}

unsafe fn get_text(parent: HWND, id: i32) -> String {
    let mut buf = [0u16; 256];
    let n = GetDlgItemTextW(parent, id, &mut buf);
    String::from_utf16_lossy(&buf[..n as usize])
}

unsafe fn set_edit(parent: HWND, id: i32, text: &str) {
    if let Ok(h) = GetDlgItem(parent, id) {
        let _ = SetWindowTextW(h, PCWSTR(wide(text).as_ptr()));
    }
}

// --- create / sync / read --------------------------------------------------

unsafe fn create_controls(parent: HWND, hinst: windows::Win32::Foundation::HINSTANCE) -> i32 {
    let st = APP.get().unwrap().lock().unwrap();
    let (lx, lw, cx, cw, rowh) = (12, 120, 140, 150, 26);
    let combo_style = WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | WS_VSCROLL.0 | CBS_DROPDOWNLIST;
    let label_style = WS_CHILD.0 | WS_VISIBLE.0;
    let mut y = 10;

    for row in layout() {
        match row {
            Row::Section(title) => {
                y += 6;
                make("STATIC", title, label_style, lx, y, 300, 20, parent, 0, hinst);
                y += 22;
            }
            Row::Key(k) => {
                make("STATIC", k.label, label_style, lx, y + 3, lw, 20, parent, 0, hinst);
                let h = make("COMBOBOX", "", combo_style, cx, y, cw, 220, parent, k.id, hinst);
                combo_add(h, &st.remap_names); // includes "(none)" for unbound
                let btn = WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_PUSHBUTTON;
                make("BUTTON", "Set", btn, cx + cw + 6, y, 44, 22, parent, k.id + SET_OFFSET, hinst);
                y += rowh;
            }
            Row::Val(v) => {
                make("STATIC", v.label, label_style, lx, y + 3, lw, 20, parent, 0, hinst);
                let mut es = WS_CHILD.0 | WS_VISIBLE.0 | WS_BORDER.0 | WS_TABSTOP.0 | ES_AUTOHSCROLL;
                if v.numeric {
                    es |= ES_NUMBER;
                }
                make("EDIT", "", es, cx, y, cw, 20, parent, v.id, hinst);
                y += rowh;
            }
            Row::Check(cf) => {
                let cs = WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_AUTOCHECKBOX;
                make("BUTTON", cf.label, cs, lx, y, 320, 20, parent, cf.id, hinst);
                y += rowh;
            }
        }
    }

    // Remap section — one row per entry in the model, with a remove button.
    y += 6;
    make("STATIC", "Idle remaps (physical → sent)", label_style, lx, y, 320, 20, parent, 0, hinst);
    y += 22;
    let btn = WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_PUSHBUTTON;
    for i in 0..st.remaps.len() {
        let idx = i as i32;
        let from_h = make("COMBOBOX", "", combo_style, lx, y, 120, 220, parent, ID_RM_FROM + idx * 2, hinst);
        combo_add(from_h, &st.remap_names);
        make("STATIC", "→", label_style, lx + 126, y + 3, 14, 20, parent, 0, hinst);
        let to_h = make("COMBOBOX", "", combo_style, lx + 144, y, 120, 220, parent, ID_RM_FROM + idx * 2 + 1, hinst);
        combo_add(to_h, &st.remap_names);
        make("BUTTON", "✕", btn, lx + 270, y, 26, 22, parent, ID_RM_DEL + idx, hinst);
        y += rowh;
    }
    if st.remaps.len() < MAX_REMAPS {
        make("BUTTON", "+ Add remap", btn, lx, y, 110, 24, parent, ID_RM_ADD, hinst);
        y += rowh + 4;
    }

    // Float-rule section: exe / class / title substrings (any field optional).
    y += 6;
    make("STATIC", "Float rules:  exe  /  class  /  title", label_style, lx, y, 340, 20, parent, 0, hinst);
    y += 22;
    let edit_style = WS_CHILD.0 | WS_VISIBLE.0 | WS_BORDER.0 | WS_TABSTOP.0 | ES_AUTOHSCROLL;
    for i in 0..st.raw.tiling.float.len() {
        let idx = i as i32;
        make("EDIT", "", edit_style, lx, y, 100, 20, parent, ID_FLOAT_EXE + idx * 3, hinst);
        make("EDIT", "", edit_style, lx + 104, y, 100, 20, parent, ID_FLOAT_EXE + idx * 3 + 1, hinst);
        make("EDIT", "", edit_style, lx + 208, y, 100, 20, parent, ID_FLOAT_EXE + idx * 3 + 2, hinst);
        make("BUTTON", "✕", btn, lx + 312, y, 26, 22, parent, ID_FLOAT_DEL + idx, hinst);
        y += rowh;
    }
    if st.raw.tiling.float.len() < MAX_FLOATS {
        make("BUTTON", "+ Add float rule", btn, lx, y, 130, 24, parent, ID_FLOAT_ADD, hinst);
        y += rowh + 4;
    }

    // Action buttons.
    y += 8;
    make("BUTTON", "Save", btn, lx, y, 80, 26, parent, ID_SAVE, hinst);
    make("BUTTON", "Reload", btn, lx + 88, y, 90, 26, parent, ID_RELOAD, hinst);
    make("BUTTON", "Start daemon", btn, lx + 186, y, 130, 26, parent, ID_DAEMON, hinst);
    y + 44 // content bottom, for window sizing
}

unsafe fn sync_controls_from_state(parent: HWND) {
    let st = APP.get().unwrap().lock().unwrap();
    for row in layout() {
        match row {
            Row::Section(_) => {}
            Row::Key(k) => combo_select(parent, k.id, &st.remap_names, &(k.get)(&st.raw)),
            Row::Val(v) => set_edit(parent, v.id, &(v.get)(&st.raw)),
            Row::Check(cf) => {
                if let Ok(h) = GetDlgItem(parent, cf.id) {
                    let v = if (cf.get)(&st.raw) { 1 } else { 0 };
                    SendMessageW(h, BM_SETCHECK, WPARAM(v), LPARAM(0));
                }
            }
        }
    }
    for (i, (f, t)) in st.remaps.iter().enumerate() {
        let idx = i as i32;
        combo_select(parent, ID_RM_FROM + idx * 2, &st.remap_names, f);
        combo_select(parent, ID_RM_FROM + idx * 2 + 1, &st.remap_names, t);
    }
    for (i, r) in st.raw.tiling.float.iter().enumerate() {
        let idx = i as i32;
        set_edit(parent, ID_FLOAT_EXE + idx * 3, r.exe.as_deref().unwrap_or(""));
        set_edit(parent, ID_FLOAT_EXE + idx * 3 + 1, r.class.as_deref().unwrap_or(""));
        set_edit(parent, ID_FLOAT_EXE + idx * 3 + 2, r.title.as_deref().unwrap_or(""));
    }
}

/// Read all control values back into the in-memory model (no file write).
unsafe fn read_controls(parent: HWND) {
    let mut st = APP.get().unwrap().lock().unwrap();
    for row in layout() {
        match row {
            Row::Section(_) => {}
            Row::Key(k) => {
                let v = get_text(parent, k.id);
                let v = if v == "(none)" { String::new() } else { v };
                (k.set)(&mut st.raw, v);
            }
            Row::Val(v) => {
                let s = get_text(parent, v.id);
                (v.set)(&mut st.raw, s);
            }
            Row::Check(cf) => {
                if let Ok(h) = GetDlgItem(parent, cf.id) {
                    let checked = SendMessageW(h, BM_GETCHECK, WPARAM(0), LPARAM(0)).0 == 1;
                    (cf.set)(&mut st.raw, checked);
                }
            }
        }
    }
    let len = st.remaps.len();
    for i in 0..len {
        let idx = i as i32;
        let f = get_text(parent, ID_RM_FROM + idx * 2);
        let t = get_text(parent, ID_RM_FROM + idx * 2 + 1);
        st.remaps[i] = (f, t);
    }
    let flen = st.raw.tiling.float.len();
    for i in 0..flen {
        let idx = i as i32;
        let opt = |s: String| if s.trim().is_empty() { None } else { Some(s) };
        st.raw.tiling.float[i] = FloatRule {
            exe: opt(get_text(parent, ID_FLOAT_EXE + idx * 3)),
            class: opt(get_text(parent, ID_FLOAT_EXE + idx * 3 + 1)),
            title: opt(get_text(parent, ID_FLOAT_EXE + idx * 3 + 2)),
        };
    }
}

unsafe fn do_save(parent: HWND) {
    read_controls(parent);
    let mut st = APP.get().unwrap().lock().unwrap();
    let mut map = HashMap::new();
    for (f, t) in &st.remaps {
        if f != "(none)" && t != "(none)" && !f.is_empty() && !t.is_empty() {
            map.insert(f.clone(), t.clone());
        }
    }
    st.raw.remap = map;

    let result = st.raw.save(&st.path);
    let (title, body) = match &result {
        Ok(()) => ("Saved", format!("Saved to {}\nApplies live to a running gkeyd.", st.path.display())),
        Err(e) => ("Not saved", format!("Config is invalid, nothing written:\n\n{e}")),
    };
    message_box(parent, title, &body);
}

unsafe fn do_reload(parent: HWND) {
    {
        let mut st = APP.get().unwrap().lock().unwrap();
        match RawConfig::load_raw(&st.path) {
            Ok(r) => {
                st.remaps = remaps_from_raw(&r);
                st.raw = r;
            }
            Err(e) => {
                let p = st.path.clone();
                drop(st);
                message_box(parent, "Reload failed", &format!("{p:?}: {e}"));
                return;
            }
        }
    }
    rebuild(parent); // remap count may have changed
}

unsafe extern "system" fn collect_child(h: HWND, lp: LPARAM) -> BOOL {
    let v = &mut *(lp.0 as *mut Vec<HWND>);
    v.push(h);
    TRUE
}

unsafe fn destroy_children(parent: HWND) {
    let mut v: Vec<HWND> = Vec::new();
    let _ = EnumChildWindows(parent, Some(collect_child), LPARAM(&mut v as *mut _ as isize));
    for h in v {
        let _ = DestroyWindow(h);
    }
}

unsafe fn apply_font(parent: HWND) {
    let font = GetStockObject(DEFAULT_GUI_FONT);
    let _ = EnumChildWindows(parent, Some(set_font_cb), LPARAM(font.0 as isize));
}

unsafe fn fit_window(parent: HWND, content_bottom: i32) {
    use windows::Win32::UI::WindowsAndMessaging::{SetWindowPos, SWP_NOMOVE, SWP_NOZORDER};
    let _ = SetWindowPos(parent, None, 0, 0, 380, content_bottom + 48, SWP_NOMOVE | SWP_NOZORDER);
}

/// Rebuild every control from the current model and resize to fit. Does NOT read
/// controls first, so callers that want to preserve UI edits must read_controls().
unsafe fn rebuild(parent: HWND) {
    destroy_children(parent);
    let hinst = GetModuleHandleW(None)
        .map(|h| HINSTANCE(h.0))
        .unwrap_or_default();
    let bottom = create_controls(parent, hinst);
    sync_controls_from_state(parent);
    apply_font(parent);
    update_daemon_button(parent);
    fit_window(parent, bottom);
}

unsafe fn message_box(parent: HWND, title: &str, body: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_OK};
    let _ = MessageBoxW(
        parent,
        PCWSTR(wide(body).as_ptr()),
        PCWSTR(wide(title).as_ptr()),
        MB_OK,
    );
}

unsafe extern "system" fn set_font_cb(child: HWND, lparam: LPARAM) -> BOOL {
    SendMessageW(child, WM_SETFONT, WPARAM(lparam.0 as usize), LPARAM(1));
    TRUE
}

/// Begin capturing the next physical keypress for `field_id`'s dropdown.
unsafe fn begin_capture(parent: HWND, field_id: i32) {
    if CAPTURING.load(Ordering::SeqCst) {
        return;
    }
    let hinst = match GetModuleHandleW(None) {
        Ok(h) => HINSTANCE(h.0),
        Err(_) => return,
    };
    match SetWindowsHookExW(WH_KEYBOARD_LL, Some(capture_proc), hinst, 0) {
        Ok(h) => {
            CAPTURE_HOOK.store(h.0 as isize, Ordering::SeqCst);
            CAPTURE_FIELD.store(field_id, Ordering::SeqCst);
            CAPTURING.store(true, Ordering::SeqCst);
            if let Ok(b) = GetDlgItem(parent, field_id + SET_OFFSET) {
                let _ = SetWindowTextW(b, PCWSTR(wide("press…").as_ptr()));
            }
        }
        Err(_) => {}
    }
}

unsafe fn stop_capture() {
    CAPTURING.store(false, Ordering::SeqCst);
    let hook = HHOOK(CAPTURE_HOOK.swap(0, Ordering::SeqCst) as *mut core::ffi::c_void);
    if !hook.0.is_null() {
        let _ = UnhookWindowsHookEx(hook);
    }
}

/// Temporary low-level hook active only during capture: grabs one key-down,
/// swallows it (so a running daemon doesn't act on it), and posts it to the UI.
unsafe extern "system" fn capture_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == 0 && CAPTURING.load(Ordering::SeqCst) {
        let m = wp.0 as u32;
        if m == WM_KEYDOWN || m == WM_SYSKEYDOWN {
            let kb = &*(lp.0 as *const KBDLLHOOKSTRUCT);
            let scancode = kb.scanCode as u16;
            let extended = (kb.flags.0 & LLKHF_EXTENDED) != 0;
            stop_capture();
            let main = HWND(MAIN_HWND.load(Ordering::SeqCst) as *mut core::ffi::c_void);
            let _ = PostMessageW(
                main,
                WM_APP_CAPTURED,
                WPARAM(scancode as usize),
                LPARAM(extended as isize),
            );
            return LRESULT(1);
        }
    }
    CallNextHookEx(HHOOK::default(), code, wp, lp)
}

unsafe fn on_captured(hwnd: HWND, scancode: u16, extended: bool) {
    let field = CAPTURE_FIELD.swap(-1, Ordering::SeqCst);
    if let Ok(b) = GetDlgItem(hwnd, field + SET_OFFSET) {
        let _ = SetWindowTextW(b, PCWSTR(wide("Set").as_ptr()));
    }
    if scancode == ESC_SCANCODE {
        return; // Esc cancels capture
    }
    let key = KeyCode::new(scancode, extended);
    let name = keys::name_of(key);
    let st = APP.get().unwrap().lock().unwrap();
    if st.key_names.iter().any(|n| n == &name) {
        combo_select(hwnd, field, &st.remap_names, &name);
    } else {
        drop(st);
        message_box(hwnd, "Unsupported key", &format!("{name} can't be bound; pick from the list."));
    }
}

// --- daemon control --------------------------------------------------------

/// PIDs of any running `gkeyd.exe`.
unsafe fn daemon_pids() -> Vec<u32> {
    let mut pids = Vec::new();
    let snap = match CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
        Ok(s) => s,
        Err(_) => return pids,
    };
    let mut e = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    if Process32FirstW(snap, &mut e).is_ok() {
        loop {
            let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            let name = String::from_utf16_lossy(&e.szExeFile[..len]);
            if name.eq_ignore_ascii_case("gkeyd.exe") {
                pids.push(e.th32ProcessID);
            }
            if Process32NextW(snap, &mut e).is_err() {
                break;
            }
        }
    }
    let _ = CloseHandle(snap);
    pids
}

/// Launch `gkeyd.exe` from the same directory as this GUI (falling back to PATH).
unsafe fn start_daemon(parent: HWND) {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("gkeyd.exe")))
        .unwrap_or_else(|| PathBuf::from("gkeyd.exe"));
    if let Err(e) = std::process::Command::new(&exe).spawn() {
        message_box(parent, "Start failed", &format!("Could not start {}:\n{e}", exe.display()));
    }
}

unsafe fn stop_daemon() {
    for pid in daemon_pids() {
        if let Ok(h) = OpenProcess(PROCESS_TERMINATE, FALSE, pid) {
            let _ = TerminateProcess(h, 0);
            let _ = CloseHandle(h);
        }
    }
}

unsafe fn update_daemon_button(hwnd: HWND) {
    let running = !daemon_pids().is_empty();
    if let Ok(b) = GetDlgItem(hwnd, ID_DAEMON) {
        let label = if running { "Stop daemon" } else { "Start daemon" };
        let _ = SetWindowTextW(b, PCWSTR(wide(label).as_ptr()));
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = (wp.0 & 0xFFFF) as i32;
            match id {
                ID_SAVE => do_save(hwnd),
                ID_RELOAD => do_reload(hwnd),
                ID_DAEMON => {
                    if daemon_pids().is_empty() {
                        start_daemon(hwnd);
                    } else {
                        stop_daemon();
                    }
                    update_daemon_button(hwnd);
                }
                ID_RM_ADD => {
                    read_controls(hwnd); // preserve in-progress edits
                    {
                        let mut st = APP.get().unwrap().lock().unwrap();
                        if st.remaps.len() < MAX_REMAPS {
                            st.remaps.push(("(none)".into(), "(none)".into()));
                        }
                    }
                    rebuild(hwnd);
                }
                _ if (ID_RM_DEL..ID_RM_ADD).contains(&id) => {
                    read_controls(hwnd);
                    {
                        let mut st = APP.get().unwrap().lock().unwrap();
                        let i = (id - ID_RM_DEL) as usize;
                        if i < st.remaps.len() {
                            st.remaps.remove(i);
                        }
                    }
                    rebuild(hwnd);
                }
                ID_FLOAT_ADD => {
                    read_controls(hwnd);
                    {
                        let mut st = APP.get().unwrap().lock().unwrap();
                        if st.raw.tiling.float.len() < MAX_FLOATS {
                            st.raw.tiling.float.push(FloatRule::default());
                        }
                    }
                    rebuild(hwnd);
                }
                _ if (ID_FLOAT_DEL..ID_FLOAT_ADD).contains(&id) => {
                    read_controls(hwnd);
                    {
                        let mut st = APP.get().unwrap().lock().unwrap();
                        let i = (id - ID_FLOAT_DEL) as usize;
                        if i < st.raw.tiling.float.len() {
                            st.raw.tiling.float.remove(i);
                        }
                    }
                    rebuild(hwnd);
                }
                _ if (1500..1600).contains(&id) => begin_capture(hwnd, id - SET_OFFSET),
                _ => {}
            }
            LRESULT(0)
        }
        WM_APP_CAPTURED => {
            on_captured(hwnd, wp.0 as u16, lp.0 != 0);
            LRESULT(0)
        }
        WM_TIMER => {
            update_daemon_button(hwnd);
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            stop_capture();
            let _ = KillTimer(hwnd, DAEMON_TIMER);
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn main() -> windows::core::Result<()> {
    let path = config_path();
    let raw = RawConfig::load_raw(&path).unwrap_or_default();
    let key_names: Vec<String> = keys::all_names().into_iter().map(String::from).collect();
    let mut remap_names = vec!["(none)".to_string()];
    remap_names.extend(key_names.iter().cloned());
    let remaps = remaps_from_raw(&raw);
    let _ = APP.set(Mutex::new(AppState {
        raw,
        path,
        key_names,
        remap_names,
        remaps,
    }));

    unsafe {
        let hinst = GetModuleHandleW(None)?;
        let class = wide("gkey_settings");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: PCWSTR(class.as_ptr()),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&wc);

        let style = WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_VISIBLE;
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class.as_ptr()),
            PCWSTR(wide("gkey settings").as_ptr()),
            style,
            100,
            60,
            500,
            900,
            None,
            HMENU::default(),
            hinst,
            None,
        )?;
        MAIN_HWND.store(hwnd.0 as isize, Ordering::SeqCst);

        let bottom = create_controls(hwnd, hinst.into());
        sync_controls_from_state(hwnd);
        apply_font(hwnd);
        fit_window(hwnd, bottom);

        let _ = ShowWindow(hwnd, SW_SHOW);
        update_daemon_button(hwnd);
        SetTimer(hwnd, DAEMON_TIMER, 1000, None);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}
