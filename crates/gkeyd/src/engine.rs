//! The engine thread: owns all modal state and turns grabbed key events into
//! remaps, mode switches, cursor motion, clicks, scrolling, and hint sessions.
//!
//! Bindings come from [`Config`] (hot-reloadable). Runs off the hook thread
//! deliberately — anything slow or blocking (input synthesis, UIA scans) must
//! never happen inside the hook callback.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crossbeam_channel::{select, tick, unbounded, Receiver, Sender};
use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

use gkey_core::action::Action;
use gkey_core::config::Config;
use gkey_core::keys::{self, KeyCode};

use crate::hints::{self, Hint};
use crate::hook::KeyEvent;
use crate::input::{self, MouseButton};
use crate::overlay::UiCmd;
use crate::state::{self, Mode};
use crate::uia::ScanReply;

struct Engine {
    config: Config,
    overlay: Sender<UiCmd>,
    uia: Sender<Sender<ScanReply>>,
    fast: bool,
    /// Active hint session (empty when not in hint mode).
    session: Vec<Hint>,
    prefix: String,
    left_control: KeyCode,
    right_alt: KeyCode,
    /// True while a synthetic AltGr LeftControl has been swallowed and we are
    /// waiting to swallow its matching release.
    altgr_fake_ctrl: bool,
    /// Physical keys currently held (per grabbed events), for repeat detection.
    pressed: HashSet<KeyCode>,
    /// Last time a grabbed key event arrived, for idle-timeout recovery.
    last_activity: Instant,
    /// Number keys 1–9 → workspace index 0–8.
    digit_ws: HashMap<KeyCode, usize>,
    /// Remaining grid refinement steps for the active hint session.
    refine_left: u8,
}

/// Sub-grid used when refining a coarse grid cell, and the smallest cell worth
/// refining rather than clicking outright.
const REFINE_COLS: i32 = 3;
const REFINE_ROWS: i32 = 3;
const MIN_REFINE_PX: i32 = 36;

/// Drop out of normal/hint mode after this long with no key activity — recovers
/// from a mode left stuck by e.g. Win+L swallowing the key-ups.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

pub fn run(
    config: Config,
    keys_rx: Receiver<KeyEvent>,
    reload_rx: Receiver<Config>,
    overlay: Sender<UiCmd>,
    uia: Sender<Sender<ScanReply>>,
) {
    let mut eng = Engine {
        config,
        overlay,
        uia,
        fast: false,
        session: Vec::new(),
        prefix: String::new(),
        left_control: keys::parse("LeftControl").expect("LeftControl in table"),
        right_alt: keys::parse("RightAlt").expect("RightAlt in table"),
        altgr_fake_ctrl: false,
        pressed: HashSet::new(),
        last_activity: Instant::now(),
        digit_ws: (1..=9)
            .filter_map(|i| keys::parse(&i.to_string()).map(|k| (k, i - 1)))
            .collect(),
        refine_left: 0,
    };
    let ticker = tick(Duration::from_secs(1));
    loop {
        select! {
            recv(keys_rx) -> msg => match msg {
                Ok(ev) => eng.on_key(ev, &keys_rx),
                Err(_) => break,
            },
            recv(reload_rx) -> msg => if let Ok(cfg) = msg {
                eng.config = cfg;
                tracing::info!("config reloaded");
            },
            recv(ticker) -> _ => eng.reconcile(),
        }
    }
    tracing::info!("engine channel closed, exiting");
}

impl Engine {
    /// AltGr filter, then normal dispatch. On AltGr layouts Windows injects a
    /// synthetic LeftControl immediately before RightAlt; if we've grabbed
    /// LeftControl it would misfire, so when a LeftControl-down is followed
    /// within a short window by a RightAlt-down we treat the ctrl as fake and
    /// swallow both its press and release.
    fn on_key(&mut self, ev: KeyEvent, keys_rx: &Receiver<KeyEvent>) {
        self.last_activity = Instant::now();
        if self.config.altgr {
            if ev.key == self.left_control && !ev.up && !self.altgr_fake_ctrl {
                match keys_rx.recv_timeout(Duration::from_millis(30)) {
                    Ok(next) if next.key == self.right_alt && !next.up => {
                        self.altgr_fake_ctrl = true;
                        self.dispatch(next); // handle the real RightAlt normally
                        return;
                    }
                    Ok(next) => {
                        self.dispatch(ev); // a real LeftControl press
                        self.on_key(next, keys_rx);
                        return;
                    }
                    Err(_) => {
                        self.dispatch(ev);
                        return;
                    }
                }
            }
            if ev.key == self.left_control && ev.up && self.altgr_fake_ctrl {
                self.altgr_fake_ctrl = false; // swallow the fake ctrl release
                return;
            }
        }
        self.dispatch(ev);
    }

