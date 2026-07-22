//! Pane-tree persistence — save / restore the workspace.
//!
//! Stored at `~/.terminite/state/last.json` (auto-saved on every
//! structural change). The shape mirrors the live `PaneNode` tree
//! but with only the bits worth replaying: tab kinds, titles, per-
//! pane chrome (bg / scale / color), shell cwds, and the most-
//! recent `publish_focus` path each pane saw (so an Editor pane
//! reopens the same file).
//!
//! Skipped on purpose: scrollback, selection, mouse position, undo
//! history. Those are session-local and would either be expensive
//! to serialize or surprising to restore.
//!
//! Bounded: file capped at [`MAX_LAYOUT_BYTES`]; pane count capped
//! at [`MAX_LAYOUT_PANES`]. Either limit breached → restore aborts
//! with a warning and terminite falls back to the default new
//! shell. We don't want a corrupt or hostile state file to flatten
//! the window into a 10k-pane grid.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;

/// Cap on the persisted layout file's on-disk size. ~256 KB fits
/// thousands of panes worth of metadata with room to spare.
pub const MAX_LAYOUT_BYTES: usize = 256 * 1024;

/// Cap on rebuilt panes. A corrupt file naming 100k panes would try
/// to spawn 100k shells — bound it. Real layouts are <50 panes.
pub const MAX_LAYOUT_PANES: usize = 256;

/// Cap on total tabs across all panes. Each tab restores a `LiveTerm`
/// + shell, so a 256 KB file packed with tiny tab entries could still
/// fork thousands of shells even under the pane cap. Bound the total.
pub const MAX_LAYOUT_TABS: usize = 512;

/// Bumped when the schema changes incompatibly. Older files load if
/// they're <= `LAYOUT_VERSION`; newer ones bail and default-spawn.
pub const LAYOUT_VERSION: u32 = 1;

/// Window size + on-screen position, in logical (size) and physical
/// (position) coordinates — enough to reopen where you left it.
/// Every field is clamped on load: a corrupt or hostile value must
/// not spawn a multi-thousand-pixel surface (that's the wgpu-OOM
/// class we already closed once elsewhere).
#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct WindowGeom {
    pub width: f32,
    pub height: f32,
    pub x: i32,
    pub y: i32,
}

/// Hard bounds on a restored window. Size is logical px; a real
/// display is nowhere near 10k logical px wide, so anything past that
/// is corruption, not a preference.
pub const MIN_WINDOW_DIM: f32 = 200.0;
pub const MAX_WINDOW_DIM: f32 = 10_000.0;
/// Position clamp — wide enough for any sane multi-monitor arrangement
/// (including a monitor to the left, hence negative), tight enough to
/// reject absurd offsets that would strand the window off every screen.
pub const MAX_WINDOW_POS: i32 = 32_000;

#[derive(Serialize, Deserialize, Debug)]
pub struct Layout {
    pub version: u32,
    /// Path through the tree to the originally-active pane —
    /// `vec![]` for the root leaf, otherwise a sequence of 0/1
    /// (first/second child). `None` if the active pane couldn't be
    /// resolved at save time; restore picks the first leaf.
    pub active_pane_path: Option<Vec<u8>>,
    /// Last window size/position. `None` on files written before this
    /// field existed, or when geometry couldn't be read.
    #[serde(default)]
    pub window: Option<WindowGeom>,
    /// Last live font size (the Cmd+/- / Cmd-scroll zoom). `None` →
    /// fall back to the configured `font_size`. Restores the zoom
    /// across restarts without rewriting the user's config file.
    #[serde(default)]
    pub font_size: Option<f32>,
    /// Last live tab-bar font size (the display-settings Tab slider). `None`
    /// → fall back to the configured `tab_font_size`. Same rationale as
    /// `font_size` — an independent chrome axis persisted across restarts.
    #[serde(default)]
    pub tab_font_size: Option<f32>,
    /// Last live tab-bar strip height (the display-settings Tab-height slider).
    /// `None` → fall back to the configured `tab_bar_height`.
    #[serde(default)]
    pub tab_bar_height: Option<f32>,
    pub root: LayoutNode,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "node", rename_all = "snake_case")]
