//! gkey daemon — system-wide modal keyboard control (remaps, a Vimium-style
//! normal mode for cursor/scroll/click/hints) plus a tiling window manager.
//!
//! Runs as a background (GUI-subsystem) process with no console; control it
//! from the tray icon or the settings GUI, and read logs from the log file.

#![windows_subsystem = "windows"]

mod engine;
mod hints;
mod hook;
mod input;
mod overlay;
mod state;
mod tiling;
mod tray;
mod uia;
mod watchdog;
mod winevent;

use std::path::PathBuf;
use std::time::Duration;

use gkey_core::config::Config;
use gkey_core::keys;
use windows::Win32::Foundation::{BOOL, FALSE, TRUE};
use windows::Win32::System::Console::SetConsoleCtrlHandler;

use anyhow::Result;
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThreadId, SetPriorityClass, HIGH_PRIORITY_CLASS,
};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, TranslateMessage, MSG,
};

unsafe extern "system" fn ctrl_handler(_ctrl_type: u32) -> BOOL {
    tiling::restore_all();
    FALSE // let the default handler proceed to terminate the process
}

/// Whether the OS granted this process UIAccess (signed exe in a secure
/// location with a uiAccess manifest). With it, the overlay can draw above
/// the shell's Start/Search flyouts.
fn has_uiaccess() -> bool {
    use windows::Win32::Security::{GetTokenInformation, TokenUIAccess, TOKEN_QUERY};
    use windows::Win32::System::Threading::OpenProcessToken;
    unsafe {
        let mut token = windows::Win32::Foundation::HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut val: u32 = 0;
        let mut len: u32 = 0;
        let ok = GetTokenInformation(
            token,
            TokenUIAccess,
            Some(&mut val as *mut u32 as *mut _),
            std::mem::size_of::<u32>() as u32,
            &mut len,
        )
        .is_ok();
        let _ = windows::Win32::Foundation::CloseHandle(token);
        ok && val != 0
    }
}

/// Launch the crash-restore watcher (next to this exe), passing our PID.
fn spawn_watcher() {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("gkey-watcher.exe")));
    match exe {
        Some(exe) if exe.exists() => {
            let _ = std::process::Command::new(exe)
                .arg(std::process::id().to_string())
                .spawn();
        }
        _ => tracing::info!("gkey-watcher.exe not found; crash-restore disabled"),
    }
}

fn config_path() -> PathBuf {
    if let Some(arg) = std::env::args().nth(1) {
        return PathBuf::from(arg);
    }
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("gkey").join("config.toml")
}

/// Debounce window-event pings and re-tile when auto-tiling is enabled.
fn spawn_tiler(rx: crossbeam_channel::Receiver<()>) {
    std::thread::Builder::new()
        .name("tiler".into())
        .spawn(move || {
            while rx.recv().is_ok() {
                if !state::auto_tiling() {
                    continue;
                }
                // Coalesce a burst of events into one retile.
                while rx.recv_timeout(Duration::from_millis(150)).is_ok() {}
                if state::auto_tiling() {
                    tiling::tile_all(tiling::current_layout());
                }
            }
        })
        .expect("spawn tiler thread");
}

/// Poll the config file's mtime once a second; on change, re-resolve and push
/// the new config to the engine (and refresh the idle grab set). A parse error
/// keeps the previous config.
fn spawn_reloader(path: PathBuf, tx: crossbeam_channel::Sender<Config>) {
    std::thread::Builder::new()
        .name("reload".into())
        .spawn(move || {
            let mtime = |p: &PathBuf| std::fs::metadata(p).and_then(|m| m.modified()).ok();
            let mut last = mtime(&path);
            loop {
                std::thread::sleep(Duration::from_secs(1));
                let cur = mtime(&path);
                if cur == last {
                    continue;
                }
                last = cur;
                match Config::load(&path) {
                    Ok(cfg) => {
                        state::set_idle_grab(cfg.idle_grab());
                        state::set_auto_tiling(cfg.auto_tiling);
                        tiling::set_float_rules(cfg.float_rules.clone());
                        tiling::set_gaps(cfg.gap, cfg.outer_gap);
                        if tx.send(cfg).is_err() {
                            break;
                        }
                        tracing::info!("config file changed → reloaded");
                    }
                    Err(e) => tracing::warn!("config reload failed, keeping previous: {e}"),
                }
            }
        })
        .expect("spawn reload thread");
}

