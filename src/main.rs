mod assets;
mod buffer;
mod commit_view;
mod diagnostics;
mod diff_edit;
mod diff_view;
mod editor;
mod file_tree;
mod git;
mod git_panel;
mod graph;
mod highlight;
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
    cx.open_window(options, |window, cx| cx.new(|cx| Workspace::new(root, window, cx)))
        .expect("failed to open window");
}

fn main() {
    let root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .filter(|p| p.is_dir() && p != &PathBuf::from("/"))
        .or_else(|| std::env::var("HOME").ok().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));
    let root = root.canonicalize().unwrap_or(root);

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
        open_window(root.clone(), cx);
        cx.activate(true);
    });
}
