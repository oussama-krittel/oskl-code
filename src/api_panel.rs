//! The API sidebar: collections (folders of saved requests), history, environments.

use crate::api::{self, Collection, Environment, Folder};
use crate::api_view::{ApiOpen, ApiRequest};
use crate::request::Request;
use crate::theme;
use crate::ui::{self, px};
use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, MouseButton, MouseDownEvent, Pixels, Point, SharedString, UniformListScrollHandle, Window, anchored, deferred, div, uniform_list};
use std::collections::HashSet;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Collections,
    History,
    Envs,
}

/// What the name box in the tree names.
#[derive(Clone, PartialEq)]
enum NameFor {
    NewCollection,
    /// A new folder in this collection, inside `parent` (a folder, or the collection).
    NewFolder { ci: usize, parent: String },
    Rename { ci: usize, what: Target },
}

#[derive(Clone, PartialEq)]
enum Target {
    Coll,
    Folder(String),
    Req(String),
}

struct Naming {
    what: NameFor,
    input: Entity<crate::editor::Editor>,
    _subs: Vec<gpui::Subscription>,
}

#[derive(Clone)]
enum RowKind {
    /// The name box of something new.
    Naming,
    Coll(usize),
    Folder(usize, String),
    Req(usize, api::Item),
    Visit(usize),
    Env(usize),
    Info(String),
}

#[derive(Clone)]
struct Row {
    depth: usize,
    kind: RowKind,
    key: String,
}

type Act = Rc<dyn Fn(&mut ApiPanel, &mut Window, &mut Context<ApiPanel>)>;

pub struct ApiPanel {
    focus: FocusHandle,
    roots: Vec<PathBuf>,
    pub tab: Tab,
    colls: Vec<Collection>,
    history: Vec<api::Visit>,
    envs: Vec<Environment>,
    active_env: Option<String>,
    /// Open collections and folders, by id.
    open: HashSet<String>,
    rows: Vec<Row>,
    selected: Option<String>,
    scroll: UniformListScrollHandle,
    filter: Entity<crate::editor::Editor>,
    menu: Option<(Point<Pixels>, Vec<(String, Act)>)>,
    status: Option<String>,
    /// Naming a new collection or folder, or renaming one (a box in the tree).
    naming: Option<Naming>,
}

/// A request's entry, dragged to another folder.
#[derive(Clone)]
pub struct DraggedRequest {
    coll: String,
    id: String,
    label: String,
}

impl EventEmitter<Request> for ApiPanel {}

fn open_req(o: ApiOpen) -> Request {
    Request::Api(ApiRequest::Open(o))
}

