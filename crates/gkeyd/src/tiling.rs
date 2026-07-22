//! On-demand tiling of the monitor under the cursor (M3 v1).
//!
//! This is not (yet) a live tiling window manager: it arranges the current
//! monitor's manageable top-level windows on demand and moves keyboard focus
//! between them. Continuous WinEvent-driven tracking, workspaces, and per-app
//! rules are future work — see docs/RESEARCH.md.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicI32, AtomicU32, AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};

use gkey_core::config::FloatRule;
use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, BOOL, FALSE, HWND, LPARAM, POINT, RECT, TRUE};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, HMONITOR, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, EnumWindows, GetClassNameW, GetCursorPos, GetForegroundWindow,
    GetWindowLongW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
    IsIconic, IsWindow, IsWindowVisible, IsZoomed, SetForegroundWindow, SetWindowPos, ShowWindow,
    ASFW_ANY, GWL_EXSTYLE, GWL_STYLE, SET_WINDOW_POS_FLAGS, SWP_NOACTIVATE, SWP_NOSENDCHANGING,
    SWP_NOZORDER, SW_HIDE, SW_RESTORE, SW_SHOWNA, WS_CAPTION, WS_EX_TOOLWINDOW,
};

use crossbeam_channel::Sender;

use crate::input;
use crate::overlay::UiCmd;

/// Windows matching any of these rules float (excluded from tiling).
static FLOAT_RULES: Mutex<Vec<FloatRule>> = Mutex::new(Vec::new());

pub fn set_float_rules(rules: Vec<FloatRule>) {
    *FLOAT_RULES.lock().unwrap() = rules;
}

/// Channel to the overlay for the workspace indicator toast.
static INDICATOR_TX: OnceLock<Sender<UiCmd>> = OnceLock::new();

pub fn set_indicator_sink(tx: Sender<UiCmd>) {
    let _ = INDICATOR_TX.set(tx);
}

fn show_indicator(mon: HMONITOR, workspace: usize) {
    if let (Some(tx), Some(area)) = (INDICATOR_TX.get(), work_area(mon)) {
        let cx = (area.left + area.right) / 2;
        let cy = (area.top + area.bottom) / 2;
        let _ = tx.send(UiCmd::Indicator {
            text: format!("Workspace {}", workspace + 1),
            cx,
            cy,
        });
    }
}

/// Gap between adjacent windows and margin from the screen edge (pixels).
static INNER_GAP: AtomicI32 = AtomicI32::new(8);
static OUTER_GAP: AtomicI32 = AtomicI32::new(8);

pub fn set_gaps(inner: i32, outer: i32) {
    INNER_GAP.store(inner.max(0), Ordering::Relaxed);
    OUTER_GAP.store(outer.max(0), Ordering::Relaxed);
}

#[derive(Clone, Copy)]
pub enum Layout {
    Bsp,
    Columns,
}

/// Last layout the user chose, so window-event auto-tiling reuses it.
static CURRENT_LAYOUT: AtomicU8 = AtomicU8::new(0);

pub fn set_layout(layout: Layout) {
    CURRENT_LAYOUT.store(layout as u8, Ordering::Relaxed);
}

pub fn current_layout() -> Layout {
    match CURRENT_LAYOUT.load(Ordering::Relaxed) {
        1 => Layout::Columns,
        _ => Layout::Bsp,
    }
}

/// Fraction of the area given to the master (first) window. Stored as f32 bits;
/// 0 means uninitialised → the 0.5 default.
static MASTER_RATIO: AtomicU32 = AtomicU32::new(0);

fn master_ratio() -> f32 {
    let bits = MASTER_RATIO.load(Ordering::Relaxed);
    if bits == 0 {
        0.5
    } else {
        f32::from_bits(bits)
    }
}

fn set_master_ratio(r: f32) {
    MASTER_RATIO.store(r.clamp(0.15, 0.85).to_bits(), Ordering::Relaxed);
}

fn work_area(mon: HMONITOR) -> Option<RECT> {
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if unsafe { GetMonitorInfoW(mon, &mut mi) }.as_bool() {
        Some(mi.rcWork)
    } else {
        None
    }
}

