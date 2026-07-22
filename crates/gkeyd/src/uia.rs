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
    UIA_IsOffscreenPropertyId, UIA_IsSelectionItemPatternAvailablePropertyId,
    UIA_IsTogglePatternAvailablePropertyId, UIA_ListItemControlTypeId, UIA_MenuItemControlTypeId,
    UIA_RadioButtonControlTypeId, UIA_SplitButtonControlTypeId, UIA_TabItemControlTypeId,
    UIA_TreeItemControlTypeId,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumThreadWindows, GetForegroundWindow, GetWindowRect, GetWindowThreadProcessId,
    IsWindowVisible,
};

/// Click points (physical screen pixels) of hintable elements.
pub type ScanReply = Vec<(i32, i32)>;

/// Merge hints whose centres are within this many pixels of each other.
const DEDUP_PX: i32 = 12;

fn build_condition(a: &IUIAutomation) -> Result<IUIAutomationCondition> {
    unsafe {
        // Cheap, cacheable availability properties instead of probing patterns,
        // plus the control types that are clickable but often expose no pattern
        // (toolbar icons, menu items, list/tab/tree items, links).
        let clickable: [(_, VARIANT); 16] = [
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

/// Foreground window plus visible popups/menus on the same thread (dropdowns,
/// context menus and flyouts live in their own HWNDs).
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
    }
    wins
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

fn scan(a: &IUIAutomation) -> Result<ScanReply> {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return Ok(Vec::new());
        }
        let cond = build_condition(a)?;
        let cache = a.CreateCacheRequest()?;
        cache.AddProperty(UIA_BoundingRectanglePropertyId)?;
        cache.SetTreeScope(TreeScope(TreeScope_Element.0 | TreeScope_Descendants.0))?;

        let mut out = ScanReply::new();
        for hwnd in candidate_windows(fg) {
            // A failing popup shouldn't abort the whole scan.
            let _ = scan_window(a, &cond, &cache, hwnd, &mut out);
        }
        Ok(dedup(out))
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
            tracing::info!("UIA scanner ready");
            while let Ok(reply) = rx.recv() {
                let mut result = scan(&automation).unwrap_or_else(|e| {
                    tracing::warn!("UIA scan failed: {e}");
                    Vec::new()
                });
                // Chromium/Electron build their accessibility tree only once a
                // client asks for it — the first scan of a cold window comes back
                // nearly empty. Give it a beat and try once more.
                if result.len() <= 1 {
                    std::thread::sleep(std::time::Duration::from_millis(180));
                    if let Ok(second) = scan(&automation) {
                        if second.len() > result.len() {
                            result = second;
                        }
                    }
                }
                tracing::info!("UIA scan: {} target(s)", result.len());
                let _ = reply.send(result);
            }
        })
        .expect("spawn uia thread");
    tx
}
