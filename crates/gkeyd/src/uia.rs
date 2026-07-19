//! UI Automation element scanner.
//!
//! Runs on a dedicated COM MTA thread that owns no windows (UIA calls from a
//! UI-owning thread can deadlock, and must never run on the hook thread). The
//! engine sends a reply channel; we scan the foreground window's clickable
//! elements in a single cached round-trip and send back their click points.
//!
//! NOTE: not yet runtime-verified on-device (Smart App Control blocks running
//! the unsigned build here) — the API sequence follows the Microsoft UIA
//! caching guidance and mousemaster's condition, but expect to tune the
//! condition and add IUIAutomation2 connection timeouts once it can be run.

use anyhow::Result;
use crossbeam_channel::{unbounded, Sender};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows::core::VARIANT;
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationCondition, TreeScope, TreeScope_Descendants,
    TreeScope_Element,
    UIA_BoundingRectanglePropertyId, UIA_ButtonControlTypeId, UIA_ControlTypePropertyId,
    UIA_IsEnabledPropertyId, UIA_IsExpandCollapsePatternAvailablePropertyId,
    UIA_IsInvokePatternAvailablePropertyId, UIA_IsKeyboardFocusablePropertyId,
    UIA_IsOffscreenPropertyId, UIA_IsSelectionItemPatternAvailablePropertyId,
    UIA_IsTogglePatternAvailablePropertyId,
};
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

/// Click points (physical screen pixels) of hintable elements.
pub type ScanReply = Vec<(i32, i32)>;

fn build_condition(a: &IUIAutomation) -> Result<IUIAutomationCondition> {
    unsafe {
        // Cheap, cacheable availability properties instead of probing patterns.
        let clickable: [(_, VARIANT); 6] = [
            (UIA_IsKeyboardFocusablePropertyId, VARIANT::from(true)),
            (UIA_IsInvokePatternAvailablePropertyId, VARIANT::from(true)),
            (
                UIA_ControlTypePropertyId,
                VARIANT::from(UIA_ButtonControlTypeId.0),
            ),
            (UIA_IsTogglePatternAvailablePropertyId, VARIANT::from(true)),
            (
                UIA_IsExpandCollapsePatternAvailablePropertyId,
                VARIANT::from(true),
            ),
            (
                UIA_IsSelectionItemPatternAvailablePropertyId,
                VARIANT::from(true),
            ),
        ];

        // OR the "clickable" signals together.
        let mut or_cond: Option<IUIAutomationCondition> = None;
        for (prop, val) in clickable {
            let c = a.CreatePropertyCondition(prop, &val)?;
            or_cond = Some(match or_cond {
                None => c,
                Some(prev) => a.CreateOrCondition(Some(&prev), Some(&c))?,
            });
        }
        let or_cond = or_cond.expect("non-empty clickable set");

        // AND with on-screen + enabled.
        let on_screen =
            a.CreatePropertyCondition(UIA_IsOffscreenPropertyId, &VARIANT::from(false))?;
        let enabled = a.CreatePropertyCondition(UIA_IsEnabledPropertyId, &VARIANT::from(true))?;
        let gate = a.CreateAndCondition(Some(&on_screen), Some(&enabled))?;
        Ok(a.CreateAndCondition(Some(&gate), Some(&or_cond))?)
    }
}

fn scan(a: &IUIAutomation) -> Result<ScanReply> {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return Ok(Vec::new());
        }
        let root = a.ElementFromHandle(fg)?;
        let cond = build_condition(a)?;

        let cache = a.CreateCacheRequest()?;
        cache.AddProperty(UIA_BoundingRectanglePropertyId)?;
        cache.SetTreeScope(TreeScope(TreeScope_Element.0 | TreeScope_Descendants.0))?;

        let found = root.FindAllBuildCache(TreeScope_Descendants, Some(&cond), Some(&cache))?;
        let len = found.Length()?;
        let mut out = Vec::with_capacity(len as usize);
        for i in 0..len {
            let el = found.GetElement(i)?;
            let r = el.CachedBoundingRectangle()?;
            let w = r.right - r.left;
            let h = r.bottom - r.top;
            if w <= 0 || h <= 0 {
                continue; // reject zero-size / undisplayed elements
            }
            out.push((r.left + w / 2, r.top + h / 2));
        }
        Ok(out)
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
                let result = scan(&automation).unwrap_or_else(|e| {
                    tracing::warn!("UIA scan failed: {e}");
                    Vec::new()
                });
                let _ = reply.send(result);
            }
        })
        .expect("spawn uia thread");
    tx
}
