//! Remembers each project's layout (splits, open files, terminals) and which project
//! windows were open, so the app comes back the way you left it.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum LayoutState {
    /// Index into `Session::panes`.
    Pane(usize),
    Split { vertical: bool, ratio: f32, a: Box<LayoutState>, b: Box<LayoutState> },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum ItemState {
    File { path: PathBuf, row: usize, col: usize },
    Terminal { cwd: PathBuf },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct PaneState {
    pub items: Vec<ItemState>,
    /// Index into `items` of the one shown.
    pub active: Option<usize>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Session {
    pub layout: LayoutState,
    pub panes: Vec<PaneState>,
    pub focused: usize,
    pub side: String,
    pub side_visible: bool,
    pub side_w: f32,
    /// Folders added to the workspace.
    #[serde(default)]
    pub folders: Vec<PathBuf>,
    /// Where the window was: which display, its size, maximized or full screen.
    #[serde(default)]
    pub window: Option<WindowPlace>,
    /// What each repository's graph shows, when not Auto.
    #[serde(default)]
    pub graph: Vec<GraphScope>,
}

/// The branches a repository's graph shows: all of them, or a pick (both off: Auto).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct GraphScope {
    pub root: PathBuf,
    pub all: bool,
    pub only: Vec<String>,
}

/// A window's place on screen, to open it there again (like VS Code).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct WindowPlace {
    /// The display's UUID (stable across restarts and replugging), if known.
    pub display: Option<String>,
    /// "windowed", "maximized" or "fullscreen".
    pub state: String,
    /// Position on that display and size; for maximized/full screen, the size to go back to.
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl WindowPlace {
    pub fn new(bounds: gpui::WindowBounds, display: Option<String>) -> Self {
        let (state, b) = match bounds {
            gpui::WindowBounds::Windowed(b) => ("windowed", b),
            gpui::WindowBounds::Maximized(b) => ("maximized", b),
            gpui::WindowBounds::Fullscreen(b) => ("fullscreen", b),
        };
        Self { display, state: state.into(), x: b.origin.x / gpui::px(1.), y: b.origin.y / gpui::px(1.), w: b.size.width / gpui::px(1.), h: b.size.height / gpui::px(1.) }
    }

    pub fn bounds(&self) -> gpui::WindowBounds {
        let b = gpui::Bounds::new(gpui::point(gpui::px(self.x), gpui::px(self.y)), gpui::size(gpui::px(self.w), gpui::px(self.h)));
        match self.state.as_str() {
            "maximized" => gpui::WindowBounds::Maximized(b),
            "fullscreen" => gpui::WindowBounds::Fullscreen(b),
            _ => gpui::WindowBounds::Windowed(b),
        }
    }
}

fn config_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/kite"))
}

/// One file per project, named after its path.
fn session_path(root: &Path) -> Option<PathBuf> {
    let name: String = root
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    config_dir().map(|d| d.join("sessions").join(format!("{}.json", name.trim_matches('_'))))
}

pub fn load(root: &Path) -> Option<Session> {
    let text = std::fs::read_to_string(session_path(root)?).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn save(root: &Path, session: &Session) {
    let Some(path) = session_path(root) else { return };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    if let Ok(text) = serde_json::to_string_pretty(session) {
        std::fs::write(path, text).ok();
    }
}

/// Project windows that were open when the app last quit.
pub fn load_windows() -> Vec<PathBuf> {
    config_dir()
        .and_then(|d| std::fs::read_to_string(d.join("windows.json")).ok())
        .and_then(|t| serde_json::from_str::<Vec<PathBuf>>(&t).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p.is_dir())
        .collect()
}

pub fn save_windows(roots: &[PathBuf]) {
    let Some(dir) = config_dir() else { return };
    std::fs::create_dir_all(&dir).ok();
    if let Ok(text) = serde_json::to_string_pretty(roots) {
        std::fs::write(dir.join("windows.json"), text).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::{ItemState, LayoutState, PaneState, Session};

    #[test]
    fn round_trips_through_json() {
        let s = Session {
            layout: LayoutState::Split { vertical: true, ratio: 0.4, a: Box::new(LayoutState::Pane(0)), b: Box::new(LayoutState::Pane(1)) },
            panes: vec![
                PaneState { items: vec![ItemState::File { path: "/p/a.ts".into(), row: 3, col: 1 }], active: Some(0) },
                PaneState { items: vec![ItemState::Terminal { cwd: "/p".into() }], active: Some(0) },
            ],
            focused: 1,
            side: "git".into(),
            side_visible: true,
            side_w: 300.,
            folders: vec![],
            window: Some(super::WindowPlace { display: Some("ABC".into()), state: "fullscreen".into(), x: 10., y: 20., w: 1200., h: 800. }),
            graph: vec![super::GraphScope { root: "/p".into(), all: false, only: vec!["main".into(), "origin/java".into()] }],
        };
        let text = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Session>(&text).unwrap(), s);
    }

    #[test]
    fn window_place_keeps_state_and_size() {
        use gpui::{Bounds, WindowBounds, point, px, size};
        let b = Bounds::new(point(px(40.), px(60.)), size(px(1200.), px(800.)));
        for wb in [WindowBounds::Windowed(b), WindowBounds::Maximized(b), WindowBounds::Fullscreen(b)] {
            assert_eq!(super::WindowPlace::new(wb, None).bounds(), wb);
        }
        // Sessions saved before windows were remembered still load.
        let old = r#"{"layout":{"Pane":0},"panes":[],"focused":0,"side":"files","side_visible":true,"side_w":250.0}"#;
        assert_eq!(serde_json::from_str::<Session>(old).unwrap().window, None);
    }
}
