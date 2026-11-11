//! The Databases sidebar: saved connections (in their groups) and, as they're expanded,
//! what's in them — loaded when opened, a level at a time.

use crate::db::{self, Conn, Node, engine};
use crate::db_view::{DbRequest, Open};
use crate::request::Request;
use crate::theme;
use crate::ui::{self, px};
use gpui::prelude::*;
use gpui::{App, Context, EventEmitter, FocusHandle, Focusable, FontWeight, SharedString, Task, UniformListScrollHandle, Window, div, transparent_black, uniform_list};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::PathBuf;

enum Loaded {
    Loading,
    Ready(Vec<Node>),
    Failed(String),
}

#[derive(Clone)]
enum RowKind {
    Group(String),
    Conn(usize),
    Node(usize, Node),
    Info(String, bool),
}

#[derive(Clone)]
struct Row {
    depth: usize,
    kind: RowKind,
    key: String,
}

pub struct DbPanel {
    focus: FocusHandle,
    conns: Vec<Conn>,
    roots: Vec<PathBuf>,
    /// Expanded: connections and nodes, by key; closed groups by name.
    open: HashSet<String>,
    closed_groups: HashSet<String>,
    loaded: HashMap<String, Loaded>,
    rows: Vec<Row>,
    selected: Option<String>,
    scroll: UniformListScrollHandle,
    tasks: HashMap<String, Task<()>>,
    /// Connections being connected (until their first listing comes back).
    connecting: HashSet<String>,
}

/// A node's key: its connection and path.
fn key(conn: &str, path: &[String]) -> String {
    let mut k = conn.to_string();
    for p in path {
        k.push('\u{1f}');
        k.push_str(p);
    }
    k
}

impl EventEmitter<Request> for DbPanel {}

impl DbPanel {
    pub fn new(roots: Vec<PathBuf>, cx: &mut Context<Self>) -> Self {
        let mut p = DbPanel {
            focus: cx.focus_handle(),
            conns: Vec::new(),
            roots,
            open: HashSet::new(),
            closed_groups: HashSet::new(),
            loaded: HashMap::new(),
            rows: Vec::new(),
            selected: None,
            scroll: UniformListScrollHandle::new(),
            tasks: HashMap::new(),
            connecting: HashSet::new(),
        };
        p.reload(cx);
        p
    }

    pub fn focus_target(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }

    /// The workspace's folders (which workspace connections show).
    pub fn set_roots(&mut self, roots: Vec<PathBuf>, cx: &mut Context<Self>) {
        if roots != self.roots {
            self.roots = roots;
            self.reload(cx);
        }
    }

    /// Re-reads the saved connections (one was added, changed or deleted); `expand` opens one.
    pub fn reload_and_expand(&mut self, expand: Option<String>, cx: &mut Context<Self>) {
        let before: HashMap<String, Conn> = self.conns.iter().map(|c| (c.id.clone(), c.clone())).collect();
        self.reload(cx);
        // A changed connection's tree is loaded again (its server may be another one now).
        let changed: Vec<String> = self.conns.iter().filter(|c| before.get(&c.id).is_some_and(|b| b != *c)).map(|c| c.id.clone()).collect();
        for id in changed {
            self.forget_tree(&id);
        }
        if let Some(id) = expand {
            self.connect(&id, cx);
        }
        self.rebuild();
        cx.notify();
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let roots = self.roots.clone();
        let mut conns: Vec<Conn> = db::load().into_iter().filter(|c| c.visible_in(&roots)).collect();
        conns.sort_by_key(|c| (c.group.to_lowercase(), c.title().to_lowercase()));
        self.conns = conns;
        self.rebuild();
        cx.notify();
    }

    fn forget_tree(&mut self, id: &str) {
        let prefix = key(id, &[]);
        self.loaded.retain(|k, _| k != &prefix && !k.starts_with(&format!("{prefix}\u{1f}")));
    }

