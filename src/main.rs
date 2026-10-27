mod assets;
mod buffer;
mod commit_view;
mod datafiles;
mod diagnostics;
mod diff_edit;
mod diff_view;
mod editor;
mod file_tree;
mod findbar;
mod git;
mod git_panel;
mod graph;
mod highlight;
mod icons;
mod keybindings;
mod keymap;
mod merge_view;
mod repo;
mod request;
mod search;
mod session;
mod settings;
mod terminal;
mod theme;
mod tools;
mod ui;
mod util;
mod workspace;

use gpui::prelude::*;
use gpui::*;
use futures::StreamExt;
use std::path::PathBuf;
use workspace::Workspace;

actions!(kite, [QuitApp, NewWindow, OpenFolderApp]);

pub fn open_window(root: PathBuf, cx: &mut App) {
    open_window_with(root, None, cx);
}

/// Opens a project window, optionally showing a file right away (`kite <folder> <file>` / `kite <file>`).
pub fn open_window_with(root: PathBuf, file: Option<PathBuf>, cx: &mut App) {
    let bounds = Bounds::centered(None, size(px(1400.), px(900.)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("OSKL Code".into()),
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
        // Bring back this project's splits, files and terminals.
        ws.update(cx, |ws, cx| ws.restore_session(window, cx));
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
    // With no arguments: from a terminal, the current folder; from the Dock/Finder, the
    // project windows that were open last time (or the start screen).
    let launch: Option<(PathBuf, Option<PathBuf>)> = match args.as_slice() {
        [a, ..] if a.is_file() => {
            let file = a.canonicalize().unwrap_or(a.clone());
            let dir = file.parent().map(PathBuf::from).unwrap_or_default();
            Some((git::repo_root(&dir).unwrap_or(dir), Some(file)))
        }
        [a, f, ..] if a.is_dir() => Some((a.clone(), Some(a.join(f)).filter(|p| p.is_file()).or(Some(f.clone()).filter(|p| p.is_file())))),
        [a] if a.is_dir() => Some((a.clone(), None)),
        _ => std::env::current_dir().ok().filter(|p| p != &PathBuf::from("/") && std::env::var_os("TERM").is_some()).map(|cwd| (cwd, None)),
    };
    let launch = launch.map(|(root, file)| (root.canonicalize().unwrap_or(root), file.map(|f| f.canonicalize().unwrap_or(f))));

    // Resolve the login-shell PATH in the background so tools are found when launched from Finder.
    std::thread::spawn(tools::init_shell_path);

    // Folders opened from macOS (Dock recent items, Finder "Open With", `open -a`).
    let (url_tx, mut url_rx) = futures::channel::mpsc::unbounded::<Vec<String>>();
    let opened_from_os = std::rc::Rc::new(std::cell::Cell::new(false));

    let app = Application::new().with_assets(assets::Assets);
    app.on_open_urls(move |urls| {
        let _ = url_tx.unbounded_send(urls);
    });
    app.on_reopen(|cx| {
        // Dock icon clicked with no windows open.
        if cx.windows().is_empty() {
            reopen_last_windows(cx);
        }
    });
    let opened = opened_from_os.clone();
    app.run(move |cx: &mut App| {
        editor::bind_keys();
        workspace::bind_keys();
        file_tree::bind_keys();
        git_panel::bind_keys();
        diff_view::bind_keys();
        commit_view::bind_keys();
        search::bind_keys();
        findbar::bind_keys();
        diff_edit::bind_keys();
        keybindings::apply(cx);
        cx.on_action(|_: &QuitApp, cx| cx.quit());
        cx.on_action(|_: &NewWindow, cx| open_window(workspace::no_project_dir(), cx));
        cx.on_action(|_: &OpenFolderApp, cx| {
            let rx = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: true, prompt: Some("Open".into()) });
            cx.spawn(async move |cx| {
                if let Ok(Ok(Some(paths))) = rx.await {
                    cx.update(|cx| {
                        for p in paths {
                            open_window(p.canonicalize().unwrap_or(p), cx);
                        }
                    })
                    .ok();
                }
            })
            .detach();
        });
        cx.set_menus(vec![Menu { name: "OSKL Code".into(), items: vec![MenuItem::action("New Window", NewWindow), MenuItem::action("Open Folder…", OpenFolderApp), MenuItem::separator(), MenuItem::action("Quit OSKL Code", QuitApp)] }]);
        cx.set_dock_menu(vec![MenuItem::action("New Window", NewWindow), MenuItem::action("Open Folder…", OpenFolderApp)]);
        // Remember which project windows were open, to reopen them next launch.
        cx.on_app_quit(|cx| {
            session::save_windows(&workspace::open_project_roots(cx));
            async {}
        })
        .detach();

        let opened = opened.clone();
        cx.spawn(async move |cx| {
            while let Some(urls) = url_rx.next().await {
                opened.set(true);
                cx.update(|cx| {
                    for url in urls {
                        if let Some(path) = url_to_path(&url) {
                            let (root, file) = if path.is_file() {
                                let dir = path.parent().map(PathBuf::from).unwrap_or_default();
                                (git::repo_root(&dir).unwrap_or(dir), Some(path))
                            } else {
                                (path, None)
                            };
                            open_window_with(root.canonicalize().unwrap_or(root), file, cx);
                        }
                    }
                    cx.activate(true);
                })
                .ok();
            }
        })
        .detach();

        match launch.clone() {
            Some((root, file)) => open_window_with(root, file, cx),
            None => {
                // Give a macOS "open this folder" request a moment to arrive first.
                let opened = opened_from_os.clone();
                cx.spawn(async move |cx| {
                    cx.background_executor().timer(std::time::Duration::from_millis(250)).await;
                    if !opened.get() {
                        cx.update(reopen_last_windows).ok();
                    }
                })
                .detach();
            }
        }
        cx.activate(true);
    });
}

/// Reopens the project windows from last time, or the start screen if there were none.
fn reopen_last_windows(cx: &mut App) {
    let roots = session::load_windows();
    if roots.is_empty() {
        open_window(workspace::no_project_dir(), cx);
    }
    for root in roots {
        open_window(root, cx);
    }
}

/// "file:///Users/me/My%20Project/" → /Users/me/My Project
fn url_to_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(b) = u8::from_str_radix(&rest[i + 1..i + 3], 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    let path = PathBuf::from(String::from_utf8(out).ok()?);
    path.exists().then_some(path)
}