impl ApiPanel {
    pub fn new(roots: Vec<PathBuf>, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| {
            let mut e = crate::editor::Editor::new(crate::editor::EditorMode::SingleLine, cx);
            e.placeholder = "Filter".into();
            e
        });
        cx.subscribe(&filter, |p: &mut ApiPanel, _, ev: &crate::editor::EditorEvent, cx| {
            if matches!(ev, crate::editor::EditorEvent::Changed) {
                p.rebuild(cx);
                cx.notify();
            }
        })
        .detach();
        let mut p = ApiPanel {
            focus: cx.focus_handle(),
            roots,
            tab: Tab::Collections,
            colls: Vec::new(),
            history: Vec::new(),
            envs: Vec::new(),
            active_env: None,
            open: HashSet::new(),
            rows: Vec::new(),
            selected: None,
            scroll: UniformListScrollHandle::new(),
            filter,
            menu: None,
            status: None,
            naming: None,
        };
        p.reload(cx);
        p
    }

    pub fn focus(&self, window: &mut Window) {
        window.focus(&self.focus);
    }

    pub fn set_roots(&mut self, roots: Vec<PathBuf>, cx: &mut Context<Self>) {
        if roots != self.roots {
            self.roots = roots;
            self.reload(cx);
        }
    }

    fn project(&self) -> Option<PathBuf> {
        self.roots.first().cloned()
    }

    /// Reads collections, environments and (when shown) history again.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.colls = api::collections(&self.roots);
        self.envs = api::environments(&self.roots);
        self.active_env = api::active_env(self.project().as_deref());
        // History is read only while it's shown (it can be a few hundred entries).
        self.history = if self.tab == Tab::History { api::history() } else { Vec::new() };
        self.rebuild(cx);
        cx.notify();
    }

    fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tab = tab;
        self.reload(cx);
    }

    fn rebuild(&mut self, cx: &App) {
        let q = self.filter.read(cx).text().to_lowercase();
        let mut rows = Vec::new();
        match self.tab {
            Tab::Collections => {
                fn matches(f: &Folder, q: &str) -> bool {
                    f.name.to_lowercase().contains(q) || f.requests.iter().any(|r| r.name.to_lowercase().contains(q) || r.method.to_lowercase() == q) || f.folders.iter().any(|s| matches(s, q))
                }
                // `np`: the folder a new folder is being named in (its box goes first inside it).
                fn walk(ci: usize, f: &Folder, depth: usize, open: &HashSet<String>, q: &str, np: Option<&str>, rows: &mut Vec<Row>) {
                    for sub in &f.folders {
                        if !q.is_empty() && !matches(sub, q) {
                            continue;
                        }
                        rows.push(Row { depth, kind: RowKind::Folder(ci, sub.id.clone()), key: sub.id.clone() });
                        if np == Some(sub.id.as_str()) {
                            rows.push(Row { depth: depth + 1, kind: RowKind::Naming, key: "naming".into() });
                        }
                        if open.contains(&sub.id) || !q.is_empty() {
                            walk(ci, sub, depth + 1, open, q, np, rows);
                        }
                    }
                    for r in &f.requests {
                        if !q.is_empty() && !(r.name.to_lowercase().contains(q) || r.method.to_lowercase() == q) && !f.name.to_lowercase().contains(q) {
                            continue;
                        }
                        rows.push(Row { depth, kind: RowKind::Req(ci, r.clone()), key: r.id.clone() });
                    }
                }
                if self.naming.as_ref().is_some_and(|n| n.what == NameFor::NewCollection) {
                    rows.push(Row { depth: 0, kind: RowKind::Naming, key: "naming".into() });
                }
                let new_in: Option<(usize, String)> = match self.naming.as_ref().map(|n| &n.what) {
                    Some(NameFor::NewFolder { ci, parent }) => Some((*ci, parent.clone())),
                    _ => None,
                };
                for (ci, c) in self.colls.iter().enumerate() {
                    if !q.is_empty() && !matches(&c.root, &q) {
                        continue;
                    }
                    rows.push(Row { depth: 0, kind: RowKind::Coll(ci), key: c.id.clone() });
                    let np = new_in.as_ref().filter(|(i, _)| *i == ci).map(|(_, p)| p.as_str());
                    if np == Some(c.id.as_str()) {
                        rows.push(Row { depth: 1, kind: RowKind::Naming, key: "naming".into() });
                    }
                    if self.open.contains(&c.id) || !q.is_empty() {
                        walk(ci, &c.root, 1, &self.open, &q, np, &mut rows);
                        if c.root.count() == 0 && q.is_empty() && np.is_none() {
                            rows.push(Row { depth: 1, kind: RowKind::Info("No requests yet".into()), key: format!("empty:{}", c.id) });
                        }
                    }
                }
            }
            Tab::History => {
                for (i, v) in self.history.iter().enumerate() {
                    if q.is_empty() || v.request.url.to_lowercase().contains(&q) || v.request.method.to_lowercase() == q {
                        rows.push(Row { depth: 0, kind: RowKind::Visit(i), key: v.id.clone() });
                    }
                }
            }
            Tab::Envs => {
                for (i, e) in self.envs.iter().enumerate() {
                    if q.is_empty() || e.name.to_lowercase().contains(&q) {
                        rows.push(Row { depth: 0, kind: RowKind::Env(i), key: e.id.clone() });
                    }
                }
            }
        }
        self.rows = rows;
    }

    /// A new collection: its name is typed in the tree (Enter keeps it, Esc drops it).
    fn new_collection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.tab = Tab::Collections;
        self.start_naming(NameFor::NewCollection, "", window, cx);
    }

    fn new_folder(&mut self, ci: usize, parent: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.open.insert(parent.to_string());
        self.start_naming(NameFor::NewFolder { ci, parent: parent.to_string() }, "", window, cx);
    }

    fn rename(&mut self, ci: usize, what: Target, window: &mut Window, cx: &mut Context<Self>) {
        let Some(c) = self.colls.get(ci) else { return };
        let now = match &what {
            Target::Coll => c.name().to_string(),
            Target::Folder(f) => c.root.find(f).map(|f| f.name.clone()).unwrap_or_default(),
            Target::Req(r) => find_item(&c.root, r).map(|i| i.name.clone()).unwrap_or_default(),
        };
        self.start_naming(NameFor::Rename { ci, what }, &now, window, cx);
    }

    fn start_naming(&mut self, what: NameFor, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| {
            let mut e = crate::editor::Editor::new(crate::editor::EditorMode::SingleLine, cx);
            e.placeholder = match &what {
                NameFor::NewCollection => "Collection name",
                NameFor::NewFolder { .. } => "Folder name",
                NameFor::Rename { .. } => "Name",
            }
            .into();
            e.set_text(text, cx);
            e.select_range(0..text.chars().count(), cx);
            e
        });
        let subs = vec![
            cx.subscribe_in(&input, window, |p: &mut ApiPanel, _, ev: &crate::editor::EditorEvent, window, cx| match ev {
                crate::editor::EditorEvent::Submit => p.commit_naming(window, cx),
                crate::editor::EditorEvent::Cancel => p.cancel_naming(window, cx),
                _ => {}
            }),
            // Clicking elsewhere keeps what was typed, like the file tree.
            cx.on_blur(&input.focus_handle(cx), window, |p, window, cx| {
                if p.naming.is_some() {
                    p.commit_naming(window, cx);
                }
            }),
        ];
        self.naming = Some(Naming { what, input: input.clone(), _subs: subs });
        self.rebuild(cx);
        if let Some(ix) = self.rows.iter().position(|r| matches!(r.kind, RowKind::Naming) || self.renaming(&r.key)) {
            self.scroll.scroll_to_item(ix, gpui::ScrollStrategy::Center);
        }
        input.read(cx).focus(window);
        // Again once it's on screen: focus given to something not drawn yet can be lost.
        let again = input.clone();
        cx.on_next_frame(window, move |p, window, cx| {
            if p.naming.as_ref().is_some_and(|n| n.input == again) {
                again.read(cx).focus(window);
            }
        });
        cx.notify();
    }

    /// Whether the row with this key is being renamed.
    fn renaming(&self, key: &str) -> bool {
        match self.naming.as_ref().map(|n| &n.what) {
            Some(NameFor::Rename { ci, what }) => match what {
                Target::Coll => self.colls.get(*ci).is_some_and(|c| c.id == key),
                Target::Folder(f) | Target::Req(f) => f == key,
            },
            _ => false,
        }
    }

    fn cancel_naming(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.naming = None;
        self.rebuild(cx);
        window.focus(&self.focus);
        cx.notify();
    }

    fn commit_naming(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(n) = self.naming.take() else { return };
        let name = n.input.read(cx).text().trim().to_string();
        window.focus(&self.focus);
        if name.is_empty() {
            self.rebuild(cx);
            return cx.notify();
        }
        let result = match n.what {
            NameFor::NewCollection => {
                let c = Collection::new(&name, self.project());
                self.open.insert(c.id.clone());
                self.selected = Some(c.id.clone());
                api::save_collection(&c)
            }
            NameFor::NewFolder { ci, parent } => match self.colls.get_mut(ci) {
                Some(c) => {
                    let f = Folder { name, auth: api::Auth::Inherit, ..Default::default() };
                    self.selected = Some(f.id.clone());
                    c.root.folder_or_self(Some(&parent)).folders.push(f);
                    api::save_collection(c)
                }
                None => Ok(()),
            },
            NameFor::Rename { ci, what } => match self.colls.get_mut(ci) {
                Some(c) => match what {
                    Target::Coll => {
                        c.root.name = name;
                        api::save_collection(c)
                    }
                    Target::Folder(f) => {
                        if let Some(f) = c.root.find_mut(&f) {
                            f.name = name;
                        }
                        api::save_collection(c)
                    }
                    Target::Req(id) => match api::load_request(c, &id) {
                        Some(mut r) => {
                            r.name = name;
                            api::save_request(c, &r, None)
                        }
                        None => Ok(()),
                    },
                },
                None => Ok(()),
            },
        };
        if let Err(e) = result {
            self.status = Some(e);
        }
        self.reload(cx);
        cx.emit(Request::Api(ApiRequest::Changed));
    }

    fn new_request(&mut self, coll: Option<String>, folder: Option<String>, cx: &mut Context<Self>) {
        if let Some(f) = &folder {
            self.open.insert(f.clone());
        }
        if let Some(c) = &coll {
            self.open.insert(c.clone());
        }
        cx.emit(open_req(ApiOpen::New { coll, folder, req: None }));
    }

    fn ask(&mut self, title: String, detail: &str, button: &str, then: impl Fn(&mut ApiPanel, &mut Context<ApiPanel>) + 'static, cx: &mut Context<Self>) {
        let weak = cx.entity().downgrade();
        let then = Rc::new(then);
        cx.emit(Request::Ask(crate::dialog::Ask {
            title,
            detail: detail.into(),
            buttons: vec![button.into()],
            danger: true,
            cancel_default: false,
            cancel_label: None,
            on_answer: Rc::new(move |_, _, cx: &mut App| {
                if let Some(p) = weak.upgrade() {
                    let then = then.clone();
                    p.update(cx, |p, cx| then(p, cx));
                }
            }),
        }));
    }

    fn delete_collection(&mut self, ci: usize, cx: &mut Context<Self>) {
        let Some(c) = self.colls.get(ci).cloned() else { return };
        self.ask(
            format!("Delete the collection “{}”?", c.name()),
            &format!("Its {} requests are deleted too.", c.root.count()),
            "Delete",
            move |p, cx| {
                api::delete_collection(&c).ok();
                p.reload(cx);
                cx.emit(Request::Api(ApiRequest::Changed));
            },
            cx,
        );
    }

    fn delete_folder(&mut self, ci: usize, fid: String, cx: &mut Context<Self>) {
        let Some(c) = self.colls.get(ci).cloned() else { return };
        let Some(f) = c.root.find(&fid).cloned() else { return };
        self.ask(
            format!("Delete the folder “{}”?", f.name),
            &format!("Its {} requests are deleted too.", f.count()),
            "Delete",
            move |p, cx| {
                let mut c = c.clone();
                api::delete_folder(&mut c, &fid).ok();
                p.reload(cx);
                cx.emit(Request::Api(ApiRequest::Changed));
            },
            cx,
        );
    }

    fn delete_request(&mut self, ci: usize, item: api::Item, cx: &mut Context<Self>) {
        let Some(c) = self.colls.get(ci).cloned() else { return };
        self.ask(
            format!("Delete the request “{}”?", item.name),
            "",
            "Delete",
            move |p, cx| {
                let mut c = c.clone();
                api::delete_request(&mut c, &item.id).ok();
                p.reload(cx);
                cx.emit(Request::Api(ApiRequest::Changed));
            },
            cx,
        );
    }

    fn duplicate(&mut self, ci: usize, id: &str, cx: &mut Context<Self>) {
        if let Some(c) = self.colls.get_mut(ci)
            && let Ok(r) = api::duplicate_request(c, id)
        {
            let cid = c.id.clone();
            self.reload(cx);
            cx.emit(open_req(ApiOpen::Request { coll: cid, id: r.id }));
        }
    }

    fn move_to(&mut self, coll: &str, id: &str, folder: &str, cx: &mut Context<Self>) {
        let Some(c) = self.colls.iter_mut().find(|c| c.id == coll) else { return };
        if c.root.find(folder).is_some_and(|f| f.requests.iter().any(|r| r.id == id)) {
            return;
        }
        api::move_request(c, id, folder).ok();
        self.open.insert(folder.to_string());
        self.reload(cx);
    }

    fn export(&mut self, ci: usize, openapi: bool, cx: &mut Context<Self>) {
        let Some(c) = self.colls.get(ci).cloned() else { return };
        let name = format!("{}.{}", c.name().replace(['/', ':'], "_"), if openapi { "openapi.json" } else { "postman_collection.json" });
        let dir = std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Downloads")).unwrap_or_default();
        let ask = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn(async move |this, cx| {
            let Some(target) = ask.await.ok().and_then(|r| r.ok()).flatten() else { return };
            let v = if openapi { api::import::to_openapi(&c) } else { api::import::to_postman(&c) };
            let r = std::fs::write(&target, serde_json::to_vec_pretty(&v).unwrap_or_default());
            this.update(cx, |p, cx| {
                p.status = Some(match r {
                    Ok(()) => format!("Exported to {}", target.display()),
                    Err(e) => e.to_string(),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Import: Postman, Insomnia, OpenAPI (JSON or YAML), a Bruno folder, a .env file.
    fn import(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions { files: true, directories: true, multiple: true, prompt: Some("Import".into()) });
        let project = self.project();
        cx.spawn(async move |this, cx| {
            let Some(paths) = rx.await.ok().and_then(|r| r.ok()).flatten() else { return };
            let mut said = Vec::new();
            for p in paths {
                let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let result: Result<String, String> = (|| {
                    if p.is_dir() {
                        let imp = api::import::bruno(&p).map_err(|e| e.to_string())?;
                        let n = imp.requests.len();
                        let c = imp.save(project.clone())?;
                        return Ok(format!("{}: {n} requests", c.name()));
                    }
                    if name.starts_with(".env") || name.ends_with(".env") {
                        let mut e = api::import::dotenv_environment(&p);
                        e.project = project.clone();
                        api::save_environment(&e)?;
                        return Ok(format!("{}: an environment", e.name));
                    }
                    let text = std::fs::read_to_string(&p).map_err(|e| e.to_string())?;
                    // A Postman environment file.
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text)
                        && v.get("values").is_some()
                        && v.get("item").is_none()
                        && let Some(mut e) = api::import::postman_environment(&v)
                    {
                        e.project = project.clone();
                        api::save_environment(&e)?;
                        return Ok(format!("{}: an environment", e.name));
                    }
                    let imp = api::import::import_text(&text, name.split('.').next().unwrap_or("Imported")).map_err(|e| e.to_string())?;
                    let (n, envs) = (imp.requests.len(), imp.environments.len());
                    let c = imp.save(project.clone())?;
                    Ok(format!("{}: {n} requests{}", c.name(), if envs > 0 { format!(", {envs} environments") } else { String::new() }))
                })();
                said.push(match result {
                    Ok(s) => format!("Imported {s}"),
                    Err(e) => format!("{name}: {e}"),
                });
            }
            this.update(cx, |p, cx| {
                p.status = Some(said.join("\n"));
                p.tab = Tab::Collections;
                p.reload(cx);
                cx.emit(Request::Api(ApiRequest::Changed));
            })
            .ok();
        })
        .detach();
    }

    fn new_env(&mut self, cx: &mut Context<Self>) {
        let e = Environment { id: api::new_id(), name: format!("Environment {}", self.envs.len()), project: self.project(), ..Default::default() };
        api::save_environment(&e).ok();
        self.reload(cx);
        cx.emit(open_req(ApiOpen::Env(e.id)));
    }

    fn open_menu(&mut self, at: Point<Pixels>, items: Vec<(String, Act)>, cx: &mut Context<Self>) {
        self.menu = Some((at, items));
        cx.notify();
    }

    fn coll_menu(&self, ci: usize, fid: String) -> Vec<(String, Act)> {
        let Some(c) = self.colls.get(ci) else { return Vec::new() };
        let cid = c.id.clone();
        let is_root = fid == c.root.id;
        let mut items: Vec<(String, Act)> = Vec::new();
        let (c1, f1) = (cid.clone(), fid.clone());
        items.push(("New request".into(), Rc::new(move |p, _, cx| p.new_request(Some(c1.clone()), Some(f1.clone()), cx))));
        let f2 = fid.clone();
        items.push(("New folder".into(), Rc::new(move |p, w, cx| p.new_folder(ci, &f2, w, cx))));
        let (c3, f3) = (cid.clone(), fid.clone());
        items.push(("Run…".into(), Rc::new(move |_, _, cx| cx.emit(open_req(ApiOpen::Runner { coll: c3.clone(), folder: f3.clone() })))));
        let (c4, f4) = (cid.clone(), fid.clone());
        items.push(("Settings…".into(), Rc::new(move |_, _, cx| cx.emit(open_req(ApiOpen::Folder { coll: c4.clone(), folder: f4.clone() })))));
        let f6 = fid.clone();
        items.push(("Rename".into(), Rc::new(move |p, w, cx| p.rename(ci, if is_root { Target::Coll } else { Target::Folder(f6.clone()) }, w, cx))));
        if is_root {
            items.push(("Export as Postman…".into(), Rc::new(move |p, _, cx| p.export(ci, false, cx))));
            items.push(("Export as OpenAPI…".into(), Rc::new(move |p, _, cx| p.export(ci, true, cx))));
            items.push(("Delete collection".into(), Rc::new(move |p, _, cx| p.delete_collection(ci, cx))));
        } else {
            let f5 = fid.clone();
            items.push(("Delete folder".into(), Rc::new(move |p, _, cx| p.delete_folder(ci, f5.clone(), cx))));
        }
        items
    }

    fn render_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let row = self.rows[ix].clone();
        let selected = self.selected.as_deref() == Some(row.key.as_str());
        let base = div()
            .id(("api-row", ix))
            .group("api-row")
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
                .group_hover("api-row", |s| s.visible())
                .bg(if selected { theme::selected_focused() } else { theme::hover() })
        };
        let name = |text: String, color: gpui::Hsla| div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_color(color).child(text);
        let badge = |m: &str| div().flex_none().w(px(38.)).text_size(px(9.5)).font_weight(FontWeight::BOLD).text_color(theme::c(api::method_color(m))).child(if m == "DELETE" { "DEL".to_string() } else if m == "OPTIONS" { "OPT".into() } else { m.to_string() });
        let rk = row.key.clone();
        // The name box (naming something new, or renaming this row).
        let name_box = |input: Entity<crate::editor::Editor>| div().flex_1().min_w_0().h(px(20.)).px(px(4.)).flex().items_center().bg(theme::bg_input()).border_1().border_color(theme::focus_border()).child(input).into_any_element();
        let renaming = self.renaming(&row.key).then(|| self.naming.as_ref().map(|n| n.input.clone())).flatten();
        let label = |text: String, color: gpui::Hsla| match renaming.clone() {
            Some(i) => name_box(i),
            None => name(text, color).into_any_element(),
        };
        match row.kind {
            RowKind::Naming => {
                let Some(n) = &self.naming else { return div().into_any_element() };
                let icon = if n.what == NameFor::NewCollection { "collection" } else { "folder" };
                base.cursor_default()
                    .child(ui::small_icon("chevron-right", theme::fg()))
                    .child(ui::small_icon(icon, if n.what == NameFor::NewCollection { theme::accent() } else { theme::fg_dim() }))
                    .child(name_box(n.input.clone()))
                    .into_any_element()
            }
            RowKind::Coll(ci) | RowKind::Folder(ci, _) => {
                let c = &self.colls[ci];
                let (fid, label_text, is_root) = match &row.kind {
                    RowKind::Folder(_, f) => (f.clone(), c.root.find(f).map(|x| x.name.clone()).unwrap_or_default(), false),
                    _ => (c.id.clone(), c.name().to_string(), true),
                };
                let open = self.open.contains(&fid);
                let (f1, f2, cid) = (fid.clone(), fid.clone(), c.id.clone());
                let menu = self.coll_menu(ci, fid.clone());
                let menu2 = menu.clone();
                let cid2 = cid.clone();
                let sel = format!("api-coll-{label_text}");
                base.debug_selector(move || sel.clone())
                    .on_click(cx.listener(move |p, _, w, cx| {
                        p.selected = Some(rk.clone());
                        w.focus(&p.focus);
                        if !p.open.remove(&f1) {
                            p.open.insert(f1.clone());
                        }
                        p.rebuild(cx);
                        cx.notify();
                    }))
                    .on_mouse_down(MouseButton::Right, cx.listener(move |p, ev: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        p.open_menu(ev.position, menu.clone(), cx);
                    }))
                    .on_drop(cx.listener(move |p, d: &DraggedRequest, _, cx| {
                        if d.coll == cid2 {
                            p.move_to(&d.coll, &d.id, &f2, cx);
                        }
                    }))
                    .drag_over::<DraggedRequest>(|s, _, _, _| s.bg(theme::selected_focused()))
                    .child(chevron(open))
                    .child(ui::small_icon(if is_root { "collection" } else if open { "folder-open" } else { "folder" }, if is_root { theme::accent() } else { theme::fg_dim() }))
                    .child(label(label_text, if is_root { theme::fg_bright() } else { theme::fg() }))
                    .when(is_root && c.project.is_some(), |d| d.child(div().flex_none().text_size(px(10.)).text_color(theme::fg_muted()).child("workspace")))
                    .when(is_root, |d| d.child(div().flex_none().text_size(px(10.5)).text_color(theme::fg_muted()).child(c.root.count().to_string())))
                    .child(
                        actions()
                            .child(hover_button(format!("api-new-req-{ix}"), "plus", "New request").on_click(cx.listener({
                                let (cid, fid) = (cid.clone(), fid.clone());
                                move |p, _, _, cx| {
                                    cx.stop_propagation();
                                    p.new_request(Some(cid.clone()), Some(fid.clone()), cx);
                                }
                            })))
                            .child(hover_button(format!("api-new-folder-{ix}"), "new-folder", "New folder").on_click(cx.listener({
                                let fid = fid.clone();
                                move |p, _, w, cx| {
                                    cx.stop_propagation();
                                    p.new_folder(ci, &fid, w, cx);
                                }
                            })))
                            .child(hover_button(format!("api-run-{ix}"), "play", "Run").on_click(cx.listener({
                                let (cid, fid) = (cid.clone(), fid.clone());
                                move |_, _, _, cx| {
                                    cx.stop_propagation();
                                    cx.emit(open_req(ApiOpen::Runner { coll: cid.clone(), folder: fid.clone() }));
                                }
                            })))
                            .child(hover_button(format!("api-more-{ix}"), "ellipsis", "More").on_mouse_down(MouseButton::Left, cx.listener(move |p, ev: &MouseDownEvent, _, cx| {
                                cx.stop_propagation();
                                p.open_menu(ev.position, menu2.clone(), cx);
                            }))),
                    )
                    .into_any_element()
            }
            RowKind::Req(ci, item) => {
                let c = &self.colls[ci];
                let (cid, id) = (c.id.clone(), item.id.clone());
                let (cid2, id2) = (cid.clone(), id.clone());
                let item2 = item.clone();
                let drag = DraggedRequest { coll: cid.clone(), id: id.clone(), label: item.name.clone() };
                let sel = format!("api-req-{}", item.name);
                base.debug_selector(move || sel.clone())
                    .on_click(cx.listener(move |p, _, _, cx| {
                        p.selected = Some(rk.clone());
                        cx.emit(open_req(ApiOpen::Request { coll: cid.clone(), id: id.clone() }));
                        cx.notify();
                    }))
                    .on_drag(drag, |d: &DraggedRequest, _, _, cx| cx.new(|_| ui::DragChip(d.label.clone())))
                    .on_mouse_down(MouseButton::Right, cx.listener({
                        let (rid, it) = (item.id.clone(), item.clone());
                        move |p, ev: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            let (r1, r2, it2) = (rid.clone(), rid.clone(), it.clone());
                            let items: Vec<(String, Act)> = vec![
                                ("Rename".into(), Rc::new(move |p: &mut ApiPanel, w: &mut Window, cx: &mut Context<ApiPanel>| p.rename(ci, Target::Req(r1.clone()), w, cx))),
                                ("Duplicate".into(), Rc::new(move |p: &mut ApiPanel, _: &mut Window, cx: &mut Context<ApiPanel>| p.duplicate(ci, &r2, cx))),
                                ("Delete request".into(), Rc::new(move |p: &mut ApiPanel, _: &mut Window, cx: &mut Context<ApiPanel>| p.delete_request(ci, it2.clone(), cx))),
                            ];
                            p.open_menu(ev.position, items, cx);
                        }
                    }))
                    .child(div().w(px(12.)))
                    .child(badge(&item.method))
                    .child(label(item.name.clone(), theme::fg()))
                    .child(
                        actions()
                            .child(hover_button(format!("api-dup-{ix}"), "copy", "Duplicate").on_click(cx.listener(move |p, _, _, cx| {
                                cx.stop_propagation();
                                let _ = &cid2;
                                p.duplicate(ci, &id2, cx);
                            })))
                            .child(hover_button(format!("api-del-{ix}"), "trash", "Delete").on_click(cx.listener(move |p, _, _, cx| {
                                cx.stop_propagation();
                                p.delete_request(ci, item2.clone(), cx);
                            }))),
                    )
                    .into_any_element()
            }
            RowKind::Visit(i) => {
                let v = &self.history[i];
                let m = match v.request.kind {
                    api::Kind::Http => v.request.method.clone(),
                    api::Kind::Websocket => "WS".into(),
                    api::Kind::Sse => "SSE".into(),
                };
                let vid = v.id.clone();
                let url = v.request.url.split_once("://").map(|(_, r)| r.to_string()).unwrap_or_else(|| v.request.url.clone());
                let status_color = match v.status {
                    0 => theme::fg_muted(),
                    200..=299 => theme::git_added(),
                    300..=399 => theme::c(0x388bfd),
                    400..=499 => theme::c(0xd29922),
                    _ => theme::git_deleted(),
                };
                let vid2 = vid.clone();
                base.on_click(cx.listener(move |p, _, _, cx| {
                    p.selected = Some(rk.clone());
                    // As it was: what was sent and what came back (read-only).
                    cx.emit(open_req(ApiOpen::Visit(vid2.clone())));
                    cx.notify();
                }))
                .child(badge(&m))
                .child(name(url, theme::fg()))
                .when(v.status > 0, |d| d.child(div().flex_none().text_size(px(10.5)).text_color(status_color).child(v.status.to_string())))
                .child(div().flex_none().text_size(px(10.5)).text_color(theme::fg_muted()).child(ago(v.at)))
                .child(actions().child(hover_button(format!("api-hist-del-{ix}"), "close", "Remove from history").on_click(cx.listener(move |p, _, _, cx| {
                    cx.stop_propagation();
                    api::delete_history(Some(&vid));
                    p.reload(cx);
                }))))
                .into_any_element()
            }
            RowKind::Env(i) => {
                let e = &self.envs[i];
                let active = self.active_env.as_deref() == Some(e.id.as_str()) && !e.global;
                let (id, id2) = (e.id.clone(), e.id.clone());
                let global = e.global;
                base.on_click(cx.listener(move |p, _, _, cx| {
                    p.selected = Some(rk.clone());
                    cx.emit(open_req(ApiOpen::Env(id.clone())));
                    cx.notify();
                }))
                .child(if global {
                    ui::small_icon("globe", theme::fg_dim()).into_any_element()
                } else {
                    div()
                        .id(SharedString::from(format!("api-env-radio-{i}")))
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(14.))
                        .rounded(px(7.))
                        .border_1()
                        .border_color(if active { theme::accent() } else { theme::border_input() })
                        .tooltip(ui::tooltip("Use this environment"))
                        .on_click(cx.listener(move |p, _, _, cx| {
                            cx.stop_propagation();
                            let now = if active { None } else { Some(id2.as_str()) };
                            api::set_active_env(p.project().as_deref(), now);
                            p.reload(cx);
                            cx.emit(Request::Api(ApiRequest::Changed));
                        }))
                        .when(active, |d| d.child(div().size(px(7.)).rounded(px(4.)).bg(theme::accent())))
                        .into_any_element()
                })
                .child(name(e.name.clone(), if active { theme::fg_bright() } else { theme::fg() }))
                .child(div().flex_none().text_size(px(10.5)).text_color(theme::fg_muted()).child(format!("{} vars", e.vars.len())))
                .when(e.project.is_some(), |d| d.child(div().flex_none().text_size(px(10.)).text_color(theme::fg_muted()).child("workspace")))
                .into_any_element()
            }
            RowKind::Info(text) => base.cursor_default().child(div().pl(px(16.)).text_size(px(12.)).text_color(theme::fg_muted()).child(text)).into_any_element(),
        }
    }

    fn render_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (at, items) = self.menu.as_ref()?;
        let list = div()
            .id("api-panel-menu")
            .occlude()
            .min_w(px(190.))
            .whitespace_nowrap()
            .py(px(4.))
            .bg(theme::bg_popup())
            .border_1()
            .border_color(theme::border_input())
            .rounded(px(6.))
            .shadow_lg()
            .text_size(px(12.))
            .on_mouse_down_out(cx.listener(|p, _, _, cx| {
                p.menu = None;
                cx.notify();
            }))
            .children(items.iter().enumerate().map(|(i, (label, act))| {
                let act = act.clone();
                let danger = label.starts_with("Delete");
                div()
                    .id(("api-panel-menu-item", i))
                    .debug_selector(move || format!("api-panel-menu-{i}"))
                    .flex()
                    .items_center()
                    .h(px(22.))
                    .px(px(10.))
                    .mx(px(4.))
                    .rounded(px(3.))
                    .cursor_pointer()
                    .when(danger, |d| d.text_color(theme::git_deleted()))
                    .hover(|s| s.bg(theme::selected_focused()))
                    .on_click(cx.listener(move |p, _, w, cx| {
                        p.menu = None;
                        act(p, w, cx);
                        cx.notify();
                    }))
                    .child(label.clone())
            }));
        Some(deferred(anchored().position(*at).snap_to_window().child(list)).with_priority(1).into_any_element())
    }
}

