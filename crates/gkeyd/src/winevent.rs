//! Live window-event tracking via SetWinEventHook.
//!
//! A dedicated thread installs an out-of-context WinEvent hook and pumps
//! messages (hook callbacks are delivered through the thread's message queue).
//! For window lifecycle events (show/hide/destroy/minimize/cloak) on top-level
//! windows it pings a channel; the tiler thread (in `main`) debounces those
//! pings and re-tiles when auto-tiling is enabled.

use std::cell::RefCell;

use crossbeam_channel::Sender;
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, TranslateMessage, EVENT_MAX, EVENT_MIN, EVENT_OBJECT_CLOAKED,
    EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE, EVENT_OBJECT_SHOW, EVENT_OBJECT_UNCLOAKED,
    EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZESTART, MSG, OBJID_WINDOW, WINEVENT_OUTOFCONTEXT,
    WINEVENT_SKIPOWNPROCESS,
};

thread_local! {
    static TX: RefCell<Option<Sender<()>>> = const { RefCell::new(None) };
}

unsafe extern "system" fn win_event_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    // Only top-level window lifecycle events matter for tiling.
    if hwnd.0.is_null() || id_object != OBJID_WINDOW.0 || id_child != 0 {
        return;
    }
    let relevant = matches!(
        event,
        EVENT_OBJECT_SHOW
            | EVENT_OBJECT_HIDE
            | EVENT_OBJECT_DESTROY
            | EVENT_OBJECT_CLOAKED
            | EVENT_OBJECT_UNCLOAKED
            | EVENT_SYSTEM_MINIMIZESTART
            | EVENT_SYSTEM_MINIMIZEEND
    );
    if !relevant {
        return;
    }
    TX.with(|t| {
        if let Some(tx) = t.borrow().as_ref() {
            let _ = tx.try_send(());
        }
    });
}

pub fn spawn(tx: Sender<()>) {
    std::thread::Builder::new()
        .name("winevent".into())
        .spawn(move || unsafe {
            TX.with(|t| *t.borrow_mut() = Some(tx));
            let hook = SetWinEventHook(
                EVENT_MIN,
                EVENT_MAX,
                HMODULE::default(),
                Some(win_event_proc),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            );
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let _ = UnhookWinEvent(hook);
        })
        .expect("spawn winevent thread");
}