    /// The visible rows: groups, connections, and what's expanded under them.
    fn rebuild(&mut self) {
        let mut rows = Vec::new();
        let mut last_group: Vec<String> = Vec::new();
        for (ci, c) in self.conns.iter().enumerate() {
            let group: Vec<String> = if c.group.is_empty() { Vec::new() } else { c.group.split('/').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect() };
            // Folder rows for the parts of the group not shown yet.
            let common = group.iter().zip(&last_group).take_while(|(a, b)| a == b).count();
            let mut hidden = false;
            for depth in 0..group.len() {
                let name = group[..=depth].join("/");
                if depth >= common {
                    if !hidden {
                        rows.push(Row { depth, kind: RowKind::Group(name.clone()), key: format!("group:{name}") });
                    }
                }
                if self.closed_groups.contains(&name) {
                    hidden = true;
                }
            }
            last_group = group.clone();
            if hidden {
                continue;
            }
            let depth = group.len();
            let k = key(&c.id, &[]);
            rows.push(Row { depth, kind: RowKind::Conn(ci), key: k.clone() });
            // While connecting, the row itself says so.
            if self.open.contains(&k) && !self.connecting.contains(&c.id) {
                self.push_children(&mut rows, ci, &[], depth + 1);
            }
        }
        self.rows = rows;
    }

    fn push_children(&self, rows: &mut Vec<Row>, ci: usize, path: &[String], depth: usize) {
        let c = &self.conns[ci];
        let k = key(&c.id, path);
        match self.loaded.get(&k) {
            None | Some(Loaded::Loading) => rows.push(Row { depth, kind: RowKind::Info("Loading…".into(), false), key: format!("{k}\u{1e}info") }),
            Some(Loaded::Failed(e)) => rows.push(Row { depth, kind: RowKind::Info(e.clone(), true), key: format!("{k}\u{1e}info") }),
            Some(Loaded::Ready(nodes)) if nodes.is_empty() => rows.push(Row { depth, kind: RowKind::Info("Nothing here".into(), false), key: format!("{k}\u{1e}info") }),
            Some(Loaded::Ready(nodes)) => {
                for n in nodes {
                    let nk = key(&c.id, &n.path);
                    rows.push(Row { depth, kind: RowKind::Node(ci, n.clone()), key: nk.clone() });
                    if !n.kind.is_leaf() && self.open.contains(&nk) {
                        self.push_children(rows, ci, &n.path, depth + 1);
                    }
                }
            }
        }
    }

    fn load(&mut self, conn_id: &str, path: Vec<String>, cx: &mut Context<Self>) {
        let Some(conn) = self.conns.iter().find(|c| c.id == conn_id).cloned() else { return };
        let k = key(conn_id, &path);
        self.loaded.insert(k.clone(), Loaded::Loading);
        let work = engine::run(engine::children(conn, path));
        let kk = k.clone();
        self.tasks.insert(
            k,
            cx.spawn(async move |this, cx| {
                let r = work.await;
                this.update(cx, |p, cx| {
                    p.tasks.remove(&kk);
                    let root = !kk.contains('\u{1f}');
                    if root {
                        p.connecting.remove(&kk);
                    }
                    match r {
                        Ok(n) => {
                            p.loaded.insert(kk.clone(), Loaded::Ready(n));
                            if root {
                                // Connected: tabs waiting for it load now.
                                cx.emit(Request::Db(DbRequest::Connected(kk.clone())));
                            }
                        }
                        // The connection itself failed: closed again, and said in a popup.
                        Err(e) if root => {
                            p.disconnect(&kk, cx);
                            p.connect_failed(&kk, e, cx);
                        }
                        Err(e) => {
                            p.loaded.insert(kk.clone(), Loaded::Failed(e));
                        }
                    }
                    p.rebuild();
                    cx.notify();
                })
                .ok();
            }),
        );
        self.rebuild();
        cx.notify();
    }

    /// Connects (opens it and lists what's in it); a failure says so in a popup.
    pub fn connect(&mut self, conn_id: &str, cx: &mut Context<Self>) {
        engine::open(conn_id);
        self.connecting.insert(conn_id.to_string());
        self.forget_tree(conn_id);
        let k = key(conn_id, &[]);
        self.open.insert(k.clone());
        self.selected = Some(k);
        self.load(conn_id, Vec::new(), cx);
    }