/// Work area (left, top, width, height) of the monitor under the cursor —
/// used by hint grid mode so the grid covers the right screen.
pub fn cursor_work_area() -> Option<(i32, i32, i32, i32)> {
    let r = work_area(cursor_monitor())?;
    Some((r.left, r.top, r.right - r.left, r.bottom - r.top))
}

fn cursor_monitor() -> HMONITOR {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
        MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST)
    }
}

fn window_rect(hwnd: HWND) -> RECT {
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut r);
    }
    r
}

fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked: u32 = 0;
    unsafe {
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut c_void,
            std::mem::size_of::<u32>() as u32,
        );
    }
    cloaked != 0
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n as usize])
}

fn window_title(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n as usize])
}

/// Executable base name (e.g. "notepad.exe") of the window's process.
fn window_exe(hwnd: HWND) -> String {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return String::new();
        }
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid) else {
            return String::new();
        };
        let mut buf = [0u16; 260];
        let mut len = buf.len() as u32;
        let ok =
            QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len)
                .is_ok();
        let _ = CloseHandle(h);
        if !ok {
            return String::new();
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit(['\\', '/']).next().unwrap_or(&path).to_string()
    }
}

/// Does this window match a configured float rule?
fn is_floating(hwnd: HWND) -> bool {
    let rules = FLOAT_RULES.lock().unwrap();
    if rules.is_empty() {
        return false;
    }
    let class = class_name(hwnd);
    let title = window_title(hwnd);
    let need_exe = rules.iter().any(|r| r.exe.is_some());
    let exe = if need_exe {
        window_exe(hwnd)
    } else {
        String::new()
    };
    rules.iter().any(|r| r.matches(&exe, &class, &title))
}

/// Is this a normal, movable application window? If `target` is set, also
/// require it to be on that monitor.
fn is_manageable(hwnd: HWND, target: Option<HMONITOR>) -> bool {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return false;
        }
        if GetWindowTextLengthW(hwnd) == 0 {
            return false;
        }
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        let exstyle = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if style & WS_CAPTION.0 == 0 {
            return false; // no title bar → not a normal window
        }
        if exstyle & WS_EX_TOOLWINDOW.0 != 0 {
            return false; // tool windows (palettes etc.)
        }
        if is_cloaked(hwnd) {
            return false; // e.g. UWP windows on another virtual desktop
        }
        // Shell surfaces.
        let class = class_name(hwnd);
        if matches!(
            class.as_str(),
            "Progman" | "WorkerW" | "Shell_TrayWnd" | "Windows.UI.Core.CoreWindow"
        ) {
            return false;
        }
        if is_floating(hwnd) {
            return false; // user-configured float rule
        }
        match target {
            Some(m) => MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) == m,
            None => true,
        }
    }
}

unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let acc = &mut *(lparam.0 as *mut (Option<HMONITOR>, Vec<HWND>));
    if is_manageable(hwnd, acc.0) {
        acc.1.push(hwnd);
    }
    TRUE
}

/// Manageable windows (all monitors if `mon` is None), ordered left-to-right
/// then top-to-bottom.
fn manageable_windows(mon: Option<HMONITOR>) -> Vec<HWND> {
    let mut acc: (Option<HMONITOR>, Vec<HWND>) = (mon, Vec::new());
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut acc as *mut _ as isize));
    }
    let mut wins = acc.1;
    wins.sort_by_key(|&h| {
        let r = window_rect(h);
        (r.left, r.top)
    });
    wins
}

fn split_v_ratio(a: RECT, ratio: f32) -> (RECT, RECT) {
    let mid = a.left + ((a.right - a.left) as f32 * ratio) as i32;
    (RECT { right: mid, ..a }, RECT { left: mid, ..a })
}

fn split_h(a: RECT) -> (RECT, RECT) {
    let mid = (a.top + a.bottom) / 2;
    (RECT { bottom: mid, ..a }, RECT { top: mid, ..a })
}

