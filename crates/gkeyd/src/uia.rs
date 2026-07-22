//! UI Automation element scanner.
//!
//! Runs on a dedicated COM MTA thread that owns no windows (UIA calls from a
//! UI-owning thread can deadlock, and must never run on the hook thread). The
//! engine sends a reply channel; we scan the foreground window — plus any
//! visible popup/menu windows on its thread — in cached round-trips and send
//! back de-duplicated click points.

use anyhow::Result;
use crossbeam_channel::{unbounded, Sender};
use windows::core::VARIANT;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT, TRUE};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationCacheRequest, IUIAutomationCondition, TreeScope,
    TreeScope_Descendants, TreeScope_Element, UIA_BoundingRectanglePropertyId,
    UIA_ButtonControlTypeId, UIA_CheckBoxControlTypeId, UIA_ComboBoxControlTypeId,
    UIA_ControlTypePropertyId, UIA_EditControlTypeId, UIA_HyperlinkControlTypeId,
    UIA_IsEnabledPropertyId, UIA_IsExpandCollapsePatternAvailablePropertyId,
    UIA_IsInvokePatternAvailablePropertyId, UIA_IsKeyboardFocusablePropertyId,
    UIA_HeaderItemControlTypeId, UIA_IsOffscreenPropertyId,
    UIA_IsSelectionItemPatternAvailablePropertyId, UIA_IsTogglePatternAvailablePropertyId,
    UIA_ListItemControlTypeId, UIA_MenuItemControlTypeId, UIA_RadioButtonControlTypeId,
    UIA_SliderControlTypeId, UIA_SpinnerControlTypeId, UIA_SplitButtonControlTypeId,
    UIA_TabItemControlTypeId, UIA_TreeItemControlTypeId,
};
use windows::core::w;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumThreadWindows, EnumWindows, FindWindowExW, FindWindowW, GetForegroundWindow, GetWindow,
    GetWindowRect, GetWindowThreadProcessId, IsWindowVisible, GW_OWNER,
};

/// Click points (physical screen pixels) of hintable elements.
pub type ScanReply = Vec<(i32, i32)>;

/// Merge hints whose centres are within this many pixels of each other.
const DEDUP_PX: i32 = 12;

/// Below this many results the tree is likely still building (Chromium/Electron
/// build accessibility lazily) — wait and scan again, merging both passes.
const RETRY_THRESHOLD: usize = 6;

fn build_condition(a: &IUIAutomation) -> Result<IUIAutomationCondition> {
    unsafe {
        // Cheap, cacheable availability properties instead of probing patterns,
        // plus the control types that are clickable but often expose no pattern
        // (toolbar icons, menu items, list/tab/tree items, links).
        let clickable: [(_, VARIANT); 19] = [
            (UIA_IsKeyboardFocusablePropertyId, VARIANT::from(true)),
            (UIA_IsInvokePatternAvailablePropertyId, VARIANT::from(true)),
            (UIA_IsTogglePatternAvailablePropertyId, VARIANT::from(true)),
            (
                UIA_IsExpandCollapsePatternAvailablePropertyId,
                VARIANT::from(true),
            ),
            (
                UIA_IsSelectionItemPatternAvailablePropertyId,
                VARIANT::from(true),
            ),
            (UIA_ControlTypePropertyId, VARIANT::from(UIA_ButtonControlTypeId.0)),
            (UIA_ControlTypePropertyId, VARIANT::from(UIA_SplitButtonControlTypeId.0)),
            (UIA_ControlTypePropertyId, VARIANT::from(UIA_HyperlinkControlTypeId.0)),
            (UIA_ControlTypePropertyId, VARIANT::from(UIA_MenuItemControlTypeId.0)),
            (UIA_ControlTypePropertyId, VARIANT::from(UIA_ListItemControlTypeId.0)),
            (UIA_ControlTypePropertyId, VARIANT::from(UIA_TabItemControlTypeId.0)),
            (UIA_ControlTypePropertyId, VARIANT::from(UIA_TreeItemControlTypeId.0)),
            (UIA_ControlTypePropertyId, VARIANT::from(UIA_CheckBoxControlTypeId.0)),
            (UIA_ControlTypePropertyId, VARIANT::from(UIA_RadioButtonControlTypeId.0)),
            (UIA_ControlTypePropertyId, VARIANT::from(UIA_ComboBoxControlTypeId.0)),
            (UIA_ControlTypePropertyId, VARIANT::from(UIA_EditControlTypeId.0)),
            (UIA_ControlTypePropertyId, VARIANT::from(UIA_SliderControlTypeId.0)),
            (UIA_ControlTypePropertyId, VARIANT::from(UIA_SpinnerControlTypeId.0)),
            (UIA_ControlTypePropertyId, VARIANT::from(UIA_HeaderItemControlTypeId.0)),
        ];

        let mut or_cond: Option<IUIAutomationCondition> = None;
        for (prop, val) in clickable {
            let c = a.CreatePropertyCondition(prop, &val)?;
            or_cond = Some(match or_cond {
                None => c,
                Some(prev) => a.CreateOrCondition(Some(&prev), Some(&c))?,
            });
        }
        let or_cond = or_cond.expect("non-empty clickable set");

        let on_screen =
            a.CreatePropertyCondition(UIA_IsOffscreenPropertyId, &VARIANT::from(false))?;
        let enabled = a.CreatePropertyCondition(UIA_IsEnabledPropertyId, &VARIANT::from(true))?;
        let gate = a.CreateAndCondition(Some(&on_screen), Some(&enabled))?;
        Ok(a.CreateAndCondition(Some(&gate), Some(&or_cond))?)
    }
}