fn find_item<'a>(f: &'a Folder, id: &str) -> Option<&'a api::Item> {
    f.requests.iter().find(|r| r.id == id).or_else(|| f.folders.iter().find_map(|s| find_item(s, id)))
}

fn ago(at: u64) -> String {
    let s = api::now_secs().saturating_sub(at);
    match s {
        0..60 => "now".into(),
        60..3600 => format!("{}m", s / 60),
        3600..86_400 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}

impl Focusable for ApiPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ApiPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let head_button = |id: &'static str, icon: &'static str, tip: &'static str| ui::icon_button(id, icon).debug_selector(move || id.to_string()).tooltip(ui::tooltip(tip));
        let tab = |id: &'static str, label: &'static str, t: Tab, cx: &mut Context<Self>| {
            let on = self.tab == t;
            div()
                .id(id)
                .debug_selector(move || id.to_string())
                .flex_1()
                .flex()
                .justify_center()
                .py(px(3.))
                .rounded(px(4.))
                .text_size(px(11.5))
                .cursor_pointer()
                .when(on, |d| d.bg(theme::selected_focused()).text_color(theme::fg_bright()))
                .when(!on, |d| d.text_color(theme::fg_dim()).hover(|s| s.bg(theme::hover())))
                .on_click(cx.listener(move |p, _, _, cx| p.set_tab(t, cx)))
                .child(label)
        };
        let empty: Option<AnyElement> = match self.tab {
            Tab::Collections if self.colls.is_empty() => Some(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .px(px(20.))
                    .pt(px(8.))
                    .text_size(px(12.))
                    .text_color(theme::fg_dim())
                    .child("Save requests in collections, with folders.")
                    .child(
                        div()
                            .id("api-empty-new")
                            .debug_selector(|| "api-empty-new".into())
                            .flex()
                            .justify_center()
                            .py(px(5.))
                            .rounded(px(4.))
                            .bg(theme::accent())
                            .text_color(theme::on(theme::accent()))
                            .font_weight(FontWeight::MEDIUM)
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::accent_hover()))
                            .on_click(cx.listener(|p, _, _, cx| p.new_request(None, None, cx)))
                            .child("New request"),
                    )
                    .child(
                        div()
                            .id("api-empty-coll")
                            .flex()
                            .justify_center()
                            .py(px(5.))
                            .rounded(px(4.))
                            .border_1()
                            .border_color(theme::border_input())
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::hover()))
                            .on_click(cx.listener(|p, _, w, cx| p.new_collection(w, cx)))
                            .child("New collection"),
                    )
                    .child(div().text_size(px(11.)).text_color(theme::fg_muted()).child("Or import one: Postman, Insomnia, OpenAPI, Bruno, cURL."))
                    .into_any_element(),
            ),
            Tab::History if self.history.is_empty() => Some(div().px(px(20.)).pt(px(8.)).text_size(px(12.)).text_color(theme::fg_dim()).child("Requests you send show here (the last 30 days).").into_any_element()),
            _ => None,
        };
        let menu = self.render_menu(cx);
        div()
            .key_context("ApiPanel")
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
                    .child(div().flex_1().child("API"))
                    .child(head_button("api-new", "plus", "New request").on_click(cx.listener(|p, _, _, cx| p.new_request(None, None, cx))))
                    .child(head_button("api-new-coll", "new-folder", "New collection").on_click(cx.listener(|p, _, w, cx| p.new_collection(w, cx))))
                    .child(head_button("api-import", "arrow-down", "Import: Postman, Insomnia, OpenAPI, Bruno, .env…").on_click(cx.listener(|p, _, _, cx| p.import(cx))))
                    .child(head_button("api-cookies", "cookie", "Cookies").on_click(cx.listener(|_, _, _, cx| cx.emit(open_req(ApiOpen::Cookies)))))
                    .child(match self.tab {
                        Tab::History => head_button("api-clear-history", "trash", "Clear history")
                            .on_click(cx.listener(|p, _, _, cx| {
                                p.ask("Clear the request history?".into(), "", "Clear", |p, cx| {
                                    api::delete_history(None);
                                    p.reload(cx);
                                }, cx);
                            }))
                            .into_any_element(),
                        Tab::Envs => head_button("api-new-env", "plus", "New environment").on_click(cx.listener(|p, _, _, cx| p.new_env(cx))).into_any_element(),
                        Tab::Collections => head_button("api-collapse", "collapse-all", "Collapse all")
                            .on_click(cx.listener(|p, _, _, cx| {
                                p.open.clear();
                                p.rebuild(cx);
                                cx.notify();
                            }))
                            .into_any_element(),
                    }),
            )
            .child(
                div()
                    .flex()
                    .gap(px(2.))
                    .mx(px(10.))
                    .p(px(2.))
                    .rounded(px(5.))
                    .border_1()
                    .border_color(theme::border_input())
                    .child(tab("api-tab-colls", "Collections", Tab::Collections, cx))
                    .child(tab("api-tab-history", "History", Tab::History, cx))
                    .child(tab("api-tab-envs", "Environments", Tab::Envs, cx)),
            )
            .child(div().mx(px(10.)).my(px(6.)).h(px(24.)).px(px(6.)).flex().items_center().rounded(px(4.)).bg(theme::bg_input()).border_1().border_color(theme::border_input()).child(self.filter.clone()))
            .when_some(self.status.clone(), |d, s| {
                d.child(
                    div()
                        .id("api-panel-status")
                        .mx(px(12.))
                        .mb(px(6.))
                        .text_size(px(11.5))
                        .text_color(theme::fg_dim())
                        .cursor_pointer()
                        .on_click(cx.listener(|p, _, _, cx| {
                            p.status = None;
                            cx.notify();
                        }))
                        .child(s),
                )
            })
            .children(empty)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .text_size(px(13.))
                    .child(uniform_list("api-tree", self.rows.len(), cx.processor(move |p, range: Range<usize>, _, cx| range.map(|ix| p.render_row(ix, cx)).collect::<Vec<_>>())).track_scroll(self.scroll.clone()).flex_1()),
            )
            .children(menu)
    }
}