fn compute(layout: Layout, area: RECT, n: usize) -> Vec<RECT> {
    let mut rects = Vec::with_capacity(n);
    if n == 0 {
        return rects;
    }
    // Outer gap: margin between the tiling and the screen edge.
    let outer = OUTER_GAP.load(Ordering::Relaxed);
    let area = RECT {
        left: area.left + outer,
        top: area.top + outer,
        right: area.right - outer,
        bottom: area.bottom - outer,
    };
    let ratio = master_ratio();
    match layout {
        Layout::Columns => {
            if n == 1 {
                rects.push(area);
            } else {
                // Master column takes `ratio`; the rest split the remainder.
                let w = area.right - area.left;
                let first = (w as f32 * ratio) as i32;
                rects.push(RECT {
                    right: area.left + first,
                    ..area
                });
                let rest_w = w - first;
                for i in 0..(n - 1) {
                    let l = area.left + first + (rest_w * i as i32) / (n as i32 - 1);
                    let r = area.left + first + (rest_w * (i as i32 + 1)) / (n as i32 - 1);
                    rects.push(RECT {
                        left: l,
                        top: area.top,
                        right: r,
                        bottom: area.bottom,
                    });
                }
            }
        }
        Layout::Bsp => {
            let mut cur = area;
            for i in 0..n {
                if i == n - 1 {
                    rects.push(cur);
                    break;
                }
                // First split uses the master ratio; deeper splits are even.
                let (a, b) = if i % 2 == 0 {
                    split_v_ratio(cur, if i == 0 { ratio } else { 0.5 })
                } else {
                    split_h(cur)
                };
                rects.push(a);
                cur = b;
            }
        }
    }
    // Inner gap: half on each side so adjacent windows are `inner` apart.
    let inner = INNER_GAP.load(Ordering::Relaxed);
    for r in &mut rects {
        r.left += inner / 2;
        r.top += inner / 2;
        r.right -= inner / 2;
        r.bottom -= inner / 2;
    }
    rects
}

/// Move/size a window so its *visible* frame matches `target`, compensating for
/// the invisible DWM drop-shadow border (the delta between the extended frame
/// bounds and the outer window rect).
fn position(hwnd: HWND, target: RECT) {
    unsafe {
        if IsZoomed(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let mut frame = RECT::default();
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut frame as *mut RECT as *mut c_void,
            std::mem::size_of::<RECT>() as u32,
        );
        let outer = window_rect(hwnd);
        let (dl, dt, dr, db) = (
            frame.left - outer.left,
            frame.top - outer.top,
            outer.right - frame.right,
            outer.bottom - frame.bottom,
        );
        let x = target.left - dl;
        let y = target.top - dt;
        let w = (target.right - target.left) + dl + dr;
        let h = (target.bottom - target.top) + dt + db;
        let flags: SET_WINDOW_POS_FLAGS = SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOSENDCHANGING;
        let _ = SetWindowPos(hwnd, None, x, y, w, h, flags);
    }
}

fn tile_windows(area: RECT, wins: &[HWND], layout: Layout) {
    let rects = compute(layout, area, wins.len());
    for (hwnd, rect) in wins.iter().zip(rects) {
        position(*hwnd, rect);
    }
}

/// Tile one monitor's visible manageable windows with the current layout.
fn tile_one(mon: HMONITOR) {
    if let Some(area) = work_area(mon) {
        let wins = manageable_windows(Some(mon));
        if !wins.is_empty() {
            tile_windows(area, &wins, current_layout());
        }
    }
}

fn monitor_key(mon: HMONITOR) -> isize {
    mon.0 as isize
}

fn monitor_from_key(key: isize) -> HMONITOR {
    HMONITOR(key as *mut c_void)
}

/// Tile the monitor under the cursor with the given layout (on-demand).
pub fn tile(layout: Layout) {
    set_layout(layout);
    tile_one(cursor_monitor());
}

/// Tile every monitor that has manageable windows (used by live auto-tiling).
pub fn tile_all(layout: Layout) {
    set_layout(layout);
    let all = manageable_windows(None);
    if all.is_empty() {
        return;
    }
    let mut groups: HashMap<isize, Vec<HWND>> = HashMap::new();
    for h in all {
        let m = unsafe { MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST) };
        groups.entry(m.0 as isize).or_default().push(h);
    }
    for (m, wins) in groups {
        let mon = HMONITOR(m as *mut c_void);
        if let Some(area) = work_area(mon) {
            tile_windows(area, &wins, layout);
        }
    }
}