    fn connect_failed(&mut self, conn_id: &str, error: String, cx: &mut Context<Self>) {
        let name = self.conns.iter().find(|c| c.id == conn_id).map(|c| c.title()).unwrap_or_default();
        let id = conn_id.to_string();
        let weak = cx.entity().downgrade();
        cx.emit(Request::Ask(crate::dialog::Ask {
            title: format!("Couldn't connect to {name}"),
            detail: error,
            // The main button first: Retry; then fixing what's wrong.
            buttons: vec!["Retry".into(), "Edit connection".into()],
            danger: false,
            cancel_default: false,
            cancel_label: Some("Close"),
            on_answer: std::rc::Rc::new(move |choice, _, cx: &mut App| {
                if let Some(p) = weak.upgrade() {
                    let id = id.clone();
                    p.update(cx, |p, cx| match choice {
                        0 => p.connect(&id, cx),
                        _ => cx.emit(Request::Db(DbRequest::Open(Open::Edit(id)))),
                    });
                }
            }),
        }));
    }

    fn toggle(&mut self, conn_id: &str, path: Vec<String>, cx: &mut Context<Self>) {
        // A connection not connected: connecting is what opening it does.
        if path.is_empty() && !engine::connected(conn_id) {
            return self.connect(conn_id, cx);
        }
        let k = key(conn_id, &path);
        if !self.open.remove(&k) {
            self.open.insert(k.clone());
            if !matches!(self.loaded.get(&k), Some(Loaded::Ready(_) | Loaded::Loading)) {
                self.load(conn_id, path, cx);
            }
        }
        self.rebuild();
        cx.notify();
    }

    fn refresh(&mut self, conn_id: &str, path: Vec<String>, cx: &mut Context<Self>) {
        if !engine::connected(conn_id) {
            return self.connect(conn_id, cx);
        }
        let k = key(conn_id, &path);
        // Everything under it is loaded again when opened.
        self.loaded.retain(|kk, _| kk != &k && !kk.starts_with(&format!("{k}\u{1f}")));
        self.open.insert(k);
        self.load(conn_id, path, cx);
    }

    /// Stops connecting: the answer, when it comes, is dropped.
    fn cancel_connect(&mut self, conn_id: &str, cx: &mut Context<Self>) {
        self.tasks.remove(&key(conn_id, &[]));
        self.disconnect(conn_id, cx);
    }

    pub fn disconnect(&mut self, conn_id: &str, cx: &mut Context<Self>) {
        self.connecting.remove(conn_id);
        engine::forget(conn_id);
        let k = key(conn_id, &[]);
        self.open.retain(|o| o != &k && !o.starts_with(&format!("{k}\u{1f}")));
        self.forget_tree(conn_id);
        self.rebuild();
        cx.notify();
    }

    fn delete(&mut self, ci: usize, cx: &mut Context<Self>) {
        let c = self.conns[ci].clone();
        let id = c.id.clone();
        let weak = cx.entity().downgrade();
        cx.emit(Request::Ask(crate::dialog::Ask {
            title: format!("Delete the connection “{}”?", c.title()),
            detail: "Its saved settings and password are removed. The database itself isn't touched.".into(),
            buttons: vec!["Delete".into()],
            danger: true,
            cancel_default: false,
            cancel_label: None,
            on_answer: std::rc::Rc::new(move |_, _, cx: &mut App| {
                db::delete(&id).ok();
                if let Some(p) = weak.upgrade() {
                    let id = id.clone();
                    p.update(cx, |p, cx| {
                        p.disconnect(&id, cx);
                        p.reload(cx);
                        cx.emit(Request::Db(DbRequest::Changed { expand: None }));
                    });
                }
            }),
        }));
    }

