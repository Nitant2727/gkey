//! Configuration model (TOML). Bindings for normal mode and hint mode are now
//! fully config-driven; the file is hot-reloaded on change (see `main`).

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::action::Action;
use crate::keys::{self, KeyCode};

// ---------------------------------------------------------------------------
// Raw (as-parsed) config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RawConfig {
    pub general: General,
    pub normal: NormalRaw,
    pub hint: HintRaw,
    pub tiling: TilingRaw,
    /// Physical-key remaps active in idle mode, e.g. `CapsLock = "Escape"`.
    pub remap: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TilingRaw {
    /// Auto-retile when windows open/close/minimize. Default off.
    pub auto: bool,
    /// Gap between adjacent tiled windows (pixels).
    pub gap: i32,
    /// Margin between the tiling and the screen edge (pixels).
    pub outer_gap: i32,
    /// In normal mode, keys 1–9 switch the cursor monitor's workspace directly.
    pub number_keys: bool,
    /// Windows matching any of these rules float (are excluded from tiling).
    pub float: Vec<FloatRule>,
}

impl Default for TilingRaw {
    fn default() -> Self {
        Self {
            auto: false,
            gap: 8,
            outer_gap: 8,
            number_keys: true,
            float: Vec::new(),
        }
    }
}

/// A window matcher. A rule matches when every field it specifies matches
/// (case-insensitive substring); a rule with no fields matches nothing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FloatRule {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exe: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

impl FloatRule {
    pub fn is_empty(&self) -> bool {
        self.exe.is_none() && self.class.is_none() && self.title.is_none()
    }

    /// Does this rule match a window with the given (exe, class, title)?
    pub fn matches(&self, exe: &str, class: &str, title: &str) -> bool {
        if self.is_empty() {
            return false;
        }
        let has = |field: &Option<String>, hay: &str| match field {
            Some(needle) => hay.to_lowercase().contains(&needle.to_lowercase()),
            None => true,
        };
        has(&self.exe, exe) && has(&self.class, class) && has(&self.title, title)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    /// Key that enters normal (pointer) mode from idle.
    pub activation: String,
    /// Pixels moved per motion press in normal mode.
    pub move_step: i32,
    /// Multiplier applied to move_step while the `faster` key is held.
    pub fast_multiplier: i32,
    /// Wheel notches per scroll press.
    pub scroll_amount: i32,
    /// Swallow the synthetic LeftControl that AltGr layouts inject before
    /// RightAlt (prevents it misfiring a LeftControl binding). Default on.
    pub altgr: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NormalRaw {
    pub move_left: String,
    pub move_right: String,
    pub move_up: String,
    pub move_down: String,
    pub faster: String,
    pub click_left: String,
    pub click_middle: String,
    pub click_right: String,
    pub scroll_up: String,
    pub scroll_down: String,
    pub scroll_left: String,
    pub scroll_right: String,
    pub hint: String,
    pub grid: String,
    /// Tiling (empty = unbound).
    pub tile: String,
    pub tile_columns: String,
    pub focus_next: String,
    pub focus_prev: String,
    pub toggle_tiling: String,
    pub resize_grow: String,
    pub resize_shrink: String,
    pub swap_next: String,
    pub swap_prev: String,
    pub workspace_next: String,
    pub workspace_prev: String,
    pub move_workspace_next: String,
    pub move_workspace_prev: String,
    pub promote: String,
    /// Key that leaves normal mode back to idle (activation key also toggles).
    pub exit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HintRaw {
    /// Alphabet used to build hint labels.
    pub chars: String,
    /// Cancel hint mode.
    pub exit: String,
    /// Delete the last typed label character.
    pub backspace: String,
}

impl Default for General {
    fn default() -> Self {
        Self {
            activation: "CapsLock".into(),
            move_step: 40,
            fast_multiplier: 4,
            scroll_amount: 3,
            altgr: true,
        }
    }
}

impl Default for NormalRaw {
    fn default() -> Self {
        Self {
            move_left: "H".into(),
            move_right: "L".into(),
            move_up: "K".into(),
            move_down: "J".into(),
            faster: "LeftShift".into(),
            click_left: "M".into(),
            click_middle: "Comma".into(),
            click_right: "Period".into(),
            scroll_up: "E".into(),
            scroll_down: "D".into(),
            scroll_left: "W".into(),
            scroll_right: "R".into(),
            hint: "F".into(),
            grid: "G".into(),
            tile: "T".into(),
            tile_columns: "".into(),
            focus_next: "N".into(),
            focus_prev: "B".into(),
            toggle_tiling: "".into(),
            resize_grow: "Equals".into(),
            resize_shrink: "Minus".into(),
            swap_next: "O".into(),
            swap_prev: "I".into(),
            workspace_next: "".into(),
            workspace_prev: "".into(),
            move_workspace_next: "".into(),
            move_workspace_prev: "".into(),
            promote: "".into(),
            exit: "Escape".into(),
        }
    }
}

impl Default for HintRaw {
    fn default() -> Self {
        Self {
            chars: "sadfjklewcmpgh".into(),
            exit: "Escape".into(),
            backspace: "Backspace".into(),
        }
    }
}

impl Default for RawConfig {
    fn default() -> Self {
        Self {
            general: General::default(),
            normal: NormalRaw::default(),
            hint: HintRaw::default(),
            tiling: TilingRaw::default(),
            remap: HashMap::new(),
        }
    }
}

impl RawConfig {
    /// Read the raw (unresolved) config from disk, or defaults if absent.
    pub fn load_raw(path: &Path) -> Result<Self> {
        if path.exists() {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("reading config {}", path.display()))?;
            toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))
        } else {
            Ok(Self::default())
        }
    }

