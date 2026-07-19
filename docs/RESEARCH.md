# gkey — Research & Architecture Notes

System-wide Vimium-like keyboard control for Windows 11 + tiling window management + configurable keybindings GUI.

Compiled from source-level reading of: mousemaster, hunt-and-peck, kanata, komorebi, GlazeWM, warpd, capsicain, oblitum/Interception, and Microsoft Learn docs (UIA, low-level hooks, layered windows). July 2026.

---

## 1. Input interception

### Decision: `WH_KEYBOARD_LL` low-level hook. No kernel driver.

The Interception driver route (capsicain, kanata's `kanata_wintercept.exe`) is disqualified for a daily-driver tool:

- Unmaintained since **v1.0.1, May 2017**; officially tested only through Windows 10.
- **Fatal slot bug** (oblitum/Interception#25, open since 2016): hard cap of 10 keyboard / 20 mouse device slots; every reconnect — including **sleep/resume** — consumes a slot. Slots exhausted ⇒ device dead until reboot. Corroborated by kanata docs, AutoHotInterception, Veyon (dropped it).
- Commercial use requires a paid license; driver binary can't be rebuilt without your own WHQL signing.

What the driver buys (pre-login, secure desktop, anticheat games, RDP-fullscreen — per capsicain README) is not worth that cost. Accept LLHOOK's known gaps.

### Hook recipe (proven by kanata + mousemaster)

- `SetWindowsHookExW(WH_KEYBOARD_LL, hook_proc, null, 0)` on a dedicated thread that runs a `GetMessageW` pump. LL hooks are delivered via the installing thread's message queue — no pump, no events, and a starved pump blocks system-wide input.
- **Callback does almost nothing**: parse `KBDLLHOOKSTRUCT`, look up "is this key interesting in the current mode" in a lock-free/atomic snapshot, `try_send` into a **bounded channel** (kanata: `sync_channel(100)`), return `1` to swallow or `CallNextHookEx` to pass. Everything else happens on the engine thread.
- **Timeout hazard**: exceed `LowLevelHooksTimeout` (capped at 1000 ms since Win10 1709) and Windows **silently removes the hook — no notification**. Mitigations, both used by mousemaster:
  - Watchdog (~1 s): poll `GetAsyncKeyState` vs our tracked pressed-set; divergence ⇒ hook likely dead or stuck keys ⇒ reset state / re-install hook.
  - Never call SendInput, UIA, or anything blocking from the hook thread.
- **Self-injection filtering**: tag all injected input with a magic `dwExtraInfo` signature (mousemaster: `0x4D4D4B42` "MMKB") and skip events carrying it. kanata instead skips *all* `LLKHF_INJECTED` events — simpler but also ignores other tools' injected input; prefer the dwExtraInfo signature and make injected-event policy configurable.
- **Reentrancy**: SendInput from processing can re-enter the hook synchronously. mousemaster keeps an `inCallback` flag and defers re-entrant events to a queue.
- **Latency setup**: `timeBeginPeriod(1)` + raise process priority class (kanata uses `REALTIME_PRIORITY_CLASS`; `HIGH_PRIORITY_CLASS` is the safer choice).
- **Injection**: `SendInput` with `KEYEVENTF_SCANCODE` (+ `KEYEVENTF_EXTENDEDKEY` and `0xE0` wScan prefix for the ~48 extended keys), not virtual keys — layout-independent, fixes NumLock/arrow corruption kanata hit with VK mode. Unicode text via `KEYEVENTF_UNICODE`. Batch related strokes in one `SendInput` call for atomic ordering.
- **Key identity**: match bindings on scancodes (kanata "winIOv2" lesson; warpd's Windows alpha died partly on shifted-key handling). Store both scancode and VK; display names via layout mapping.

### Known-unfixable LLHOOK gaps (document, don't fight)

- Win+L locks before key-ups arrive ⇒ stuck-state risk. Mitigate: kanata's `GetAsyncKeyState` cross-check + clear-state-on-idle (60 s).
- Ctrl+Alt+Del, UAC secure desktop: invisible to userspace.
- Other LLHOOK apps (AHK, PowerToys): last-installed hook sees events first; conflicts possible.
- Elevated windows ignore synthetic input from a medium-IL process (UIPI) — see §4 UIAccess.
- AltGr on intl layouts: Windows injects a synthetic LCtrl (scancode `0x21D`) with RAlt. Handle explicitly (kanata's `AltGrBehaviour`, mousemaster's eat-tracking) or intl layouts get stuck modifiers.
- OS key-repeat arrives as repeated WM_KEYDOWN; classify via pressed-set (press already tracked ⇒ repeat). Don't re-run the state machine on repeats — resolve current mapping and re-emit (kanata `handle_repeat`).

## 2. Modal engine

Two-thread split (kanata pattern):
- **Hook thread**: classify + enqueue only.
- **Engine thread**: owns all state. Event-driven with a **1 ms virtual tick** for timing (tap-hold, combo timeouts, mode timeouts). Block on channel recv when nothing time-pending; tick otherwise. Snap the tick clock if >10 ms elapsed (don't replay backlog).

Mode system (mousemaster is the best reference):
- User-defined modes; built-in `idle` (pass-through) mode. Mode = keymap + hint config + grid config + scroll physics + indicator style + timeout (+ fallback mode) + cursor-hide.
- Mode history stack; "previous mode" as a bind target.
- Combo grammar worth copying (mousemaster): `+key` press-and-eat, `#key` press-and-pass, `-key` release, bare tap; `{...}` chords; per-move min/max durations for tap-vs-hold; `_{keys}`/`^{keys}` held/not-held preconditions; key aliases and app aliases (`app-alias.browser=firefox.exe chrome.exe`) for per-app modes; macros to `+key | 'text' | wait-N`.
- Suppress decision must be resolvable in the hook callback from an atomic snapshot of "keys interesting in the current mode" (kanata's `MAPPED_KEYS` pattern). In command modes, suppress everything except explicit pass-throughs.

## 3. Hint mode (Vimium clicks)

### UIA — the right way

hunt-and-peck's slowness is a solved problem: it used `FindAll` with **no CacheRequest**, so every property read was a cross-process COM call. mousemaster + Microsoft docs give the fast recipe:

- **COM `IUIAutomation`** (CoCreate `CUIAutomation8`), never managed `System.Windows.Automation` (legacy, worse coverage, slower). Set `IUIAutomation2::ConnectionTimeout` / `TransactionTimeout` so a hung app can't wedge the scan.
- **Dedicated MTA thread** (`CoInitializeEx(COINIT_MULTITHREADED)`) that owns no windows. UIA from a UI-owning thread can deadlock (our overlay is itself a provider). Never from the hook thread. Return results as a future/channel message.
- **One round trip**: `FindAllBuildCache(TreeScope_Descendants, condition, cacheRequest)` from `ElementFromHandle(foregroundHwnd)`. CacheRequest: add `BoundingRectangle` (+ Name/ControlType if labeling); `put_TreeFilter` = control-view condition; `AutomationElementMode_None` for the scan pass (lightweight, no element refs) — re-resolve only the chosen element if invoking.
- **Hintable condition** (mousemaster's, verbatim property IDs):
  `IsOffscreen=false AND IsEnabled=true AND (IsKeyboardFocusable OR IsInvokePatternAvailable OR ControlType=Button OR IsExpandCollapsePatternAvailable OR IsTogglePatternAvailable OR IsSelectionItemPatternAvailable)`
  — built with `CreateAndCondition`/`CreateOrCondition`; use pattern-*availability* properties (cacheable, cheap), not `GetCurrentPattern` probing (hunt-and-peck's second mistake).
- Post-filter: reject zero rects, require containment in window bounds, dedup near-duplicate centers (mousemaster: within ~13 px·scale). `IsOffscreen` doesn't account for occlusion by other windows — intersect with window rect.
- Windows to scan: foreground window + `EnumThreadWindows` same-thread visible windows, filtered by monitor + ownership (mousemaster does this to stop stray hints from Chromium child windows).
- Cache last element list per window to soften repeat invocations; pre-warm on startup (mousemaster TODO — first query lags).

### Acting on selection

Default **`SendInput` mouse click at the element's clickable point** (works on everything, no pattern support needed); optional UIA `Invoke`/`Toggle`/`Select`/`SetFocus` per-binding (hunt-and-peck's approach — no cursor movement, but only works where patterns exist). Prefer `GetClickablePoint` (fails when obscured — useful signal), fall back to rect center.

Mouse synthesis notes (mousemaster): relative `MOUSEEVENTF_MOVE` then `SetCursorPos` absolute correction (pointer-precision makes pure relative drift); scroll = `MOUSEEVENTF_WHEEL`/`HWHEEL` in 120-unit notches, inertial physics in engine (warpd's scroll accel model feels good); run SendInput on a worker, never the hook thread.

### Fallbacks — mandatory

1. **Grid mode** (warpd): recursive bisection, `u/i/j/k` quadrant shrink + `w/a/s/d` move. Works everywhere including games/legacy apps with empty UIA trees.
2. **Screen-wide hint grid** (warpd hint mode): uniform N×N labeled grid, two-phase coarse→fine (`hint2` 3×3 refinement) for near-pixel accuracy.
3. Normal mode: `hjkl` pointer nudging with accel/decel keys, drag toggle (`v`), click keys, position history (`C-o`/`C-i` vim-style jumps).

### Chromium/Electron reality

- Chromium builds accessibility **on demand**: first UIA client touch (WM_GETOBJECT with UiaRootObjectId) triggers tree construction. Cold window ⇒ first scan may return just a pane; **retry after a beat**.
- Chrome/Chromium ≥ 138 ships **native UIA on by default** (2025 change) — Electron ≥ 37 inherits it. Older Electron: shallow proxy tree unless launched with `--force-renderer-accessibility` / `UiaProvider` feature. Document per-app workaround; grid fallback covers the rest.

### Hint labels

Vimium's exact algorithm, ported in hunt-and-peck (`HintLabelService.cs`): home-row alphabet (`sadfjklewcmpgh`), base-N labels, prefix-free mix of short/long, **reversed** so first chars vary. Prefix-match as user types; dim non-matching; auto-select on unique match. Single-char labels when count ≤ alphabet size (mousemaster).

## 4. Overlay windows

- Styles: `WS_POPUP` + `WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW`. `WS_EX_TRANSPARENT` on a layered window = full click-through; `WS_EX_NOACTIVATE` = no focus theft; `WS_EX_TOOLWINDOW` = hidden from alt-tab. Show `SW_SHOWNOACTIVATE`, position with `SWP_NOACTIVATE`.
  - Do NOT copy hunt-and-peck's overlay (focused modal window, closes on deactivate) — capture keys via our hook instead, overlay stays pure display.
- Rendering: start with **`UpdateLayeredWindow`** (premultiplied 32-bpp DIB, `ULW_ALPHA`) — simple, fine for static hints. Upgrade path: **DirectComposition** (`WS_EX_NOREDIRECTIONBITMAP`, composition swapchain, Direct2D) for animated hints — Kenny Kerr MSDN June 2014 article is the canonical recipe. komorebi draws borders with plain Direct2D HwndRenderTarget per border window, one thread + pump each — fine too.
- One overlay per monitor. **PerMonitorV2** DPI (`SetProcessDpiAwarenessContext`), handle `WM_DPICHANGED`. UIA rects are physical px — with PMv2 they map 1:1 (hunt-and-peck's HiDPI bugs came from system-DPI awareness).
- Topmost churn: restack relative to own windows with `SetWindowPos(hwnd[i], hwnd[i-1], …)` instead of re-asserting HWND_TOPMOST (mousemaster — avoids DWM flicker).
- **Elevated windows**: medium-IL process can't overlay above or click into admin windows (UIPI). Proper fix = **UIAccess**: manifest `uiAccess="true"`, Authenticode-signed, installed under `%ProgramFiles%`. Grants: overlay above shell bands (Start menu etc. via `ZBID_UIACCESS`), input to elevated windows. mousemaster's alternative: winlogon-token respawn hack (requires admin). Ship v1 without; degrade gracefully (detect elevated foreground ⇒ show "unavailable" indicator).
- Optional: `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` to hide overlays from recordings.

## 5. Tiling window manager

komorebi (Rust, master) is the blueprint; GlazeWM (Rust v3) for hotkeys-in-process and crash-watcher patterns.

- **Event source**: `SetWinEventHook(EVENT_MIN..EVENT_MAX, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS)` on its own thread with a message pump; forward through bounded channel (komorebi: crossbeam cap 20). Key events: ObjectShow/Hide/Destroy/Cloaked/Uncloaked, SystemForeground/ObjectFocus, SystemMoveSizeStart/End, MinimizeStart/End, ObjectNameChange (Electron apps set titles late — promote NameChange→Show for configured apps).
- **Manageability filter** (komorebi `should_manage`): visible + real title + not DWM-cloaked (`DwmGetWindowAttribute(DWMWA_CLOAKED)`) + style gate (`WS_CAPTION && WS_EX_WINDOWEDGE`, reject `WS_EX_DLGMODALFRAME`, layered windows only if whitelisted) + app rule lists (ignore/force-manage/floating, matched by Title/Class/Exe/Path × Equals/Prefix/Suffix/Contains/Regex). Ship komorebi's community `applications.json` concept — huge head start on per-app quirks.
- **Positioning**: `SetWindowPos` with `SWP_NOACTIVATE | SWP_NOSENDCHANGING | SWP_NOCOPYBITS | SWP_FRAMECHANGED`. **Shadow compensation**: inflate target rect by delta between `DwmGetWindowAttribute(DWMWA_EXTENDED_FRAME_BOUNDS)` and `GetWindowRect` — else visible edges misalign (komorebi `shadow_rect`).
- **Workspaces = cloaking**, not virtual desktops, not SW_HIDE: `IApplicationView::SetCloak` (undocumented COM; komorebi default, GlazeWM `hide_method: cloak` since PR #792). Instant, no animation, keeps taskbar entry. Track own-hidden hwnds to distinguish self-initiated Hide events.
- **Crash recovery — two layers**:
  1. State dump to `%TEMP%\<app>.state.json` on shutdown, reapply on start (komorebi).
  2. **Separate watcher process** (GlazeWM `wm-watcher`): subscribes to manage/unmanage events over IPC; if the IPC stream dies without a clean `ApplicationExiting`, walks tracked hwnds and un-cloaks/shows/restores everything. Copy this — it's the only guard against hard crashes leaving windows invisible.
- **Layouts**: start BSP (komorebi `recursive_fibonacci`: even index splits vertical, odd horizontal, split ratio applied to remainder) + Columns + Monocle. Manual resize = per-container edge-delta overlays on the computed layout.
- **Focus hack**: `SetForegroundWindow` fails from background processes — komorebi sends a dummy `SendInput` mouse event first (+ `AllowSetForegroundWindow`).
- **Monitors**: hidden message-only window for `WM_DISPLAYCHANGE`, `WM_SETTINGCHANGE(SPI_SETWORKAREA)`, `WM_DEVICECHANGE`, `WM_POWERBROADCAST` (+ lid GUID), session lock/unlock via `WTSRegisterSessionNotification`. On monitor disconnect: cache workspace state keyed by monitor serial, minimize affected windows (stops Windows auto-moving them), restore on reconnect.
- **Borders**: per-border top-level window + Direct2D, tracks target via `EVENT_OBJECT_LOCATIONCHANGE`. **Animations**: separate thread per animated window, interpolate rects at fixed FPS, cancel-aware (komorebi engine) — never in the layout event loop.
- Unlike komorebi (delegates hotkeys to whkd) and like GlazeWM: our hotkeys are in-process — we already own the keyboard hook. This also lets us bind Win-combos whkd can't.

## 6. Process architecture & stack

**Language: Rust** (`windows-rs`). kanata + komorebi + GlazeWM v3 prove every subsystem in production Rust. No GC pauses against the 1000 ms hook budget (GraalVM-compiled mousemaster works, but Rust is the safer default; C# fine for GUI only).

```
gkeyd.exe  (daemon, no console)
├─ hook thread        WH_KEYBOARD_LL (+WH_MOUSE_LL optional) + pump; enqueue only
├─ engine thread      modal state machine, 1 ms tick, combo matching, SendInput out
├─ uia thread (MTA)   hint scans, invoke; ConnectionTimeout set
├─ winevent thread    SetWinEventHook + pump → WM module
├─ wm module          layout, cloak workspaces, monitors, borders (thread per border)
├─ overlay threads    per-monitor hint/indicator windows
└─ ipc server         JSON over local socket/named pipe: commands, queries, event subscriptions
gkey.exe   (CLI)      talks IPC — scriptability from day one (komorebic model)
gkey-watcher.exe      crash janitor: subscribes via IPC, restores cloaked windows on daemon death
gkey-settings.exe     GUI (separate process; Tauri or egui): edits config, live preview via IPC
```

- **Config**: single human-editable file (TOML), hot-reload via file watcher → engine swap-on-idle (kanata reloads only when keys idle — copy that). GUI edits the same file; single source of truth. warpd's pattern: one options table = defaults + docs + `--list-options` dump.
- **IPC protocol**: newline-delimited JSON (kanata TCP + komorebi socket precedent): `ChangeMode`, `Query{state|modes|layout}`, `Reload`, `SetBinding`, event stream `ModeChanged|LayerChanged|WindowManaged|…` for GUI/status-bar/watcher subscribers.
- Singleton via named mutex. Tray icon on GUI or daemon. `timeBeginPeriod(1)`, HIGH_PRIORITY_CLASS, PerMonitorV2 manifest.
- **Code-sign early** — global keyboard hook + input synthesis = classic AV keylogger heuristic.

## 7. Pitfall checklist (condensed)

1. Silent hook removal at LowLevelHooksTimeout ⇒ GetAsyncKeyState watchdog + auto-reinstall.
2. Hook callback: no allocation, no locks held long, no SendInput, no COM. Enqueue and return.
3. GC'd callback references (JNA/.NET) — N/A in Rust, but keep hook closure alive explicitly.
4. dwExtraInfo signature on all injected input; decide policy for others' injected input.
5. AltGr synthetic LCtrl (`0x21D`); extended-key scancodes; NumLock/arrows; Shift+arrow highlight breakage — use scancode I/O throughout.
6. Win+L stuck modifiers ⇒ idle-clear + keystate sync.
7. UIA: MTA thread, CacheRequest always, ConnectionTimeout, never trust `IsOffscreen` for occlusion, retry cold Chromium windows.
8. Elevated windows: no overlay, no input without UIAccess (signed + Program Files). Detect + degrade.
9. Cloaked-window orphans on crash ⇒ watcher process + state file.
10. DWM shadow bounds vs GetWindowRect ⇒ shadow_rect compensation.
11. Mixed DPI ⇒ PMv2 + physical-px consistency end to end.
12. Monitor unplug/sleep/lid/lock ⇒ reconciliation state machine, cache workspaces by monitor serial.
13. Slow-launching apps (SLOW_APPLICATION_IDENTIFIERS) and late-title Electron apps need grace periods.
14. Foreground-lock: dummy input before SetForegroundWindow.
15. AV false positives ⇒ Authenticode signing, no self-extracting installers.

## 8. Suggested build order

1. **M0 skeleton**: hook thread + engine + TOML config + tray + IPC + CLI. Remap proof: caps→esc, modal `leader` key entering command mode.
2. **M1 pointer**: normal mode (hjkl move/click/drag/scroll physics), screen-grid + bisect-grid modes, overlay windows, multi-monitor + DPI.
3. **M2 hints**: UIA thread, cached scan, hint labels, SendInput click + UIA invoke option, Chromium retry, per-app quirks list.
4. **M3 tiling**: WinEvent listener, manageability rules, BSP + monocle, cloak workspaces, watcher process, borders, monitor reconciliation.
5. **M4 GUI**: settings app over IPC — binding editor (capture keys via daemon), mode editor, app rules, live reload.
6. **M5 polish**: animations, app-specific modes, position history, UIAccess-signed build.

## Sources

- https://github.com/petoncle/mousemaster — modal engine, UIA hint condition, overlay, combo grammar
- https://github.com/zsims/hunt-and-peck — UIA hints in C#, vimium label algorithm, what not to do (no cache, system DPI)
- https://github.com/jtroo/kanata — hook thread recipe, scancode I/O, AltGr, TCP control, live reload
- https://github.com/LGUG2Z/komorebi — tiling architecture end to end
- https://github.com/glzr-io/glazewm — in-process hotkeys, cloak hiding, wm-watcher crash janitor
- https://github.com/rvaiya/warpd — mode UX, grid/hint design, config style
- https://github.com/cajhin/capsicain + https://github.com/oblitum/Interception — driver route (rejected: slot bug #25, 2017-abandoned, commercial license)
- Microsoft Learn: SetWindowsHookEx, LowLevelKeyboardProc, UIA client/caching/threading/security docs, UpdateLayeredWindow, SetWindowDisplayAffinity
- https://blog.adeltax.com/window-z-order-in-windows-10/ — z-order bands, ZBID_UIACCESS
- https://developer.chrome.com/blog/windows-uia-support-update — native UIA default since Chrome 138