    fn render_row(&self, ix: usize, cx: &mut Context<Self>) -> gpui::AnyElement {
        let row = self.rows[ix].clone();
        let selected = self.selected.as_deref() == Some(row.key.as_str());
        let base = div()
            .id(("db-row", ix))
            .group("db-row")
            .relative()
            .flex()
            .items_center()
            .w_full()
            .h(theme::row_h())
            .pl(px(8. + row.depth as f32 * 12.))
            .pr(px(6.))
            .gap(px(5.))
            .cursor_pointer()
            .when(selected, |d| d.bg(theme::selected_focused()))
            .when(!selected, |d| d.hover(|s| s.bg(theme::hover())));
        let chevron = |open: bool| ui::small_icon(if open { "chevron-down" } else { "chevron-right" }, theme::fg());
        let hover_button = |id: String, icon: &'static str, tip: &'static str| {
            let sel = id.clone();
            div()
                .id(SharedString::from(id))
                .debug_selector(move || sel.clone())
                .flex()
                .items_center()
                .justify_center()
                .size(px(20.))
                .rounded(px(3.))
                .hover(|s| s.bg(theme::ca(0x5a5d5e50)))
                .tooltip(ui::tooltip(tip))
                .child(ui::small_icon(icon, theme::fg()))
        };
        // Buttons shown over the row's end while it's hovered: they take no room otherwise.
        let actions = || {
            div()
                .absolute()
                .right(px(4.))
                .top_0()
                .h_full()
                .flex()
                .items_center()
                .gap(px(1.))
                .pl(px(6.))
                .invisible()
                .group_hover("db-row", |s| s.visible())
                .bg(if selected { theme::selected_focused() } else { theme::hover() })
        };
        let name = |text: String, color: gpui::Hsla| div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_color(color).child(text);
        match row.kind {
            RowKind::Group(g) => {
                let open = !self.closed_groups.contains(&g);
                let label = g.rsplit('/').next().unwrap_or(&g).to_string();
                base.debug_selector(move || format!("db-group-{label}"))
                    .on_click(cx.listener(move |p, _, _, cx| {
                        if !p.closed_groups.remove(&g) {
                            p.closed_groups.insert(g.clone());
                        }
                        p.rebuild();
                        cx.notify();
                    }))
                    .child(chevron(open))
                    .child(ui::folder_icon("", open))
                    .child(name(self.rows[ix].key.trim_start_matches("group:").rsplit('/').next().unwrap_or("").to_string(), theme::fg()))
                    .into_any_element()
            }
            RowKind::Conn(ci) => {
                let c = &self.conns[ci];
                let id = c.id.clone();
                let open = self.open.contains(&row.key);
                let connecting = self.connecting.contains(&c.id);
                // Connected for real once its first listing came back.
                let live = engine::connected(&c.id) && !connecting;
                let (i1, i2, i3, i4, i5) = (id.clone(), id.clone(), id.clone(), id.clone(), id.clone());
                let title = c.title();
                let sel_name = format!("db-conn-{title}");
                let rk = row.key.clone();
                base.debug_selector(move || sel_name)
                    .on_click(cx.listener(move |p, _, w, cx| {
                        p.selected = Some(rk.clone());
                        w.focus(&p.focus, cx);
                        p.toggle(&i1, Vec::new(), cx);
                    }))
                    .child(chevron(open))
                    .child(crate::db_view::kind_badge(c.kind))
                    .child(name(title, theme::fg_bright()))
                    .when(c.project.is_some(), |d| d.child(div().text_size(px(10.)).text_color(theme::fg_muted()).child("workspace")))
                    .when(live, |d| d.child(ui::small_icon("dot", theme::git_added()).size(px(9.))))
                    // Connecting: says so, with Cancel (shown without hovering); nothing else yet.
                    .when(connecting, |d| {
                        let id = id.clone();
                        d.child(div().flex_none().text_size(px(11.)).text_color(theme::fg_muted()).child("Connecting…")).child(
                            div()
                                .id(SharedString::from(format!("db-cancel-{ci}")))
                                .debug_selector(move || format!("db-cancel-{ci}"))
                                .flex()
                                .flex_none()
                                .items_center()
                                .justify_center()
                                .size(px(20.))
                                .rounded(px(3.))
                                .cursor_pointer()
                                .hover(|s| s.bg(theme::ca(0x5a5d5e50)))
                                .tooltip(ui::tooltip("Cancel"))
                                .on_click(cx.listener(move |p, _, _, cx| {
                                    cx.stop_propagation();
                                    p.cancel_connect(&id, cx);
                                }))
                                .child(ui::small_icon("close", theme::fg_dim())),
                        )
                    })
                    // Not connected: Connect and Edit, always shown; the rest once it's connected.
                    .when(!live && !connecting, |d| {
                        let id = id.clone();
                        d.child(
                            div()
                                .id(SharedString::from(format!("db-connect-{ci}")))
                                .debug_selector(move || format!("db-connect-{ci}"))
                                .flex_none()
                                .px(px(6.))
                                .rounded(px(3.))
                                .text_size(px(11.))
                                .text_color(theme::accent())
                                .border_1()
                                .border_color(theme::border_input())
                                .cursor_pointer()
                                .hover(|s| s.bg(theme::hover()))
                                .on_click(cx.listener(move |p, _, _, cx| {
                                    cx.stop_propagation();
                                    p.connect(&id, cx);
                                }))
                                .child("Connect"),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("db-edit-{ci}")))
                                .debug_selector(move || format!("db-edit-{ci}"))
                                .flex()
                                .flex_none()
                                .items_center()
                                .justify_center()
                                .size(px(20.))
                                .rounded(px(3.))
                                .cursor_pointer()
                                .hover(|s| s.bg(theme::ca(0x5a5d5e50)))
                                .tooltip(ui::tooltip("Edit connection"))
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.stop_propagation();
                                    cx.emit(Request::Db(DbRequest::Open(Open::Edit(i4.clone()))));
                                }))
                                .child(ui::small_icon("settings", theme::fg_dim())),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("db-delete-{ci}")))
                                .debug_selector(move || format!("db-delete-{ci}"))
                                .flex()
                                .flex_none()
                                .items_center()
                                .justify_center()
                                .size(px(20.))
                                .rounded(px(3.))
                                .cursor_pointer()
                                .hover(|s| s.bg(theme::ca(0x5a5d5e50)))
                                .tooltip(ui::tooltip("Remove connection"))
                                .on_click(cx.listener(move |p, _, _, cx| {
                                    cx.stop_propagation();
                                    p.delete(ci, cx);
                                }))
                                .child(ui::small_icon("trash", theme::fg_dim())),
                        )
                    })
                    .when(live, |d| {
                        d.child(
                            actions()
                                .child(hover_button(format!("db-query-{ci}"), "terminal", "New query").on_click(cx.listener(move |_, _, _, cx| {
                                    cx.stop_propagation();
                                    cx.emit(Request::Db(DbRequest::Open(Open::Query { conn: i2.clone(), path: Vec::new() })));
                                })))
                                .child(hover_button(format!("db-refresh-{ci}"), "refresh", "Refresh").on_click(cx.listener(move |p, _, _, cx| {
                                    cx.stop_propagation();
                                    p.refresh(&i3, Vec::new(), cx);
                                })))
                                .child(hover_button(format!("db-edit-{ci}"), "settings", "Edit connection").on_click(cx.listener({
                                    let id = id.clone();
                                    move |_, _, _, cx| {
                                        cx.stop_propagation();
                                        cx.emit(Request::Db(DbRequest::Open(Open::Edit(id.clone()))));
                                    }
                                })))
                                .child(hover_button(format!("db-disconnect-{ci}"), "close", "Disconnect").on_click(cx.listener(move |p, _, _, cx| {
                                    cx.stop_propagation();
                                    p.disconnect(&i5, cx);
                                })))
                                .child(hover_button(format!("db-delete-{ci}"), "trash", "Remove connection").on_click(cx.listener(move |p, _, _, cx| {
                                    cx.stop_propagation();
                                    p.delete(ci, cx);
                                }))),
                        )
                    })
                    .into_any_element()
            }
            RowKind::Node(ci, n) => {
                let c = &self.conns[ci];
                let (id, path) = (c.id.clone(), n.path.clone());
                let leaf = n.kind.is_leaf();
                let open = self.open.contains(&row.key);
                let rk = row.key.clone();
                let label = n.label.clone();
                let (id2, path2) = (id.clone(), path.clone());
                let (id3, path3) = (id.clone(), path.clone());
                let queryable = matches!(n.kind, db::NodeKind::Database) || leaf;
                let mongo_db = c.kind == db::Kind::MongoDb && matches!(n.kind, db::NodeKind::Database) && path.len() == 1;
                base.debug_selector(move || format!("db-node-{label}"))
                    .on_click(cx.listener(move |p, _, w, cx| {
                        p.selected = Some(rk.clone());
                        w.focus(&p.focus, cx);
                        if leaf {
                            cx.emit(Request::Db(DbRequest::Open(Open::Data { conn: id.clone(), path: path.clone() })));
                        } else {
                            p.toggle(&id, path.clone(), cx);
                            // A MongoDB database also opens its page of collections, like Compass
                            // (when it's opened, not when it's folded away).
                            if mongo_db && !open {
                                cx.emit(Request::Db(DbRequest::Open(Open::Database { conn: id.clone(), db: path[0].clone() })));
                            }
                        }
                    }))
                    .child(if leaf { ui::small_icon("dot", transparent_black()) } else { chevron(open) })
                    .child(ui::small_icon(n.kind.icon(), if leaf { theme::accent() } else { theme::fg_dim() }))
                    .child(name(n.label.clone(), theme::fg()))
                    .when(!n.detail.is_empty(), |d| d.child(div().flex_none().text_size(px(11.)).text_color(theme::fg_muted()).child(n.detail.clone())))
                    .child(
                        actions()
                            .when(queryable, |d| {
                                d.child(hover_button(format!("db-node-query-{ix}"), "terminal", "New query").on_click(cx.listener(move |_, _, _, cx| {
                                    cx.stop_propagation();
                                    cx.emit(Request::Db(DbRequest::Open(Open::Query { conn: id2.clone(), path: path2.clone() })));
                                })))
                            })
                            .when(!leaf, |d| {
                                d.child(hover_button(format!("db-node-refresh-{ix}"), "refresh", "Refresh").on_click(cx.listener(move |p, _, _, cx| {
                                    cx.stop_propagation();
                                    p.refresh(&id3, path3.clone(), cx);
                                })))
                            }),
                    )
                    .into_any_element()
            }
            RowKind::Info(text, error) => base
                .cursor_default()
                .child(ui::small_icon(if error { "error" } else { "dot" }, if error { theme::git_deleted() } else { transparent_black() }))
                .child(div().flex_1().min_w_0().text_size(px(12.)).text_color(if error { theme::git_deleted() } else { theme::fg_muted() }).overflow_hidden().text_ellipsis().whitespace_nowrap().child(text))
                .into_any_element(),
        }
    }
}