    fn dispatch(&mut self, ev: KeyEvent) {
        // Repeat = an OS auto-repeat key-down for a key already held.
        let repeat = if ev.up {
            self.pressed.remove(&ev.key);
            false
        } else {
            !self.pressed.insert(ev.key)
        };
        match state::mode() {
            Mode::Idle => self.handle_idle(ev, repeat),
            Mode::Normal => self.handle_normal(ev, repeat),
            Mode::Hint => self.handle_hint(ev, repeat),
        }
    }

    /// Periodic housekeeping: clear a stuck fast modifier and recover a mode
    /// left stuck after the desktop was locked mid-session (Win+L etc).
    fn reconcile(&mut self) {
        if self.fast {
            if let Some(k) = self.config.faster {
                if !input::is_physically_down(k) {
                    self.fast = false;
                }
            }
        }
        if state::mode() != Mode::Idle && self.last_activity.elapsed() > IDLE_TIMEOUT {
            if state::mode() == Mode::Hint {
                self.end_hint();
            }
            state::set_mode(Mode::Idle);
            self.fast = false;
            self.pressed.clear();
            tracing::info!("idle timeout → idle mode");
        }
    }

    fn handle_idle(&mut self, ev: KeyEvent, repeat: bool) {
        if ev.key == self.config.activation {
            if !ev.up && !repeat {
                state::set_mode(Mode::Normal);
                tracing::info!("→ normal mode");
            }
            return;
        }
        if let Some(&target) = self.config.remaps.get(&ev.key) {
            input::key(target, ev.up);
        }
    }

    fn handle_normal(&mut self, ev: KeyEvent, repeat: bool) {
        if Some(ev.key) == self.config.faster {
            self.fast = !ev.up;
            return;
        }
        // Mode switches fire once per physical press, never on auto-repeat.
        if !ev.up
            && !repeat
            && (ev.key == self.config.normal_exit || ev.key == self.config.activation)
        {
            state::set_mode(Mode::Idle);
            self.fast = false;
            tracing::info!("→ idle mode");
            return;
        }
        if ev.up {
            return;
        }
        // Number keys: N switches to workspace N, Shift+N moves the focused
        // window there (Shift = the `faster` modifier, tracked in self.fast).
        if !repeat && self.config.number_workspaces {
            if let Some(&n) = self.digit_ws.get(&ev.key) {
                if self.fast {
                    crate::tiling::move_window_to(n);
                } else {
                    crate::tiling::switch_to(n);
                }
                return;
            }
        }
        let Some(&action) = self.config.normal_map.get(&ev.key) else {
            return; // unbound keys are swallowed in normal mode
        };
        let step = if self.fast {
            self.config.move_step * self.config.fast_multiplier
        } else {
            self.config.move_step
        };
        let s = self.config.scroll_amount;
        match action {
            // Motion and scrolling ride the OS auto-repeat (held = continuous).
            Action::MoveLeft => input::move_cursor_by(-step, 0),
            Action::MoveRight => input::move_cursor_by(step, 0),
            Action::MoveUp => input::move_cursor_by(0, -step),
            Action::MoveDown => input::move_cursor_by(0, step),
            Action::ScrollUp => input::scroll_vertical(s),
            Action::ScrollDown => input::scroll_vertical(-s),
            Action::ScrollLeft => input::scroll_horizontal(-s),
            Action::ScrollRight => input::scroll_horizontal(s),
            // One-shot actions ignore auto-repeat.
            Action::ClickLeft if !repeat => input::click(MouseButton::Left),
            Action::ClickMiddle if !repeat => input::click(MouseButton::Middle),
            Action::ClickRight if !repeat => input::click(MouseButton::Right),
            Action::Hint if !repeat => self.enter_hint_uia(),
            Action::Grid if !repeat => self.enter_hint_grid(),
            Action::Tile if !repeat => crate::tiling::tile(crate::tiling::Layout::Bsp),
            Action::TileColumns if !repeat => crate::tiling::tile(crate::tiling::Layout::Columns),
            Action::FocusNext if !repeat => crate::tiling::focus(1),
            Action::FocusPrev if !repeat => crate::tiling::focus(-1),
            Action::ToggleTiling if !repeat => {
                let on = !state::auto_tiling();
                state::set_auto_tiling(on);
                tracing::info!("auto-tiling {}", if on { "on" } else { "off" });
                if on {
                    crate::tiling::tile_all(crate::tiling::current_layout());
                }
            }
            // Resize rides auto-repeat (hold to keep growing/shrinking).
            Action::ResizeGrow => crate::tiling::resize(0.05),
            Action::ResizeShrink => crate::tiling::resize(-0.05),
            Action::SwapNext if !repeat => crate::tiling::swap(1),
            Action::SwapPrev if !repeat => crate::tiling::swap(-1),
            Action::WorkspaceNext if !repeat => crate::tiling::workspace_cycle(1),
            Action::WorkspacePrev if !repeat => crate::tiling::workspace_cycle(-1),
            Action::MoveWorkspaceNext if !repeat => crate::tiling::move_cycle(1),
            Action::MoveWorkspacePrev if !repeat => crate::tiling::move_cycle(-1),
            Action::Promote if !repeat => crate::tiling::promote(),
            _ => {}
        }
    }