unsafe extern "system" fn collect_thread_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let acc = &mut *(lparam.0 as *mut Vec<HWND>);
    if IsWindowVisible(hwnd).as_bool() {
        let mut r = RECT::default();
        if GetWindowRect(hwnd, &mut r).is_ok() && r.right > r.left && r.bottom > r.top {
            acc.push(hwnd);
        }
    }
    TRUE
}

struct OwnedCtx {
    owner: HWND,
    out: Vec<HWND>,
}

unsafe extern "system" fn collect_owned_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = &mut *(lparam.0 as *mut OwnedCtx);
    if IsWindowVisible(hwnd).as_bool()
        && GetWindow(hwnd, GW_OWNER).map(|h| h == ctx.owner).unwrap_or(false)
    {
        ctx.out.push(hwnd);
    }
    TRUE
}

/// Foreground window plus visible popups/menus on the same thread (dropdowns,
/// context menus and flyouts live in their own HWNDs) plus windows owned by
/// the foreground window (dialogs and palettes can run on other threads).
fn candidate_windows(fg: HWND) -> Vec<HWND> {
    let mut wins = vec![fg];
    unsafe {
        let tid = GetWindowThreadProcessId(fg, None);
        if tid != 0 {
            let mut extra: Vec<HWND> = Vec::new();
            let _ = EnumThreadWindows(
                tid,
                Some(collect_thread_window),
                LPARAM(&mut extra as *mut _ as isize),
            );
            for h in extra {
                if h != fg && !wins.contains(&h) {
                    wins.push(h);
                }
            }
        }
        let mut owned = OwnedCtx {
            owner: fg,
            out: Vec::new(),
        };
        let _ = EnumWindows(
            Some(collect_owned_window),
            LPARAM(&mut owned as *mut _ as isize),
        );
        for h in owned.out {
            if !wins.contains(&h) {
                wins.push(h);
            }
        }
    }
    wins
}

/// The taskbar lives in its own process and is never the foreground window, so
/// it must be scanned explicitly: primary bar, per-monitor secondary bars, and
/// the tray-overflow flyout.
fn taskbar_windows() -> Vec<HWND> {
    let mut out = Vec::new();
    unsafe {
        let mut push = |h: HWND| {
            if !h.0.is_null() && IsWindowVisible(h).as_bool() && !out.contains(&h) {
                out.push(h);
            }
        };
        if let Ok(h) = FindWindowW(w!("Shell_TrayWnd"), None) {
            push(h);
        }
        let mut prev = HWND::default();
        while let Ok(h) = FindWindowExW(None, prev, w!("Shell_SecondaryTrayWnd"), None) {
            if h.0.is_null() {
                break;
            }
            push(h);
            prev = h;
        }
        if let Ok(h) = FindWindowW(w!("TopLevelWindowForOverflowXamlIsland"), None) {
            push(h);
        }
    }
    out
}