impl Focusable for DbPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for DbPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let head_button = |id: &'static str, icon: &'static str, tip: &'static str| ui::icon_button(id, icon).debug_selector(move || id.to_string()).tooltip(ui::tooltip(tip));
        div()
            .key_context("DbPanel")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .h(px(35.))
                    .pl(px(20.))
                    .pr(px(6.))
                    .gap(px(1.))
                    .text_size(px(10.5))
                    .child(div().flex_1().child("DATABASES"))
                    .child(head_button("db-new", "plus", "New connection…").on_click(cx.listener(|_, _, _, cx| cx.emit(Request::Db(DbRequest::Open(Open::New))))))
                    .child(head_button("db-refresh-all", "refresh", "Refresh").on_click(cx.listener(|p, _, _, cx| {
                        p.loaded.clear();
                        let open: Vec<String> = p.open.iter().cloned().collect();
                        for k in open {
                            let mut parts = k.split('\u{1f}').map(str::to_string);
                            let id = parts.next().unwrap_or_default();
                            let path: Vec<String> = parts.collect();
                            p.load(&id, path, cx);
                        }
                        p.reload(cx);
                    })))
                    .child(head_button("db-collapse", "collapse-all", "Collapse all").on_click(cx.listener(|p, _, _, cx| {
                        p.open.clear();
                        p.rebuild();
                        cx.notify();
                    }))),
            )
            .child(if self.conns.is_empty() {
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .px(px(20.))
                    .pt(px(8.))
                    .text_size(px(12.))
                    .text_color(theme::fg_dim())
                    .child("No connections yet.")
                    .child(
                        div()
                            .id("db-empty-new")
                            .debug_selector(|| "db-empty-new".into())
                            .flex()
                            .justify_center()
                            .py(px(5.))
                            .rounded(px(4.))
                            .bg(theme::accent())
                            .text_color(theme::on(theme::accent()))
                            .font_weight(FontWeight::MEDIUM)
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::accent_hover()))
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(Request::Db(DbRequest::Open(Open::New)))))
                            .child("New connection"),
                    )
                    .child(div().text_size(px(11.)).text_color(theme::fg_muted()).child("PostgreSQL, SQL Server, MongoDB, Redis and RabbitMQ."))
                    .into_any_element()
            } else {
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .text_size(px(13.))
                    .child(uniform_list("db-tree", self.rows.len(), cx.processor(move |p, range: Range<usize>, _, cx| range.map(|ix| p.render_row(ix, cx)).collect::<Vec<_>>())).track_scroll(&self.scroll).flex_1())
                    .into_any_element()
            })
    }
}
