# gkey

System-wide, keyboard-only control for Windows 11 — Vimium for the whole
desktop. Enter a modal "normal" mode and drive the cursor, click, and scroll
without touching the mouse. Physical-key remaps run globally.

Tiling window management and UIA hint-mode clicking are planned (see
[docs/RESEARCH.md](docs/RESEARCH.md) for the full architecture); this repo is
currently at **phase 1**.

## Status

**M0–M1 (pointer control) — built & runtime-smoke-tested.**
**M2 (hint mode + overlay) — built & compiles clean; not yet runtime-verified on
this dev machine** (Smart App Control blocks running the unsigned binary here —
see [Running under Smart App Control](#running-under-smart-app-control)).

- Global `WH_KEYBOARD_LL` hook with a hook-thread/engine-thread split; the
  callback only classifies and enqueues, so it stays well under the
  `LowLevelHooksTimeout` that silently unhooks slow callbacks.
- Modal engine: **idle** (typing + remaps), **normal** (pointer), **hint**.
- Self-injection filtering via a `dwExtraInfo` signature; scancode-based
  `SendInput` so bindings are keyboard-layout independent.
- **Hook watchdog** — a heartbeat detects the silent hook removal Windows does
  when a callback exceeds `LowLevelHooksTimeout`, and reinstalls the hook on the
  hook thread so the daemon can't go permanently deaf.
- **AltGr handling** — swallows the synthetic LeftControl that AltGr layouts
  inject before RightAlt, so a grabbed/remapped LeftControl doesn't misfire on
  every AltGr press (`[general].altgr`, on by default; toggle in the GUI).
- **Key-repeat classification** — motion/scroll ride the OS auto-repeat
  (hold = continuous); one-shot actions (clicks, hint/grid, mode switches) fire
  once per physical press, so holding a key can't rapid-toggle modes.
- **Stuck-state reconciliation** — a 1s ticker clears a stuck fast modifier
  (cross-checked with `GetAsyncKeyState`) and drops back to idle after 60s of
  inactivity, recovering from a mode left stuck by Win+L.
- **Tiling** — `t` tiles the monitor under the cursor (BSP), columns layout
  available, `n`/`b` move focus. Filters to real app windows (visible, titled,
  not cloaked/tool), compensates the DWM shadow frame. **Live auto-tiling**
  (`[tiling].auto`, toggle at runtime): a `SetWinEventHook` thread watches
  window show/hide/destroy/minimize/cloak events and a debounced tiler re-tiles
  every affected monitor automatically. **Resize** (`=`/`-`) adjusts the master
  area ratio; **swap** (`o`/`i`) exchanges the focused window with a neighbour;
  **promote** sends the focused window to the master slot.
  **Per-app float rules** (`[[tiling.float]]`, matched by exe/class/title) keep
  dialogs, pop-ups, and chosen apps out of the tiling (editable in the GUI).
  **Configurable gaps** (`[tiling].gap` between windows, `outer_gap` at the
  screen edge).
- **Workspaces** — 9 **per-monitor** virtual workspaces (windows hidden/shown
  per workspace, GlazeWM-style); switching affects only the monitor under the
  cursor. Direct **number-key switching** (`1`–`9` in normal mode, toggleable)
  plus cycle-switch and move-focused-window bindings, with a **"Workspace N"
  indicator toast** shown on the overlay when you switch. Orphan safety:
  an in-process console handler restores hidden windows on clean exit, and a
  separate `gkey-watcher` process restores them after a hard kill / crash.
- **Hint mode**: `f` scans the foreground window's clickable controls via UI
  Automation (one cached round-trip on a dedicated MTA thread) and labels them;
  `g` overlays a uniform grid as a fallback for apps with no accessibility tree.
  A click-through, top-most, colour-keyed layered overlay draws the labels; type
  the label to move+click.
- **Fully config-driven bindings** (`[normal]`, `[hint]`, `[remap]`) with **live
  hot-reload** — edit `%APPDATA%\gkey\config.toml`, save, and changes apply
  within ~1s without restarting the daemon.
- **Settings GUI** (`gkey-settings.exe`) — a native window with a labelled
  dropdown for every binding plus the remaps. Each binding also has a **Set**
  button: click it and press the key you want (a temporary low-level hook grabs
  the keypress and swallows it so the running daemon doesn't react; `Esc`
  cancels). A **Start/Stop daemon** button launches `gkeyd.exe` (from the same
  folder) or terminates it, with the label auto-tracking whether it's running.
  Save validates and writes the config, which the daemon hot-reloads. No
  restart, no hand-editing TOML.

## Layout

A Cargo workspace of four crates:

- `gkey-core` — shared key / action / config model (pure, no Win32).
- `gkeyd` — the daemon (hook, engine, overlay, UIA, tiling).
- `gkey-settings` — the settings GUI (native Win32 via `windows-rs`).
- `gkey-watcher` — crash-restore watcher; the daemon spawns it with its PID, it
  waits on the daemon and un-hides any workspace-hidden windows if the daemon
  dies without cleaning up. Keep it next to `gkeyd.exe`.

The GUI edits the same config the daemon watches, so they stay in sync through
one file — no IPC needed yet.

Privacy note: in idle mode the hook only grabs remap keys and the activation
key — ordinary typing (passwords included) is never captured. All keys are
captured only in the user-invoked normal/hint modes, and nothing is written to
disk.

### Normal-mode keys (defaults)

| Key | Action |
|-----|--------|
| `h` `j` `k` `l` | Move cursor left / down / up / right |
| hold `Shift` | Move faster (`fast_multiplier`×) |
| `m` `,` `.` | Left / middle / right click |
| `e` / `d` | Scroll up / down |
| `f` | Hint mode over UI-Automation elements |
| `g` | Hint mode over a screen grid (fallback) |
| `t` | Tile monitor under cursor (BSP) |
| `1`–`9` | Switch cursor monitor to workspace N |
| `Shift`+`1`–`9` | Move focused window to workspace N |
| `n` / `b` | Focus next / previous window |
| `=` / `-` | Grow / shrink the master area |
| `o` / `i` | Swap focused window with next / previous |
| `Esc` or `CapsLock` | Back to idle |

Enter normal mode with **CapsLock** (configurable). In hint mode, type a label to
click it, `Backspace` to correct, `Esc` to cancel.

## Running under Smart App Control

Windows 11's **Smart App Control (SAC)** blocks unsigned / low-reputation
executables — it will refuse to launch `gkeyd.exe`. SAC only trusts apps signed
by a CA in Microsoft's Trusted Root Program (self-signing does not clear it).

Two ways to run gkey — **see [RUNNING.md](RUNNING.md) for step-by-step**:

- **Toggle SAC off** (free, and *reversible* on current Windows 11 — the earlier
  "one-way door" no longer applies), then run the binaries. Simplest for
  personal use.
- **Sign with a trusted cert** and run with SAC on. Cheapest legit route is
  Azure Trusted Signing (~$9.99/mo, RSA); use [scripts/sign.ps1](scripts/sign.ps1).

## Build & run

Requires the Rust MSVC toolchain.

```powershell
cargo build --release
.\target\release\gkeyd.exe            # uses %APPDATA%\gkey\config.toml, or defaults
.\target\release\gkeyd.exe my.toml    # or an explicit config path
```

Copy [config.example.toml](config.example.toml) to
`%APPDATA%\gkey\config.toml` to customise. The daemon runs in the foreground and
logs to stdout; close the console to stop it.

> Global keyboard hooks and synthetic input look like a keylogger to antivirus
> heuristics — sign the binary before distributing it.

## Roadmap

- **GUI polish** — dynamic add/remove remap rows, comment-preserving config
  writes (`toml_edit`), a system-tray icon.
- **M2 hardening** — runtime-verify hint mode broadly; add `IUIAutomation2`
  connection timeouts, cold-Chromium retry, occlusion/monitor filtering of
  hints, two-phase grid refinement.
- **M3 tiling — next** — named/labeled workspaces; packaging (tray icon,
  installer, a code-signing story for Smart App Control).

> The GUI is native Win32, not a web/`egui` app, because Smart App Control on the
> dev machine blocks running the build-script binaries those stacks require.
