//! Normal-mode actions that keys can be bound to in config.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    ClickLeft,
    ClickMiddle,
    ClickRight,
    ScrollUp,
    ScrollDown,
    ScrollLeft,
    ScrollRight,
    /// Enter hint mode over UI-Automation elements.
    Hint,
    /// Enter hint mode over a screen grid.
    Grid,
    /// Tile the current monitor's windows (BSP layout).
    Tile,
    /// Tile the current monitor's windows (columns layout).
    TileColumns,
    /// Move keyboard focus to the next window on the monitor.
    FocusNext,
    /// Move keyboard focus to the previous window on the monitor.
    FocusPrev,
    /// Toggle live auto-tiling on/off.
    ToggleTiling,
    /// Grow the master (first) tiling area.
    ResizeGrow,
    /// Shrink the master (first) tiling area.
    ResizeShrink,
    /// Swap the focused window with the next in tiling order.
    SwapNext,
    /// Swap the focused window with the previous in tiling order.
    SwapPrev,
    /// Switch to the next / previous workspace.
    WorkspaceNext,
    WorkspacePrev,
    /// Move the focused window to the next / previous workspace.
    MoveWorkspaceNext,
    MoveWorkspacePrev,
    /// Make the focused window the master (first) tile.
    Promote,
}