fn focus_window(hwnd: HWND) {
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let _ = AllowSetForegroundWindow(ASFW_ANY);
        // Synthetic input resets the foreground-change lock so SetForegroundWindow
        // succeeds from our background process. Our heartbeat is swallowed by the
        // hook, so it is otherwise invisible.
        input::heartbeat();
        let _ = SetForegroundWindow(hwnd);
    }
}

/// Move focus to the next (`+1`) or previous (`-1`) window on the current monitor.
pub fn focus(dir: i32) {
    let mon = cursor_monitor();
    let wins = manageable_windows(Some(mon));
    if wins.is_empty() {
        return;
    }
    let fg = unsafe { GetForegroundWindow() };
    let idx = wins.iter().position(|&h| h == fg).unwrap_or(0) as i32;
    let next = (idx + dir).rem_euclid(wins.len() as i32) as usize;
    focus_window(wins[next]);
}

/// Grow (`+delta`) or shrink (`-delta`) the master area, then re-tile.
pub fn resize(delta: f32) {
    set_master_ratio(master_ratio() + delta);
    let mon = cursor_monitor();
    if let Some(area) = work_area(mon) {
        let wins = manageable_windows(Some(mon));
        if !wins.is_empty() {
            tile_windows(area, &wins, current_layout());
        }
    }
}

/// Swap the focused window with its neighbour (`+1`/`-1`) in tiling order.
pub fn swap(dir: i32) {
    let mon = cursor_monitor();
    let wins = manageable_windows(Some(mon));
    if wins.len() < 2 {
        return;
    }
    let fg = unsafe { GetForegroundWindow() };
    let Some(idx) = wins.iter().position(|&h| h == fg) else {
        return;
    };
    let target = ((idx as i32 + dir).rem_euclid(wins.len() as i32)) as usize;
    if target == idx {
        return;
    }
    let Some(area) = work_area(mon) else {
        return;
    };
    let rects = compute(current_layout(), area, wins.len());
    position(wins[idx], rects[target]);
    position(wins[target], rects[idx]);
}

/// Make the focused window the master (first slot), swapping it with the
/// current master.
pub fn promote() {
    let mon = cursor_monitor();
    let wins = manageable_windows(Some(mon));
    if wins.len() < 2 {
        return;
    }
    let fg = unsafe { GetForegroundWindow() };
    let Some(idx) = wins.iter().position(|&h| h == fg) else {
        return;
    };
    if idx == 0 {
        return; // already master
    }
    let Some(area) = work_area(mon) else {
        return;
    };
    let rects = compute(current_layout(), area, wins.len());
    position(wins[idx], rects[0]);
    position(wins[0], rects[idx]);
}

// --- workspaces (per monitor) ----------------------------------------------
//
// Each monitor has its own active workspace. Windows not on their monitor's
// active workspace are hidden (SW_HIDE). A window remembers its (monitor,
// workspace) in `assign`; new windows join their monitor's active workspace on
// first sighting. Switching affects only the monitor under the cursor.

const WORKSPACES: usize = 9;

struct Ws {
    active: HashMap<isize, usize>, // monitor key -> active workspace
    assign: HashMap<isize, (isize, usize)>, // hwnd -> (monitor key, workspace)
}

impl Ws {
    fn active_ws(&self, mon: isize) -> usize {
        *self.active.get(&mon).unwrap_or(&0)
    }
}

fn ws() -> &'static Mutex<Ws> {
    static W: OnceLock<Mutex<Ws>> = OnceLock::new();
    W.get_or_init(|| {
        Mutex::new(Ws {
            active: HashMap::new(),
            assign: HashMap::new(),
        })
    })
}

fn set_visible(hwnd: HWND, visible: bool) {
    unsafe {
        let _ = ShowWindow(hwnd, if visible { SW_SHOWNA } else { SW_HIDE });
    }
}

fn prune_dead() {
    ws().lock()
        .unwrap()
        .assign
        .retain(|&hi, _| unsafe { IsWindow(HWND(hi as *mut c_void)).as_bool() });
}

