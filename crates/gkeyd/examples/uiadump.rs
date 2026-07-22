//! Diagnostic: dump the UIA tree of a window and show which elements the
//! hint scanner's condition would accept.
//!
//! Usage: cargo run -p gkeyd --example uiadump -- <window title substring>

use windows::core::VARIANT;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, TRUE};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowTextW, IsWindowVisible};

struct FindCtx {
    needle: String,
    found: Vec<(HWND, String, String)>,
}

unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = &mut *(lparam.0 as *mut FindCtx);
    if !IsWindowVisible(hwnd).as_bool() {
        return TRUE;
    }
    let mut buf = [0u16; 512];
    let n = GetWindowTextW(hwnd, &mut buf);
    if n > 0 {
        let title = String::from_utf16_lossy(&buf[..n as usize]);
        if title.to_lowercase().contains(&ctx.needle) {
            let mut cbuf = [0u16; 256];
            let cn = windows::Win32::UI::WindowsAndMessaging::GetClassNameW(hwnd, &mut cbuf);
            let class = String::from_utf16_lossy(&cbuf[..cn.max(0) as usize]);
            ctx.found.push((hwnd, title, class));
        }
    }
    TRUE
}

fn type_name(id: i32) -> &'static str {
    match id {
        50000 => "Button",
        50001 => "Calendar",
        50002 => "CheckBox",
        50003 => "ComboBox",
        50004 => "Edit",
        50005 => "Hyperlink",
        50006 => "Image",
        50007 => "ListItem",
        50008 => "List",
        50009 => "Menu",
        50010 => "MenuBar",
        50011 => "MenuItem",
        50012 => "ProgressBar",
        50013 => "RadioButton",
        50014 => "ScrollBar",
        50015 => "Slider",
        50016 => "Spinner",
        50017 => "StatusBar",
        50018 => "Tab",
        50019 => "TabItem",
        50020 => "Text",
        50021 => "ToolBar",
        50022 => "ToolTip",
        50023 => "Tree",
        50024 => "TreeItem",
        50025 => "Custom",
        50026 => "Group",
        50027 => "Thumb",
        50028 => "DataGrid",
        50029 => "DataItem",
        50030 => "Document",
        50031 => "SplitButton",
        50032 => "Window",
        50033 => "Pane",
        50034 => "Header",
        50035 => "HeaderItem",
        50036 => "Table",
        50037 => "TitleBar",
        50038 => "Separator",
        50039 => "SemanticZoom",
        50040 => "AppBar",
        _ => "?",
    }
}

fn main() {
    let needle = std::env::args().nth(1).expect("usage: uiadump <title substring>").to_lowercase();
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let mut ctx = FindCtx { needle, found: Vec::new() };
        let _ = EnumWindows(Some(enum_cb), LPARAM(&mut ctx as *mut _ as isize));
        if ctx.found.is_empty() {
            eprintln!("no visible window matching");
            std::process::exit(1);
        }
        let a: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_ALL).unwrap();
        for (hwnd, title, class) in ctx.found {
            eprintln!("== window: {title:?} class={class:?} hwnd={:?}", hwnd.0);
            dump_window(&a, hwnd);
        }
    }
}

fn dump_window(a: &IUIAutomation, hwnd: HWND) {
    unsafe {
        let Ok(root) = a.ElementFromHandle(hwnd) else {
            eprintln!("ElementFromHandle failed");
            return;
        };

        let cache = a.CreateCacheRequest().unwrap();
        for p in [
            UIA_ControlTypePropertyId,
            UIA_NamePropertyId,
            UIA_BoundingRectanglePropertyId,
            UIA_IsOffscreenPropertyId,
            UIA_IsEnabledPropertyId,
            UIA_IsKeyboardFocusablePropertyId,
            UIA_IsInvokePatternAvailablePropertyId,
            UIA_IsTogglePatternAvailablePropertyId,
            UIA_IsExpandCollapsePatternAvailablePropertyId,
            UIA_IsSelectionItemPatternAvailablePropertyId,
            UIA_LegacyIAccessibleDefaultActionPropertyId,
        ] {
            cache.AddProperty(p).unwrap();
        }
        let cond = a.CreateTrueCondition().unwrap();
        let all = root.FindAllBuildCache(TreeScope_Descendants, Some(&cond), Some(&cache)).unwrap();
        let len = all.Length().unwrap();
        eprintln!("total elements: {len}");

        // Mirror of the scanner's accepted control types.
        let accepted_types = [
            50000, 50031, 50005, 50011, 50007, 50019, 50024, 50002, 50013, 50003, 50004, 50015,
            50016, 50035,
        ];

        for i in 0..len {
            let Ok(el) = all.GetElement(i) else { continue };
            let ct = el.CachedControlType().map(|c| c.0).unwrap_or(0);
            let name = el.CachedName().map(|s| s.to_string()).unwrap_or_default();
            let r = el.CachedBoundingRectangle().unwrap_or_default();
            let (w, h) = (r.right - r.left, r.bottom - r.top);
            let off = el.CachedIsOffscreen().map(|b| b.as_bool()).unwrap_or(false);
            let en = el.CachedIsEnabled().map(|b| b.as_bool()).unwrap_or(true);
            let focus = el.CachedIsKeyboardFocusable().map(|b| b.as_bool()).unwrap_or(false);
            let get_bool = |p| {
                el.GetCachedPropertyValue(p)
                    .ok()
                    .map(|v: VARIANT| bool::try_from(&v).unwrap_or(false))
                    .unwrap_or(false)
            };
            let inv = get_bool(UIA_IsInvokePatternAvailablePropertyId);
            let tog = get_bool(UIA_IsTogglePatternAvailablePropertyId);
            let exp = get_bool(UIA_IsExpandCollapsePatternAvailablePropertyId);
            let sel = get_bool(UIA_IsSelectionItemPatternAvailablePropertyId);
            let action = el
                .GetCachedPropertyValue(UIA_LegacyIAccessibleDefaultActionPropertyId)
                .ok()
                .map(|v: VARIANT| v.to_string())
                .unwrap_or_default();

            let matched = !off && en && (focus || inv || tog || exp || sel || accepted_types.contains(&ct));
            // Only print rows that are plausibly interactive or that we match,
            // to keep output readable.
            let interesting = matched || !action.is_empty() || inv || tog || exp || sel || focus;
            if !interesting {
                continue;
            }
            println!(
                "{} type={} name={:?} rect={}x{} off={} en={} focus={} inv={} tog={} exp={} sel={} action={:?}",
                if matched { "HIT " } else { "MISS" },
                type_name(ct),
                name.chars().take(40).collect::<String>(),
                w, h, off as u8, en as u8, focus as u8, inv as u8, tog as u8, exp as u8, sel as u8,
                action.chars().take(20).collect::<String>(),
            );
        }
    }
}