fn main() -> Result<()> {
    // Log to a daily-rotated file (no console — this is a GUI-subsystem app).
    let log_dir = {
        let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
        PathBuf::from(base).join("gkey")
    };
    let _ = std::fs::create_dir_all(&log_dir);
    let (writer, _log_guard) =
        tracing_appender::non_blocking(tracing_appender::rolling::daily(&log_dir, "gkeyd.log"));
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .with_writer(writer)
        .init();
    tracing::info!("gkey daemon starting; logging to {}", log_dir.display());
    tracing::info!(
        "UIAccess: {} (overlay {} draw over Start/Search)",
        has_uiaccess(),
        if has_uiaccess() { "can" } else { "cannot" }
    );

    // Per-monitor DPI v2 so cursor coordinates are true physical pixels across
    // mixed-DPI monitors (matters once overlays/hints land).
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    let path = config_path();
    let config = Config::load(&path)?;
    let activation = config.activation;
    tracing::info!(
        "loaded config: activation={}, {} remap(s)",
        keys::name_of(activation),
        config.remaps.len()
    );

    // Keys the hook must grab while idle: remap sources plus the activation key.
    state::set_idle_grab(config.idle_grab());
    state::set_auto_tiling(config.auto_tiling);
    tiling::set_float_rules(config.float_rules.clone());
    tiling::set_gaps(config.gap, config.outer_gap);

    // Restore any workspace-hidden windows if the console is closed / Ctrl-C'd,
    // so windows aren't left invisible when the daemon exits.
    unsafe {
        let _ = SetConsoleCtrlHandler(Some(ctrl_handler), TRUE);
    }
    // And a separate watcher process covers hard kills / crashes.
    spawn_watcher();

    let (tx, rx) = crossbeam_channel::bounded::<hook::KeyEvent>(256);
    let (reload_tx, reload_rx) = crossbeam_channel::unbounded::<Config>();

    // Overlay (hint rendering) and UIA (element scanning) each own a thread.
    let overlay_tx = overlay::spawn();
    let uia_tx = uia::spawn();
    tiling::set_indicator_sink(overlay_tx.clone());

    // Live window tracking → debounced auto-tiling.
    let (winevent_tx, winevent_rx) = crossbeam_channel::bounded::<()>(64);
    winevent::spawn(winevent_tx);
    spawn_tiler(winevent_rx);

    // Engine owns the config and runs off the input path.
    std::thread::Builder::new()
        .name("engine".into())
        .spawn(move || engine::run(config, rx, reload_rx, overlay_tx, uia_tx))?;

    // Watch the config file and hot-reload on change.
    spawn_reloader(path, reload_tx);

    // Nudge scheduling/timer resolution for low-latency input handling. The
    // hook thread also gets time-critical priority: under heavy load (video
    // calls, games) a starved hook misses the OS hook timeout, keys lag, and
    // the watchdog trips reinstalls.
    unsafe {
        let _ = SetPriorityClass(GetCurrentProcess(), HIGH_PRIORITY_CLASS);
        let _ = windows::Win32::System::Threading::SetThreadPriority(
            windows::Win32::System::Threading::GetCurrentThread(),
            windows::Win32::System::Threading::THREAD_PRIORITY_TIME_CRITICAL,
        );
        windows::Win32::Media::timeBeginPeriod(1);
    }

    // Install the hook on this thread; it must then pump messages. The watchdog
    // posts WM_APP_REINSTALL here if the hook goes silent.
    let activation_name = keys::name_of(activation);
    let hook_tid = unsafe { GetCurrentThreadId() };
    let mut hook_handle = hook::install(tx)?;
    watchdog::spawn(hook_tid);
    if tray::install() {
        tracing::info!("tray icon added");
    }
    tracing::info!("keyboard hook installed — press {activation_name} to enter normal mode");

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if msg.message == watchdog::WM_APP_REINSTALL {
                hook_handle = hook::reinstall(hook_handle);
                state::set_mode(state::Mode::Idle);
                tracing::info!("keyboard hook reinstalled by watchdog");
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    Ok(())
}