fn scan_window(
    a: &IUIAutomation,
    cond: &IUIAutomationCondition,
    cache: &IUIAutomationCacheRequest,
    hwnd: HWND,
    out: &mut ScanReply,
) -> Result<()> {
    unsafe {
        let root = a.ElementFromHandle(hwnd)?;
        let found = root.FindAllBuildCache(TreeScope_Descendants, Some(cond), Some(cache))?;
        let len = found.Length()?;
        for i in 0..len {
            let Ok(el) = found.GetElement(i) else { continue };
            let Ok(r) = el.CachedBoundingRectangle() else {
                continue;
            };
            let (w, h) = (r.right - r.left, r.bottom - r.top);
            if w <= 0 || h <= 0 {
                continue; // zero-size / not displayed
            }
            out.push((r.left + w / 2, r.top + h / 2));
        }
    }
    Ok(())
}

fn dedup(mut points: ScanReply) -> ScanReply {
    let mut kept: ScanReply = Vec::with_capacity(points.len());
    points.sort_by_key(|&(x, y)| (y, x));
    for p in points {
        if !kept
            .iter()
            .any(|&(x, y)| (x - p.0).abs() <= DEDUP_PX && (y - p.1).abs() <= DEDUP_PX)
        {
            kept.push(p);
        }
    }
    kept
}

/// Scan the foreground app and the taskbar. Returns the number of points that
/// came from the app itself (the retry heuristic must ignore the taskbar's
/// always-present buttons) plus the merged, de-duplicated point list.
fn scan(
    a: &IUIAutomation,
    cond: &IUIAutomationCondition,
    cache: &IUIAutomationCacheRequest,
) -> Result<(usize, ScanReply)> {
    unsafe {
        let fg = GetForegroundWindow();
        let app_wins = if fg.0.is_null() {
            Vec::new()
        } else {
            candidate_windows(fg)
        };
        let mut out = ScanReply::new();
        for hwnd in &app_wins {
            // A failing popup shouldn't abort the whole scan.
            let _ = scan_window(a, cond, cache, *hwnd, &mut out);
        }
        let app_count = out.len();
        for hwnd in taskbar_windows() {
            if !app_wins.contains(&hwnd) {
                let _ = scan_window(a, cond, cache, hwnd, &mut out);
            }
        }
        Ok((app_count, dedup(out)))
    }
}

/// Start the UIA thread; send it a reply channel to request a foreground scan.
pub fn spawn() -> Sender<Sender<ScanReply>> {
    let (tx, rx) = unbounded::<Sender<ScanReply>>();
    std::thread::Builder::new()
        .name("uia".into())
        .spawn(move || {
            unsafe {
                // MTA: UIA clients should not run on an STA/UI thread.
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            let automation: IUIAutomation =
                match unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_ALL) } {
                    Ok(a) => a,
                    Err(e) => {
                        tracing::error!("failed to create UIAutomation: {e}");
                        return;
                    }
                };
            let (cond, cache) = match (|| -> Result<_> {
                let cond = build_condition(&automation)?;
                let cache = unsafe {
                    let c = automation.CreateCacheRequest()?;
                    c.AddProperty(UIA_BoundingRectanglePropertyId)?;
                    c.SetTreeScope(TreeScope(TreeScope_Element.0 | TreeScope_Descendants.0))?;
                    c
                };
                Ok((cond, cache))
            })() {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!("failed to build UIA condition: {e}");
                    return;
                }
            };
            tracing::info!("UIA scanner ready");
            while let Ok(mut reply) = rx.recv() {
                // Serve only the newest request: older senders still queued
                // mean the engine already gave up on them.
                while let Ok(newer) = rx.try_recv() {
                    reply = newer;
                }
                let started = std::time::Instant::now();
                let (mut app_count, mut result) =
                    scan(&automation, &cond, &cache).unwrap_or_else(|e| {
                        tracing::warn!("UIA scan failed: {e}");
                        (0, Vec::new())
                    });
                // Chromium/Electron/XAML build their accessibility tree only
                // once a client asks for it — the first scans of a cold window
                // come back sparse (often just the window chrome). Rescan and
                // merge, but stop as soon as the result stops growing so warm
                // windows stay fast.
                for pause_ms in [150u64, 350] {
                    if app_count >= RETRY_THRESHOLD {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(pause_ms));
                    let Ok((second_count, second)) = scan(&automation, &cond, &cache) else {
                        break;
                    };
                    app_count = app_count.max(second_count);
                    let before = result.len();
                    result.extend(second);
                    result = dedup(result);
                    if result.len() <= before && !result.is_empty() {
                        break; // stable — window is genuinely sparse
                    }
                }
                tracing::info!(
                    "UIA scan: {} target(s) in {}ms",
                    result.len(),
                    started.elapsed().as_millis()
                );
                let _ = reply.send(result);
            }
        })
        .expect("spawn uia thread");
    tx
}
