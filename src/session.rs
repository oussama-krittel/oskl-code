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
        };
        let text = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Session>(&text).unwrap(), s);
    }
}