    fn handle_hint(&mut self, ev: KeyEvent, repeat: bool) {
        if ev.up {
            return;
        }
        if ev.key == self.config.hint_exit || ev.key == self.config.activation {
            if !repeat {
                self.end_hint();
                state::set_mode(Mode::Normal);
            }
            return;
        }
        if ev.key == self.config.hint_backspace {
            self.prefix.pop();
            self.refilter();
            return; // backspace may auto-repeat
        }
        if repeat {
            return; // don't let a held letter re-append
        }
        let Some(ch) = letter_of(ev.key) else {
            return;
        };
        self.prefix.push(ch);

        let matches: Vec<&Hint> = self
            .session
            .iter()
            .filter(|h| h.label.starts_with(&self.prefix))
            .collect();

        match matches.len() {
            0 => {
                self.prefix.pop(); // dead end — ignore rather than cancel
            }
            1 => {
                let h = matches[0].clone();
                // A coarse grid cell refines into a finer grid instead of
                // clicking, so the screen never fills with labels at once.
                if self.refine_left > 0 && h.w >= MIN_REFINE_PX && h.h >= MIN_REFINE_PX {
                    self.refine_left -= 1;
                    let sub = hints::grid_cells(
                        h.cx - h.w / 2,
                        h.cy - h.h / 2,
                        h.w,
                        h.h,
                        REFINE_COLS,
                        REFINE_ROWS,
                    );
                    self.begin_hint(sub);
                } else {
                    self.select(h.cx, h.cy);
                }
            }
            _ => self.refilter(),
        }
    }

    fn refilter(&mut self) {
        let filtered: Vec<Hint> = self
            .session
            .iter()
            .filter(|h| h.label.starts_with(&self.prefix))
            .cloned()
            .collect();
        let _ = self.overlay.send(UiCmd::Show(filtered));
    }

    fn select(&mut self, cx: i32, cy: i32) {
        self.end_hint();
        input::set_cursor(cx, cy);
        input::click(MouseButton::Left);
        state::set_mode(Mode::Normal);
        tracing::info!("hint selected → click at ({cx}, {cy})");
    }

    fn end_hint(&mut self) {
        let _ = self.overlay.send(UiCmd::Hide);
        self.session.clear();
        self.prefix.clear();
    }

    fn begin_hint(&mut self, targets: Vec<hints::Target>) {
        if targets.is_empty() {
            tracing::info!("no hint targets found");
            return;
        }
        let hints = hints::build(&targets, &self.config.hint_chars);
        let _ = self.overlay.send(UiCmd::Show(hints.clone()));
        self.session = hints;
        self.prefix.clear();
        state::set_mode(Mode::Hint);
        tracing::info!("→ hint mode ({} targets)", self.session.len());
    }

    fn enter_hint_uia(&mut self) {
        let (rtx, rrx) = unbounded::<ScanReply>();
        if self.uia.send(rtx).is_err() {
            return;
        }
        // The scanner retries cold accessibility trees, so allow for that.
        let points = rrx
            .recv_timeout(Duration::from_millis(1200))
            .unwrap_or_default();
        if points.is_empty() {
            tracing::info!("UIA scan empty, falling back to grid");
            self.enter_hint_grid();
            return;
        }
        self.refine_left = 0; // element hints are exact — never refine
        self.begin_hint(hints::point_targets(&points));
    }

    fn enter_hint_grid(&mut self) {
        let (l, t, w, h) = crate::tiling::cursor_work_area().unwrap_or_else(|| unsafe {
            (0, 0, GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN))
        });
        let targets = hints::grid_cells(l, t, w, h, self.config.grid_cols, self.config.grid_rows);
        self.refine_left = u8::from(self.config.grid_refine);
        self.begin_hint(targets);
    }
}

/// Lowercase letter produced by a physical key, if it is an A–Z key.
fn letter_of(key: KeyCode) -> Option<char> {
    let name = keys::name_of(key);
    let mut chars = name.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_alphabetic() => Some(c.to_ascii_lowercase()),
        _ => None,
    }
}
