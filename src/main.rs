mod assets;
mod buffer;
mod commit_view;
mod datafiles;
mod diagnostics;
mod diff_edit;
mod diff_view;
mod editor;
mod file_tree;
mod git;
mod git_panel;
mod graph;
mod highlight;
mod icons;
mod keymap;
mod repo;
mod request;
mod search;
mod settings;
mod terminal;
mod theme;
mod tools;
mod ui;
mod util;
mod workspace;

use gpui::prelude::*;
use gpui::*;
use std::path::PathBuf;
use workspace::Workspace;

actions!(kite, [QuitApp]);

pub fn open_window(root: PathBuf, cx: &mut App) {
    open_window_with(root, None, cx);
}

/// Opens a project window, optionally showing a file right away (`kite <folder> <file>` / `kite <file>`).
pub fn open_window_with(root: PathBuf, file: Option<PathBuf>, cx: &mut App) {
    let bounds = Bounds::centered(None, size(px(1400.), px(900.)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("kite".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(12.), px(11.))),
        }),
        window_min_size: Some(size(px(600.), px(400.))),
        focus: true,
        show: true,
        ..Default::default()
    };
    cx.open_window(options, |window, cx| {
        let ws = cx.new(|cx| Workspace::new(root, window, cx));
        if let Some(file) = file {
            ws.update(cx, |ws, cx| ws.open_file(file, None, false, window, cx));
        }
        ws
    })
        .expect("failed to open window");
}

fn main() {
    let args: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    // `kite <file>` opens the file's repo (or folder); `kite <folder> [file]` opens the folder.
    let (root, file) = match args.as_slice() {
        [a, ..] if a.is_file() => {
            let file = a.canonicalize().unwrap_or(a.clone());
            let dir = file.parent().map(PathBuf::from).unwrap_or_default();
            (git::repo_root(&dir).unwrap_or(dir), Some(file))
        }
        [a, f, ..] if a.is_dir() => (a.clone(), Some(a.join(f)).filter(|p| p.is_file()).or(Some(f.clone()).filter(|p| p.is_file()))),
        [a] if a.is_dir() => (a.clone(), None),
        _ => {
            let cwd = std::env::current_dir().ok().filter(|p| p != &PathBuf::from("/"));
            (cwd.or_else(|| std::env::var("HOME").ok().map(PathBuf::from)).unwrap_or_else(|| PathBuf::from(".")), None)
        }
    };
    let root = root.canonicalize().unwrap_or(root);
    let file = file.map(|f| f.canonicalize().unwrap_or(f));

    // Resolve the login-shell PATH in the background so tools are found when launched from Finder.
    std::thread::spawn(tools::init_shell_path);

    Application::new().with_assets(assets::Assets).run(move |cx: &mut App| {
        editor::bind_keys(cx);
        workspace::bind_keys(cx);
        file_tree::bind_keys(cx);
        git_panel::bind_keys(cx);
        diff_view::bind_keys(cx);
        commit_view::bind_keys(cx);
        search::bind_keys(cx);
        diff_edit::bind_keys(cx);
        cx.on_action(|_: &QuitApp, cx| cx.quit());
        cx.set_menus(vec![Menu { name: "kite".into(), items: vec![MenuItem::action("Quit kite", QuitApp)] }]);
        open_window_with(root.clone(), file.clone(), cx);
        cx.activate(true);
    });
}