pub enum LayoutNode {
    Pane(LayoutPane),
    Split {
        dir: LayoutSplitDir,
        ratio: f32,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}

#[derive(Serialize, Deserialize, Debug)]
pub struct LayoutPane {
    pub tabs: Vec<LayoutTab>,
    #[serde(default)]
    pub active_tab: usize,
    #[serde(default)]
    pub bg_idx: u8,
    #[serde(default = "default_scale")]
    pub font_scale: f32,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct LayoutTab {
    pub kind: LayoutTabKind,
    pub title: String,
    #[serde(default)]
    pub color_idx: u8,
    /// Working directory the shell should respawn in. Ignored for
    /// non-shell kinds. Falls back to terminite's cwd when missing
    /// or non-existent.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Most-recent `publish_focus` path the pane was driven to.
    /// Replayed as a synthetic focus event to the module on
    /// restore so Editor reopens its file, Preview re-renders, etc.
    #[serde(default)]
    pub focused_path: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LayoutTabKind {
    Shell,
    Welcome,
    Module { id: String },
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum LayoutSplitDir {
    Vertical,
    Horizontal,
}

fn default_scale() -> f32 {
    1.0
}

/// Resolve `~/.terminite/state/last.json`. Returns `None` if `$HOME`
/// isn't set (unusual; we'd skip persistence entirely in that case).
pub fn last_layout_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let mut p = PathBuf::from(home);
    p.push(".terminite");
    p.push("state");
    p.push("last.json");
    Some(p)
}

/// Atomic write: serialize to a temp file in the same directory,
/// fsync, rename into place. Avoids leaving a half-written JSON if
/// we crash mid-write — restore then reads the previous good state.
pub fn save(layout: &Layout) -> std::io::Result<()> {
    let Some(target) = last_layout_path() else {
        return Ok(()); // No HOME → silently skip persistence.
    };
    let dir = target.parent().expect("layout path has a parent");
    std::fs::create_dir_all(dir)?;
    let json = serde_json::to_vec_pretty(layout).map_err(std::io::Error::other)?;
    if json.len() > MAX_LAYOUT_BYTES {
        crate::logging::warn(&format!(
            "layout: refusing to save {} bytes > cap {}",
            json.len(),
            MAX_LAYOUT_BYTES
        ));
        return Ok(());
    }
    let tmp = dir.join("last.json.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&json)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &target)?;
    Ok(())
}

/// Read + parse the saved layout. Returns `Ok(None)` for "no file"
/// (fresh install / first launch); `Err` for read or parse failure.
pub fn load() -> std::io::Result<Option<Layout>> {
    let Some(path) = last_layout_path() else {
        return Ok(None);
    };
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if bytes.len() > MAX_LAYOUT_BYTES {
        crate::logging::warn(&format!(
            "layout: refusing to load {} bytes > cap {}",
            bytes.len(),
            MAX_LAYOUT_BYTES
        ));
        return Ok(None);
    }
    let layout: Layout = serde_json::from_slice(&bytes).map_err(std::io::Error::other)?;
    if layout.version > LAYOUT_VERSION {
        crate::logging::warn(&format!(
            "layout: file version {} > supported {} — ignoring",
            layout.version, LAYOUT_VERSION
        ));
        return Ok(None);
    }
    let pane_count = count_panes(&layout.root);
    if pane_count > MAX_LAYOUT_PANES {
        crate::logging::warn(&format!(
            "layout: refusing to restore {} panes > cap {}",
            pane_count, MAX_LAYOUT_PANES
        ));
        return Ok(None);
    }
    let tab_count = count_tabs(&layout.root);
    if tab_count > MAX_LAYOUT_TABS {
        crate::logging::warn(&format!(
            "layout: refusing to restore {} tabs > cap {}",
            tab_count, MAX_LAYOUT_TABS
        ));
        return Ok(None);
    }
    // A pane with zero tabs would panic the moment its active tab is
    // indexed. Reject the whole layout atomically and fall back to a
    // default spawn rather than restore a booby-trapped tree.
    if !every_pane_has_tabs(&layout.root) {
        crate::logging::warn("layout: a pane has no tabs — ignoring the saved layout");
        return Ok(None);
    }
    // Clamp floats (NaN / inf / out-of-range ratios + scales) and
    // active-tab indices in place — these are recoverable, no reason to
    // discard an otherwise-valid layout over them.
    let mut layout = layout;
    sanitize(&mut layout.root);
    // Clamp window geometry: drop it entirely if size is non-finite,
    // else bound size + position so a corrupt file can't spawn a giant
    // surface or strand the window off-screen.
    if let Some(w) = layout.window.as_mut() {
        if !w.width.is_finite() || !w.height.is_finite() {
            layout.window = None;
        } else {
            w.width = w.width.clamp(MIN_WINDOW_DIM, MAX_WINDOW_DIM);
            w.height = w.height.clamp(MIN_WINDOW_DIM, MAX_WINDOW_DIM);
            w.x = w.x.clamp(-MAX_WINDOW_POS, MAX_WINDOW_POS);
            w.y = w.y.clamp(-MAX_WINDOW_POS, MAX_WINDOW_POS);
        }
    }
    // Clamp the persisted zoom to the same bounds the config field uses,
    // dropping non-finite values.
    layout.font_size = layout.font_size.filter(|s| s.is_finite()).map(|s| {
        s.clamp(crate::config::MIN_FONT_SIZE, crate::config::MAX_FONT_SIZE)
    });
    layout.tab_font_size = layout.tab_font_size.filter(|s| s.is_finite()).map(|s| {
        s.clamp(crate::config::MIN_TAB_FONT_SIZE, crate::config::MAX_TAB_FONT_SIZE)
    });
    layout.tab_bar_height = layout.tab_bar_height.filter(|s| s.is_finite()).map(|s| {
        s.clamp(crate::config::MIN_TAB_BAR_HEIGHT, crate::config::MAX_TAB_BAR_HEIGHT)
    });
    Ok(Some(layout))
}

fn count_panes(node: &LayoutNode) -> usize {
    match node {
        LayoutNode::Pane(_) => 1,
        LayoutNode::Split { first, second, .. } => count_panes(first) + count_panes(second),
    }
}

fn count_tabs(node: &LayoutNode) -> usize {
    match node {
        LayoutNode::Pane(p) => p.tabs.len(),
        LayoutNode::Split { first, second, .. } => count_tabs(first) + count_tabs(second),
    }
}

fn every_pane_has_tabs(node: &LayoutNode) -> bool {
    match node {
        LayoutNode::Pane(p) => !p.tabs.is_empty(),
        LayoutNode::Split { first, second, .. } => {
            every_pane_has_tabs(first) && every_pane_has_tabs(second)
        }
    }
}

/// Clamp every float and index in the tree to a sane range so a hostile
/// or corrupt file can't inject NaN scales or out-of-bounds active tabs.
fn sanitize(node: &mut LayoutNode) {
    match node {
        LayoutNode::Pane(p) => {
            if !p.font_scale.is_finite() || p.font_scale <= 0.0 {
                p.font_scale = 1.0;
            }
            p.font_scale = p.font_scale.clamp(0.2, 5.0);
            if p.active_tab >= p.tabs.len() {
                p.active_tab = 0;
            }
        }
        LayoutNode::Split {
            ratio,
            first,
            second,
            ..
        } => {
            if !ratio.is_finite() {
                *ratio = 0.5;
            }
            *ratio = ratio.clamp(0.05, 0.95);
            sanitize(first);
            sanitize(second);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Layout {
        Layout {
            version: LAYOUT_VERSION,
            active_pane_path: Some(vec![1, 0]),
            window: Some(WindowGeom { width: 1200.0, height: 800.0, x: 100, y: 60 }),
            font_size: Some(15.0),
            tab_font_size: Some(18.0),
            tab_bar_height: Some(44.0),
            root: LayoutNode::Split {
                dir: LayoutSplitDir::Vertical,
                ratio: 0.6,
                first: Box::new(LayoutNode::Pane(LayoutPane {
                    tabs: vec![LayoutTab {
                        kind: LayoutTabKind::Shell,
                        title: "shell".to_string(),
                        color_idx: 0,
                        cwd: Some("/tmp".to_string()),
                        focused_path: None,
                    }],
                    active_tab: 0,
                    bg_idx: 0,
                    font_scale: 1.0,
                })),
                second: Box::new(LayoutNode::Split {
                    dir: LayoutSplitDir::Horizontal,
                    ratio: 0.5,
                    first: Box::new(LayoutNode::Pane(LayoutPane {
                        tabs: vec![LayoutTab {
                            kind: LayoutTabKind::Module { id: "nav-module".to_string() },
                            title: "Nav".to_string(),
                            color_idx: 3,
                            cwd: None,
                            focused_path: None,
                        }],
                        active_tab: 0,
                        bg_idx: 2,
                        font_scale: 0.8,
                    })),
                    second: Box::new(LayoutNode::Pane(LayoutPane {
                        tabs: vec![LayoutTab {
                            kind: LayoutTabKind::Module { id: "editor-module".to_string() },
                            title: "Edit".to_string(),
                            color_idx: 0,
                            cwd: None,
                            focused_path: Some("/tmp/foo.rs".to_string()),
                        }],
                        active_tab: 0,
                        bg_idx: 0,
                        font_scale: 1.0,
                    })),
                }),
            },
        }
    }

    #[test]
    fn roundtrip_via_json() {
        let l = sample();
        let json = serde_json::to_vec_pretty(&l).expect("serialize");
        let back: Layout = serde_json::from_slice(&json).expect("deserialize");
        assert_eq!(back.version, LAYOUT_VERSION);
        assert_eq!(back.active_pane_path, Some(vec![1, 0]));
        assert_eq!(count_panes(&back.root), 3);
    }

    #[test]
    fn rejects_oversize_panes() {
        // Build a degenerate-but-valid tree of 300 panes; load() would
        // refuse to read it past the cap.
        fn nest(n: usize) -> LayoutNode {
            if n == 1 {
                LayoutNode::Pane(LayoutPane {
                    tabs: vec![LayoutTab {
                        kind: LayoutTabKind::Shell,
                        title: String::new(),
                        color_idx: 0,
                        cwd: None,
                        focused_path: None,
                    }],
                    active_tab: 0,
                    bg_idx: 0,
                    font_scale: 1.0,
                })
            } else {
                LayoutNode::Split {
                    dir: LayoutSplitDir::Vertical,
                    ratio: 0.5,
                    first: Box::new(LayoutNode::Pane(LayoutPane {
                        tabs: vec![LayoutTab {
                            kind: LayoutTabKind::Shell,
                            title: String::new(),
                            color_idx: 0,
                            cwd: None,
                            focused_path: None,
                        }],
                        active_tab: 0,
                        bg_idx: 0,
                        font_scale: 1.0,
                    })),
                    second: Box::new(nest(n - 1)),
                }
            }
        }
        let big = nest(300);
        assert!(count_panes(&big) > MAX_LAYOUT_PANES);
    }
}