    /// Validate (via [`Config::resolve`]) then write to disk, creating parent
    /// directories as needed. Returns an error without writing if invalid.
    pub fn save(&self, path: &Path) -> Result<()> {
        Config::resolve(self.clone()).context("config is invalid; not saved")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let text = toml::to_string_pretty(self).context("serializing config")?;
        std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Resolved config used by the engine
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Config {
    pub activation: KeyCode,
    pub move_step: i32,
    pub fast_multiplier: i32,
    pub scroll_amount: i32,
    pub altgr: bool,
    /// Live auto-tiling on window events.
    pub auto_tiling: bool,
    /// Tiling gaps (pixels).
    pub gap: i32,
    pub outer_gap: i32,
    /// Keys 1–9 switch workspaces directly in normal mode.
    pub number_workspaces: bool,
    /// Windows that should float (be excluded from tiling).
    pub float_rules: Vec<FloatRule>,

    /// Normal-mode key → action.
    pub normal_map: HashMap<KeyCode, Action>,
    /// Modifier held for faster motion.
    pub faster: Option<KeyCode>,
    /// Leave normal mode.
    pub normal_exit: KeyCode,

    /// Alphabet for hint labels.
    pub hint_chars: Vec<char>,
    pub hint_exit: KeyCode,
    pub hint_backspace: KeyCode,

    pub remaps: HashMap<KeyCode, KeyCode>,
}

fn key(name: &str, what: &str) -> Result<KeyCode> {
    keys::parse(name).with_context(|| format!("unknown {what} key: {name}"))
}

impl Config {
    pub fn resolve(raw: RawConfig) -> Result<Self> {
        let n = &raw.normal;
        let mut normal_map: HashMap<KeyCode, Action> = HashMap::new();
        let mut bind = |name: &str, what: &str, action: Action| -> Result<()> {
            let name = name.trim();
            if name.is_empty() {
                return Ok(()); // empty = unbound
            }
            normal_map.insert(key(name, what)?, action);
            Ok(())
        };
        bind(&n.move_left, "normal.move_left", Action::MoveLeft)?;
        bind(&n.move_right, "normal.move_right", Action::MoveRight)?;
        bind(&n.move_up, "normal.move_up", Action::MoveUp)?;
        bind(&n.move_down, "normal.move_down", Action::MoveDown)?;
        bind(&n.click_left, "normal.click_left", Action::ClickLeft)?;
        bind(&n.click_middle, "normal.click_middle", Action::ClickMiddle)?;
        bind(&n.click_right, "normal.click_right", Action::ClickRight)?;
        bind(&n.scroll_up, "normal.scroll_up", Action::ScrollUp)?;
        bind(&n.scroll_down, "normal.scroll_down", Action::ScrollDown)?;
        bind(&n.scroll_left, "normal.scroll_left", Action::ScrollLeft)?;
        bind(&n.scroll_right, "normal.scroll_right", Action::ScrollRight)?;
        bind(&n.hint, "normal.hint", Action::Hint)?;
        bind(&n.grid, "normal.grid", Action::Grid)?;
        bind(&n.tile, "normal.tile", Action::Tile)?;
        bind(&n.tile_columns, "normal.tile_columns", Action::TileColumns)?;
        bind(&n.focus_next, "normal.focus_next", Action::FocusNext)?;
        bind(&n.focus_prev, "normal.focus_prev", Action::FocusPrev)?;
        bind(
            &n.toggle_tiling,
            "normal.toggle_tiling",
            Action::ToggleTiling,
        )?;
        bind(&n.resize_grow, "normal.resize_grow", Action::ResizeGrow)?;
        bind(
            &n.resize_shrink,
            "normal.resize_shrink",
            Action::ResizeShrink,
        )?;
        bind(&n.swap_next, "normal.swap_next", Action::SwapNext)?;
        bind(&n.swap_prev, "normal.swap_prev", Action::SwapPrev)?;
        bind(
            &n.workspace_next,
            "normal.workspace_next",
            Action::WorkspaceNext,
        )?;
        bind(
            &n.workspace_prev,
            "normal.workspace_prev",
            Action::WorkspacePrev,
        )?;
        bind(
            &n.move_workspace_next,
            "normal.move_workspace_next",
            Action::MoveWorkspaceNext,
        )?;
        bind(
            &n.move_workspace_prev,
            "normal.move_workspace_prev",
            Action::MoveWorkspacePrev,
        )?;
        bind(&n.promote, "normal.promote", Action::Promote)?;

        let faster = if n.faster.trim().is_empty() {
            None
        } else {
            Some(key(&n.faster, "normal.faster")?)
        };

        let mut hint_chars: Vec<char> = Vec::new();
        for c in raw.hint.chars.chars() {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_alphabetic() && !hint_chars.contains(&c) {
                hint_chars.push(c);
            }
        }
        if hint_chars.len() < 2 {
            anyhow::bail!("hint.chars must contain at least 2 distinct letters");
        }

        let mut remaps = HashMap::new();
        for (from, to) in &raw.remap {
            remaps.insert(key(from, "remap source")?, key(to, "remap target")?);
        }

        Ok(Self {
            activation: key(&raw.general.activation, "general.activation")?,
            move_step: raw.general.move_step.max(1),
            fast_multiplier: raw.general.fast_multiplier.max(1),
            scroll_amount: raw.general.scroll_amount.max(1),
            altgr: raw.general.altgr,
            auto_tiling: raw.tiling.auto,
            gap: raw.tiling.gap.max(0),
            outer_gap: raw.tiling.outer_gap.max(0),
            number_workspaces: raw.tiling.number_keys,
            float_rules: raw
                .tiling
                .float
                .iter()
                .filter(|r| !r.is_empty())
                .cloned()
                .collect(),
            normal_map,
            faster,
            normal_exit: key(&n.exit, "normal.exit")?,
            hint_chars,
            hint_exit: key(&raw.hint.exit, "hint.exit")?,
            hint_backspace: key(&raw.hint.backspace, "hint.backspace")?,
            remaps,
        })
    }

    /// Load from a TOML file, falling back to defaults if it does not exist.
    pub fn load(path: &Path) -> Result<Self> {
        let raw = if path.exists() {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("reading config {}", path.display()))?;
            toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))?
        } else {
            tracing::info!("no config at {}, using defaults", path.display());
            RawConfig::default()
        };
        Self::resolve(raw)
    }

    /// Keys the hook must grab while idle: remap sources plus the activation key.
    pub fn idle_grab(&self) -> std::collections::HashSet<KeyCode> {
        let mut set: std::collections::HashSet<KeyCode> = self.remaps.keys().copied().collect();
        set.insert(self.activation);
        set
    }
}