/// Assign any currently-visible windows on `mon` that we haven't seen yet to
/// that monitor's active workspace. Caller holds the lock.
fn assign_new_on(w: &mut Ws, mon_key: isize) {
    let cur = w.active_ws(mon_key);
    for h in manageable_windows(Some(monitor_from_key(mon_key))) {
        w.assign.entry(h.0 as isize).or_insert((mon_key, cur));
    }
}

/// Write the set of currently-hidden window handles for the watcher process.
fn write_hidden_state() {
    let lines = {
        let w = ws().lock().unwrap();
        let mut s = String::new();
        for (&hi, &(mon, wsid)) in w.assign.iter() {
            if wsid != w.active_ws(mon) {
                s.push_str(&hi.to_string());
                s.push('\n');
            }
        }
        s
    };
    let _ = std::fs::write(gkey_core::hidden_state_path(), lines);
}

fn switch_monitor(mon_key: isize, target: usize) {
    let entries: Vec<(isize, usize)> = {
        let mut w = ws().lock().unwrap();
        if target == w.active_ws(mon_key) {
            return;
        }
        assign_new_on(&mut w, mon_key);
        w.active.insert(mon_key, target);
        w.assign
            .iter()
            .filter(|(_, &(m, _))| m == mon_key)
            .map(|(&h, &(_, wsid))| (h, wsid))
            .collect()
    };
    for (hi, wsid) in entries {
        let h = HWND(hi as *mut c_void);
        if unsafe { IsWindow(h) }.as_bool() {
            set_visible(h, wsid == target);
        }
    }
    prune_dead();
    tile_one(monitor_from_key(mon_key));
    write_hidden_state();
    show_indicator(monitor_from_key(mon_key), target);
    tracing::info!("workspace {} (monitor)", target + 1);
}

fn move_focused(mon_key: isize, fg: HWND, target: usize) {
    let hide = {
        let mut w = ws().lock().unwrap();
        let cur = w.active_ws(mon_key);
        assign_new_on(&mut w, mon_key);
        w.assign.insert(fg.0 as isize, (mon_key, target));
        target != cur
    };
    if hide {
        set_visible(fg, false);
    }
    tile_one(monitor_from_key(mon_key));
    write_hidden_state();
    tracing::info!("moved window to workspace {}", target + 1);
}

/// Switch the cursor monitor directly to workspace `n` (0-indexed).
pub fn switch_to(n: usize) {
    if n < WORKSPACES {
        switch_monitor(monitor_key(cursor_monitor()), n);
    }
}

/// Move the focused window directly to workspace `n` on its monitor.
pub fn move_window_to(n: usize) {
    if n >= WORKSPACES {
        return;
    }
    let fg = unsafe { GetForegroundWindow() };
    if fg.0.is_null() || !is_manageable(fg, None) {
        return;
    }
    let key = monitor_key(unsafe { MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST) });
    move_focused(key, fg, n);
}

/// Switch the cursor monitor to the next (`+1`) / previous (`-1`) workspace.
pub fn workspace_cycle(dir: i32) {
    let key = monitor_key(cursor_monitor());
    let cur = ws().lock().unwrap().active_ws(key) as i32;
    let n = (cur + dir).rem_euclid(WORKSPACES as i32) as usize;
    switch_monitor(key, n);
}

/// Move the focused window to the next / previous workspace on its monitor.
pub fn move_cycle(dir: i32) {
    let fg = unsafe { GetForegroundWindow() };
    if fg.0.is_null() || !is_manageable(fg, None) {
        return;
    }
    let key = monitor_key(unsafe { MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST) });
    let cur = ws().lock().unwrap().active_ws(key) as i32;
    let n = (cur + dir).rem_euclid(WORKSPACES as i32) as usize;
    move_focused(key, fg, n);
}

/// Show every tracked window again (called on shutdown so nothing is left
/// hidden if the daemon exits).
pub fn restore_all() {
    {
        let w = ws().lock().unwrap();
        for &hi in w.assign.keys() {
            let h = HWND(hi as *mut c_void);
            if unsafe { IsWindow(h) }.as_bool() {
                unsafe {
                    let _ = ShowWindow(h, SW_SHOWNA);
                }
            }
        }
    }
    // Clear the state file so the watcher restores nothing on a clean exit.
    let _ = std::fs::write(gkey_core::hidden_state_path(), "");
}
