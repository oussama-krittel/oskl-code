//! Database tabs: the connect form, a table's (collection's, key's, queue's) Data View and
//! Query, and query tabs. Results show in a grid that draws only the rows on screen.

use crate::db::{self, Conn, Grid, Kind, Page, PageSpec, Scope, TextStyle, Tree, compass, compass::Seg, engine};
use std::collections::HashMap;
use std::collections::HashSet;
use crate::editor::{Editor, EditorEvent, EditorMode};
use crate::request::Request;
use crate::theme;
use crate::ui::{self, px};
use gpui::prelude::*;
use gpui::{
    App, ClickEvent, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, MouseButton, ScrollHandle, SharedString, Subscription, Task,
    ListAlignment, ListState, Window, actions, deferred, div, list,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

mod assist;
mod compass_ui;
mod overview;
mod sel;
use compass_ui::{AGG, Compass};
pub use compass_ui::CTab;

actions!(db_view, [RunQuery, CopyCell, RefreshData, RowUp, RowDown, CloseDetail, SelectAllRows]);

pub fn bind_keys() {
    crate::keybindings::collect([
        crate::keybindings::kb("cmd-enter", RunQuery, Some("DbView")),
        crate::keybindings::kb("cmd-enter", RunQuery, Some("DbView > Editor")),
        crate::keybindings::kb("cmd-c", CopyCell, Some("DbView")),
        crate::keybindings::kb("cmd-a", SelectAllRows, Some("DbView")),
        crate::keybindings::kb("cmd-r", RefreshData, Some("DbView")),
        crate::keybindings::kb("up", RowUp, Some("DbView")),
        crate::keybindings::kb("down", RowDown, Some("DbView")),
        crate::keybindings::kb("escape", CloseDetail, Some("DbView")),
    ]);
}

/// What a database tab shows.
#[derive(Clone, Debug, PartialEq)]
pub enum Open {
    /// The connect form for a new connection.
    New,
    /// The form for a saved one.
    Edit(String),
    /// A table, collection, key or queue: its data, and a query tab.
    Data { conn: String, path: Vec<String> },
    /// A query on a connection or one of its databases.
    Query { conn: String, path: Vec<String> },
    /// A MongoDB database: its collections with their sizes.
    Database { conn: String, db: String },
}

/// What a database tab asks of the workspace and the panel.
pub enum DbRequest {
    Open(Open),
    /// Saved connections changed (expand this one in the panel, if any).
    Changed { expand: Option<String> },
    /// This connection was connected (from the panel): its tabs load.
    Connected(String),
    /// Close this tab.
    Close,
}

/// Rows per page in the Data View.
const PAGE_SIZES: [usize; 4] = [25, 50, 100, 500];

pub struct DbView {
    focus: FocusHandle,
    pub open: Open,
    body: Body,
    _subs: Vec<Subscription>,
    /// Text dragged over in the documents and tables (⌘C copies it).
    sel: sel::Selection,
    editing: Option<Editing>,
    /// Table: the cell the next edit box opens over.
    edit_anchor: Option<usize>,
    /// Table: the opened row before the last click (a double-click puts it back).
    open_before: Option<Option<usize>>,
    /// A column's edge being dragged: (grid, column, where the drag started, its width then).
    col_drag: Option<(Option<usize>, usize, gpui::Pixels, f32)>,
}

enum Body {
    Form(Box<Form>),
    Data(Box<Data>),
    Overview(Box<overview::Overview>),
    Missing(String),
}

struct Form {
    conn: Conn,
    saved: bool,
    name: Entity<Editor>,
    group: Entity<Editor>,
    host: Entity<Editor>,
    port: Entity<Editor>,
    user: Entity<Editor>,
    password: Entity<Editor>,
    database: Entity<Editor>,
    url: Entity<Editor>,
    /// The project a Workspace connection belongs to (none: no folder open).
    project: Option<PathBuf>,
    status: Option<Result<String, String>>,
    busy: bool,
    task: Option<Task<()>>,
}

#[derive(Clone, Copy, PartialEq)]
enum Inner {
    DataView,
    Query,
}

struct Data {
    conn: Conn,
    path: Vec<String>,
    /// Has a Data View (a table…); else it's a query tab only.
    has_data: bool,
    inner: Inner,
    // Data View
    spec: PageSpec,
    /// What was last read (put back when unsaved changes are kept instead of reading again).
    loaded_spec: PageSpec,
    page: Option<Result<Page, String>>,
    grid: Option<GridView>,
    loading: bool,
    search: Entity<Editor>,
    menu: Option<Menu>,
    search_task: Option<Task<()>>,
    // Query
    query: Entity<Editor>,
    results: Vec<GridView>,
    result_ix: usize,
    query_error: Option<String>,
    running: Option<Instant>,
    took: Option<std::time::Duration>,
    task: Option<Task<()>>,
    query_task: Option<Task<()>>,
    tick: Option<Task<()>>,
    exporting: bool,
    /// How MongoDB documents show.
    doc_mode: DocMode,
    /// A MongoDB collection: Compass's tabs (query bar, aggregations, schema, indexes, validation).
    compass: Option<Box<Compass>>,
    /// What the query editors suggest from.
    names: assist::Shared,
}

/// Puts a grid's columns in the order of `old` (those it has), the others after them.
fn keep_column_order(old: &[String], g: &mut Grid) {
    let mut order: Vec<usize> = old.iter().filter_map(|c| g.columns.iter().position(|x| x == c)).collect();
    let rest: Vec<usize> = (0..g.columns.len()).filter(|i| !order.contains(i)).collect();
    order.extend(rest);
    if order.iter().enumerate().all(|(a, b)| a == *b) {
        return;
    }
    g.columns = order.iter().map(|i| g.columns[*i].clone()).collect();
    for row in &mut g.rows {
        *row = order.iter().map(|i| row.get(*i).cloned().flatten()).collect();
    }
}

/// The first statement of a query that changes or deletes data (dropping, deleting, emptying,
/// updating many…), to ask before running it.
pub(crate) fn destructive(kind: Kind, text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let words: &[&str] = match kind {
        Kind::MongoDb => &["dropdatabase(", ".drop(", "dropindex", "dropindexes", "deletemany(", "deleteone(", ".remove(", "updatemany(", "replaceone(", "renamecollection", "\"$out\"", "$out:", "'$out'", "\"$merge\"", "$merge:", "'$merge'", "findoneanddelete", "findandmodify"],
        Kind::Postgres | Kind::SqlServer => &["drop ", "truncate ", "delete ", "update ", "alter ", "merge "],
        Kind::Redis => &["flushall", "flushdb", "del ", "unlink ", "rename "],
        Kind::RabbitMq => &["delete ", "/purge", "/contents"],
    };
    let sql = matches!(kind, Kind::Postgres | Kind::SqlServer);
    for (line, low) in text.lines().zip(lower.lines()) {
        let l = low.trim_start();
        // SQL: a statement starting with it (not a column named update_at…); others: anywhere.
        let hit = if sql { words.iter().any(|w| l.starts_with(w) || l.contains(&format!("; {w}")) || l.contains(&format!(";{w}"))) } else { words.iter().any(|w| l.contains(w)) };
        if hit && !l.starts_with("--") && !l.starts_with("//") {
            let shown: String = line.trim().chars().take(120).collect();
            return Some(shown);
        }
    }
    None
}

/// "This changes data: run it?" — `go` runs it when answered yes.
pub(crate) fn confirm_destructive(what: &str, on: &str, weak: gpui::WeakEntity<DbView>, go: fn(&mut DbView, &mut Context<DbView>), cx: &mut Context<DbView>) {
    cx.emit(Request::Ask(crate::dialog::Ask {
        title: "Run a query that changes data?".into(),
        detail: format!("On {on}:\n{what}\n\nIt changes or deletes data, and can't be undone here."),
        buttons: vec!["Run".into()],
        danger: true,
        cancel_default: true,
        cancel_label: None,
        on_answer: std::rc::Rc::new(move |_, _, cx: &mut App| {
            if let Some(v) = weak.upgrade() {
                v.update(cx, |v, cx| go(v, cx));
            }
        }),
    }));
}

/// Whether a table's first column is an id, kept in view when it scrolls sideways.
fn pinned_first(columns: &[String]) -> bool {
    columns.first().is_some_and(|c| c == "_id" || c.eq_ignore_ascii_case("id"))
}

/// Remembers the fields (columns) of results, for suggestions; MongoDB's nested ones as `a.b`.
fn learn_fields(names: &assist::Shared, g: &Grid) {
    let mut n = names.borrow_mut();
    let mut add = |f: String| {
        if n.fields.len() < 2000 && !n.fields.contains(&f) {
            n.fields.push(f);
        }
    };
    for c in &g.columns {
        add(c.clone());
    }
    fn walk(t: &Tree, prefix: &str, depth: usize, add: &mut dyn FnMut(String)) {
        if let Tree::Object(fields) = t {
            for (k, v) in fields {
                let p = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
                if depth > 0 {
                    add(p.clone());
                }
                if depth < 2 {
                    walk(v, &p, depth + 1, add);
                }
            }
        }
    }
    for t in g.trees.iter().flatten().take(50) {
        walk(t, "", 0, &mut add);
    }
}

/// The query editors' coloring: MongoDB's shell is JavaScript; SQL its own.
fn query_lang(kind: Kind) -> &'static str {
    match kind {
        Kind::MongoDb => "js",
        Kind::Postgres => "sql",
        Kind::SqlServer => "tsql",
        Kind::Redis | Kind::RabbitMq => "",
    }
}

fn suggestions(kind: Kind, names: &assist::Shared) -> Option<assist::Suggest> {
    match kind {
        Kind::MongoDb => Some(assist::mongo(names.clone())),
        Kind::Postgres | Kind::SqlServer => Some(assist::sql(names.clone())),
        Kind::Redis | Kind::RabbitMq => None,
    }
}

/// MongoDB documents as cards of fields (like Compass's List), as JSON, or as a table.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum DocMode {
    List,
    Json,
    Table,
}

#[derive(Clone, Copy, PartialEq)]
enum Menu {
    Columns,
    Sizes,
}

/// A grid on screen: the rows, column widths, scroll, the selected cell, the open row and
/// the unfolded parts of its documents.
struct GridView {
    grid: Arc<Grid>,
    /// The widths the columns started with (a double-click on a header's edge puts one back).
    auto_widths: Vec<f32>,
    widths: Vec<f32>,
    types: Vec<String>,
    list: ListState,
    hscroll: ScrollHandle,
    sel: Option<(usize, usize)>,
    open: Option<usize>,
    /// "<row>/<field>/…" of nested values shown open.
    unfolded: HashSet<String>,
    /// Documents being edited in place, by row.
    edits: HashMap<usize, DocEdit>,
    /// Documents being edited as JSON text, by row.
    json_edits: HashMap<usize, JsonEdit>,
}

/// A document's changes, not saved yet (Update saves them, Cancel drops them).
struct DocEdit {
    original: Tree,
    working: Tree,
    ops: Vec<compass::Op>,
    /// Fields changed or added, highlighted.
    changed: HashSet<Vec<Seg>>,
    saving: bool,
    error: Option<String>,
}

/// The key or value being typed into, in a document's card.
struct Editing {
    which: Option<usize>,
    row: usize,
    path: Vec<Seg>,
    key: bool,
    /// A field just added: its value is asked for next.
    fresh: bool,
    editor: Entity<Editor>,
    _sub: Subscription,
    /// The value's type (Compass's list); picked from the menu when `ty_chosen`.
    ty: &'static str,
    ty_chosen: bool,
    /// The type menu is open.
    menu: bool,
    /// Table: the cell it's typed over.
    anchor_col: Option<usize>,
}

/// A document typed whole, as JSON, in its card.
struct JsonEdit {
    editor: Entity<Editor>,
    /// The text it started with (unchanged: nothing to keep when the view is switched).
    initial: String,
    /// Why the text isn't a document (Update waits until it is).
    invalid: Option<String>,
    error: Option<String>,
    saving: bool,
    _sub: Subscription,
}

impl GridView {
    fn new(grid: Grid) -> GridView {
        // Wide enough for the longest of the first rows' values (capped), at ~7px a character.
        let widths: Vec<f32> = (0..grid.columns.len())
            .map(|c| {
                let longest = grid.rows.iter().take(200).map(|r| r.get(c).and_then(|v| v.as_ref()).map(|v| v.chars().count()).unwrap_or(4)).max().unwrap_or(0);
                let chars = longest.max(grid.columns[c].chars().count() + 2).min(48);
                (chars as f32 * 7.9 + 26.).clamp(64., 420.)
            })
            .collect();
        let types = column_types(&grid);
        // The header shows the name and (MongoDB) its type: room for both.
        let widths: Vec<f32> = widths.into_iter().enumerate().map(|(c, w): (usize, f32)| {
            let head = grid.columns[c].chars().count() + types.get(c).map(|t| t.len() + 1).unwrap_or(0) + 3;
            w.max((head as f32 * 7.6 + 26.).min(420.))
        }).collect();
        let list = ListState::new(grid.rows.len(), ListAlignment::Top, gpui::px(600.));
        GridView { grid: Arc::new(grid), auto_widths: widths.clone(), widths, types, list, hscroll: ScrollHandle::new(), sel: None, open: None, unfolded: HashSet::new(), edits: HashMap::new(), json_edits: HashMap::new() }
    }

    fn selected_text(&self) -> Option<(String, Option<String>)> {
        let (r, c) = self.sel?;
        Some((self.grid.columns.get(c)?.clone(), self.grid.rows.get(r)?.get(c)?.clone()))
    }
}

fn input(placeholder: &str, text: &str, cx: &mut App) -> Entity<Editor> {
    cx.new(|cx| {
        let mut ed = Editor::new(EditorMode::SingleLine, cx);
        ed.placeholder = placeholder.to_string().into();
        ed.set_text(text, cx);
        ed
    })
}

impl EventEmitter<Request> for DbView {}

impl DbView {
    pub fn new(open: Open, project: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subs = Vec::new();
        let find = |id: &str| db::load().into_iter().find(|c| c.id == id);
        let body = match &open {
            Open::New => Body::Form(Box::new(Form::new(Conn::new(Kind::Postgres), false, project, &mut subs, window, cx))),
            Open::Edit(id) => match find(id) {
                Some(c) => Body::Form(Box::new(Form::new(c, true, project, &mut subs, window, cx))),
                None => Body::Missing("This connection was deleted.".into()),
            },
            Open::Database { conn, db } => match find(conn) {
                Some(c) => Body::Overview(Box::new(overview::Overview::new(c, db.clone()))),
                None => Body::Missing("This connection was deleted.".into()),
            },
            Open::Data { conn, path } | Open::Query { conn, path } => match find(conn) {
                Some(c) => {
                    let has_data = matches!(open, Open::Data { .. });
                    let starter = db::starter_query(c.kind, path);
                    let search = input("Search…", "", cx);
                    // Names the suggestions use: collections or tables, fields or columns.
                    let names: assist::Shared = Default::default();
                    let lang = query_lang(c.kind);
                    let suggest = suggestions(c.kind, &names);
                    let query = cx.new(|cx| {
                        let mut ed = Editor::new(EditorMode::Full, cx);
                        ed.set_text(&starter, cx);
                        ed.set_language(lang, cx);
                        ed.suggest = suggest.clone();
                        ed
                    });
                    subs.push(cx.subscribe_in(&search, window, |v: &mut DbView, _, ev: &EditorEvent, _, cx| match ev {
                        EditorEvent::Submit => v.search_now(cx),
                        EditorEvent::Changed => v.search_soon(cx),
                        _ => {}
                    }));
                    let compass = (has_data && c.kind == Kind::MongoDb && path.len() == 2).then(|| Box::new(Compass::new(&names, &mut subs, window, cx)));
                    let data = Data {
                        conn: c,
                        path: path.clone(),
                        has_data,
                        inner: if has_data { Inner::DataView } else { Inner::Query },
                        spec: PageSpec { limit: PAGE_SIZES[0], mongo: compass.as_ref().map(|_| Default::default()), ..Default::default() },
                        loaded_spec: PageSpec::default(),
                        page: None,
                        grid: None,
                        loading: false,
                        search,
                        menu: None,
                        search_task: None,
                        query,
                        results: Vec::new(),
                        result_ix: 0,
                        query_error: None,
                        running: None,
                        took: None,
                        task: None,
                        query_task: None,
                        tick: None,
                        exporting: false,
                        doc_mode: DocMode::List,
                        compass,
                        names,
                    };
                    Body::Data(Box::new(data))
                }
                None => Body::Missing("This connection was deleted.".into()),
            },
        };
        let mut v = DbView { focus: cx.focus_handle(), open, body, _subs: subs, sel: sel::Selection::default(), editing: None, edit_anchor: None, open_before: None, col_drag: None };
        v.load(cx);
        v.learn_names(cx);
        v.load_overview(cx);
        // The Indexes tab shows how many there are, like Compass.
        if matches!(&v.body, Body::Data(d) if d.compass.is_some()) {
            v.load_indexes(cx);
        }
        v.load_secret(cx);
        v
    }

    /// A saved connection's password (or connection string) into the form, read in the
    /// background: the Keychain may ask first, and the window shouldn't freeze meanwhile.
    fn load_secret(&mut self, cx: &mut Context<Self>) {
        let Body::Form(f) = &mut self.body else { return };
        if !f.saved {
            return;
        }
        let read = engine::run(engine::secret(f.conn.id.clone()));
        f.task = Some(cx.spawn(async move |this, cx| {
            let secret = read.await.unwrap_or_default();
            this.update(cx, |v, cx| {
                if let Some(f) = v.form_mut() {
                    let target = if f.conn.use_url { f.url.clone() } else { f.password.clone() };
                    if target.read(cx).text().is_empty() {
                        target.update(cx, |e, cx| e.set_text(&secret, cx));
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    pub fn focus_target(&self, cx: &App) -> FocusHandle {
        match &self.body {
            Body::Form(f) => f.name.read(cx).focus_target(cx),
            Body::Data(d) if d.inner == Inner::Query => d.query.read(cx).focus_target(cx),
            Body::Data(d) if d.compass.as_ref().and_then(|c| c.focus_target(cx)).is_some() => d.compass.as_ref().and_then(|c| c.focus_target(cx)).unwrap(),
            _ => self.focus.clone(),
        }
    }

    /// The tab's title: what it shows, and (for the tooltip) where.
    pub fn title(&self) -> (String, String) {
        match &self.body {
            Body::Form(f) if f.saved => (format!("Edit: {}", f.conn.title()), String::new()),
            Body::Form(_) => ("Connect".into(), String::new()),
            Body::Data(d) => {
                let what = d.path.last().cloned().unwrap_or_else(|| d.conn.title());
                if d.has_data { (format!("{}: {what}", d.conn.kind.label()), d.path.join(" / ")) } else { (format!("Query: {what}"), d.conn.title()) }
            }
            Body::Overview(o) => (o.db.clone(), o.conn.title()),
            Body::Missing(_) => ("Database".into(), String::new()),
        }
    }


    /// Its type (the form's follows the type chosen).
    pub fn kind(&self) -> Option<Kind> {
        match &self.body {
            Body::Form(f) => Some(f.conn.kind),
            Body::Data(d) => Some(d.conn.kind),
            Body::Overview(o) => Some(o.conn.kind),
            Body::Missing(_) => None,
        }
    }

    /// The connection it uses (data and query tabs).
    pub fn conn_id(&self) -> Option<String> {
        match &self.body {
            Body::Data(d) => Some(d.conn.id.clone()),
            Body::Overview(o) => Some(o.conn.id.clone()),
            _ => None,
        }
    }

    /// Its connection was connected: loads what it shows.
    pub fn reconnected(&mut self, cx: &mut Context<Self>) {
        let waiting = matches!(&self.body, Body::Data(d) if d.page.as_ref().is_none_or(|p| p.is_err()));
        if waiting {
            self.load(cx);
        }
        if matches!(&self.body, Body::Overview(o) if o.list.as_ref().is_none_or(|l| l.is_err())) {
            self.load_overview(cx);
        }
    }

    /// Whether this tab shows `open` (to switch to it instead of opening another).
    pub fn shows(&self, open: &Open) -> bool {
        matches!(open, Open::Edit(_) | Open::Data { .. } | Open::Database { .. }) && &self.open == open
    }

    /// A saved connection changed: data tabs use its new settings.
    pub fn conn_changed(&mut self, cx: &mut Context<Self>) {
        if let Body::Data(d) = &mut self.body {
            match db::load().into_iter().find(|c| c.id == d.conn.id) {
                Some(c) => d.conn = c,
                None => self.body = Body::Missing("This connection was deleted.".into()),
            }
            cx.notify();
        }
    }

    // ------------------------------------------------------------ data view

    /// Documents changed in place and not saved yet (or being typed into).
    fn has_unsaved(&self, cx: &App) -> bool {
        let Body::Data(d) = &self.body else { return false };
        let Some(g) = &d.grid else { return false };
        self.editing.as_ref().is_some_and(|e| e.which.is_none())
            || g.edits.values().any(|e| e.working != e.original)
            || g.json_edits.values().any(|j| j.editor.read(cx).text() != j.initial)
    }

    /// Reads the rows (again). Unsaved changes in them are asked about first: kept, nothing is
    /// read and the view stays as it was (its sort, page, query); or discarded.
    fn load(&mut self, cx: &mut Context<Self>) {
        if !self.has_unsaved(cx) {
            return self.load_now(cx);
        }
        let Body::Data(d) = &mut self.body else { return };
        let wanted = std::mem::replace(&mut d.spec, d.loaded_spec.clone());
        let weak = cx.entity().downgrade();
        cx.emit(Request::Ask(crate::dialog::Ask {
            title: "Discard unsaved changes?".into(),
            detail: "The documents are read again: changes not saved with Update are lost.".into(),
            buttons: vec!["Discard changes".into()],
            danger: true,
            cancel_default: true,
            cancel_label: Some("Keep editing"),
            on_answer: std::rc::Rc::new(move |_, _, cx: &mut App| {
                let wanted = wanted.clone();
                if let Some(v) = weak.upgrade() {
                    v.update(cx, |v, cx| {
                        v.editing = None;
                        if let Body::Data(d) = &mut v.body {
                            d.spec = wanted;
                            if let Some(g) = &mut d.grid {
                                g.edits.clear();
                                g.json_edits.clear();
                            }
                        }
                        v.load_now(cx);
                    });
                }
            }),
        }));
        cx.notify();
    }

    fn load_now(&mut self, cx: &mut Context<Self>) {
        let Body::Data(d) = &mut self.body else { return };
        if !d.has_data {
            return;
        }
        d.loaded_spec = d.spec.clone();
        d.loading = true;
        let work = engine::run(engine::page(d.conn.clone(), d.path.clone(), d.spec.clone()));
        d.task = Some(cx.spawn(async move |this, cx| {
            let r = work.await;
            this.update(cx, |v, cx| {
                if let Body::Data(d) = &mut v.body {
                    d.loading = false;
                    let keep_cols = d.grid.as_ref().map(|g| g.grid.columns.clone());
                    d.grid = r.as_ref().ok().map(|p| {
                        let mut grid = p.grid.clone();
                        // Columns stay in the order shown (sorting puts other documents first,
                        // whose fields may come in another order); new ones go at the end.
                        if let Some(old) = &keep_cols {
                            keep_column_order(old, &mut grid);
                        }
                        let mut g = GridView::new(grid);
                        // Columns seen before keep their widths (also as dragged), so nothing jumps.
                        if let Some(old) = &d.grid {
                            for (i, c) in g.grid.columns.iter().enumerate() {
                                if let Some(j) = old.grid.columns.iter().position(|x| x == c) {
                                    g.widths[i] = old.widths[j];
                                }
                            }
                        }
                        // Sorting, paging, a save or refresh keeps the place: the columns in view (sorted
                        // documents can list their fields in another order, so whatever they are), and
                        // what was opened in the documents.
                        if let Some(old) = &d.grid {
                            g.hscroll = old.hscroll.clone();
                            g.unfolded = old.unfolded.clone();
                        }
                        g
                    });
                    if let Ok(p) = &r {
                        learn_fields(&d.names, &p.grid);
                    }
                    v.sel.reset();
                    // The rows were read again: an edit box open in one of them goes.
                    if v.editing.as_ref().is_some_and(|e| e.which.is_none()) {
                        v.editing = None;
                    }
                    d.page = Some(r);
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// The collections (or tables and views) the suggestions offer, read once in the background.
    fn learn_names(&mut self, cx: &mut Context<Self>) {
        let Body::Data(d) = &self.body else { return };
        if suggestions(d.conn.kind, &d.names).is_none() {
            return;
        }
        let (conn, names) = (d.conn.clone(), d.names.clone());
        // Where its tables are: the tab's database (MongoDB), or the database it's on.
        let start: Vec<String> = d.path.iter().take(1).cloned().collect();
        let work = engine::run(async move {
            let mut found = Vec::new();
            let mut queue = vec![(start, 0)];
            let mut calls = 0;
            while let Some((path, depth)) = queue.pop() {
                calls += 1;
                if calls > 30 {
                    break;
                }
                let Ok(nodes) = engine::children(conn.clone(), path).await else { continue };
                for n in nodes {
                    match n.kind {
                        db::NodeKind::Table | db::NodeKind::View | db::NodeKind::Collection => found.push(n.label),
                        db::NodeKind::Database | db::NodeKind::Schema | db::NodeKind::Folder if depth < 3 && !matches!(n.label.as_str(), "admin" | "config" | "local" | "information_schema" | "pg_catalog") => queue.push((n.path, depth + 1)),
                        _ => {}
                    }
                }
            }
            Ok::<_, anyhow::Error>(found)
        });
        cx.spawn(async move |_, _| {
            // As they are now (one dropped is gone, one created there).
            if let Ok(mut found) = work.await {
                found.dedup();
                names.borrow_mut().tables = found;
            }
        })
        .detach();
    }

    fn search_now(&mut self, cx: &mut Context<Self>) {
        let Body::Data(d) = &mut self.body else { return };
        let text = d.search.read(cx).text();
        if text.trim() == d.spec.search.trim() {
            return;
        }
        d.spec.search = text;
        d.spec.offset = 0;
        d.search_task = None;
        self.load(cx);
    }

    /// Searches once typing pauses.
    fn search_soon(&mut self, cx: &mut Context<Self>) {
        let Body::Data(d) = &mut self.body else { return };
        d.search_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(450)).await;
            this.update(cx, |v, cx| v.search_now(cx)).ok();
        }));
    }

    fn set_page(&mut self, offset: usize, cx: &mut Context<Self>) {
        if let Body::Data(d) = &mut self.body {
            d.spec.offset = offset;
            self.load(cx);
        }
    }

    fn sort_by(&mut self, col: String, cx: &mut Context<Self>) {
        if let Body::Data(d) = &mut self.body {
            // Ascending, descending, unsorted.
            d.spec.sort = match d.spec.sort.take() {
                Some((c, false)) if c == col => Some((c, true)),
                Some((c, true)) if c == col => None,
                _ => Some((col, false)),
            };
            d.spec.offset = 0;
            // MongoDB: the query bar's Sort says it too (one sort, shown where it's set).
            if let Some(c) = d.compass.as_deref() {
                let text = match &d.spec.sort {
                    Some((col, desc)) => format!("{{ {}: {} }}", Tree::key(col, TextStyle::Shell), if *desc { -1 } else { 1 }),
                    None => String::new(),
                };
                c.set_sort_text(&text, cx);
                if let Some(q) = &mut d.spec.mongo {
                    q.sort = text;
                }
            }
            self.load(cx);
        }
    }

    fn refresh_data(&mut self, _: &RefreshData, _: &mut Window, cx: &mut Context<Self>) {
        self.learn_names(cx);
        self.load(cx);
    }

    /// Writes the table (with its search and sort) to a CSV file, page by page, at most
    /// 100,000 rows.
    fn export_csv(&mut self, cx: &mut Context<Self>) {
        let Body::Data(d) = &mut self.body else { return };
        if d.exporting {
            return;
        }
        let name = format!("{}.csv", d.path.last().cloned().unwrap_or_else(|| "export".into()).replace(['/', ':'], "_"));
        let dir = std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Downloads")).unwrap_or_default();
        let ask = cx.prompt_for_new_path(&dir, Some(&name));
        let (conn, path, spec) = (d.conn.clone(), d.path.clone(), d.spec.clone());
        d.exporting = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let target = ask.await.ok().and_then(|r| r.ok()).flatten();
            let result = match target {
                None => Ok(None),
                Some(file) => engine::run(async move {
                    use std::io::Write;
                    let mut out = std::io::BufWriter::new(std::fs::File::create(&file)?);
                    let field = |s: &str| if s.contains([',', '"', '\n', '\r']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() };
                    let (mut offset, mut rows) = (0usize, 0usize);
                    loop {
                        let page = engine::page(conn.clone(), path.clone(), PageSpec { offset, limit: 1000, ..spec.clone() }).await?;
                        if offset == 0 {
                            writeln!(out, "{}", page.grid.columns.iter().map(|c| field(c)).collect::<Vec<_>>().join(","))?;
                        }
                        for r in &page.grid.rows {
                            writeln!(out, "{}", r.iter().map(|c| c.as_deref().map(field).unwrap_or_default()).collect::<Vec<_>>().join(","))?;
                        }
                        rows += page.grid.rows.len();
                        offset += 1000;
                        if page.grid.rows.len() < 1000 || rows >= 100_000 || page.total.is_some_and(|t| t as usize <= offset) || conn.kind == Kind::RabbitMq {
                            break;
                        }
                    }
                    out.flush()?;
                    Ok(Some((file, rows)))
                })
                .await,
            };
            this.update(cx, |v, cx| {
                if let Body::Data(d) = &mut v.body {
                    d.exporting = false;
                }
                match result {
                    Ok(Some((file, rows))) => cx.emit(Request::Message(format!("Exported {rows} rows to {}", file.display()))),
                    Ok(None) => {}
                    Err(e) => cx.emit(Request::Error(format!("Export failed: {e}"))),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// RabbitMQ: shows up to 10 messages of the queue, which stay in it.
    fn peek_messages(&mut self, cx: &mut Context<Self>) {
        let Body::Data(d) = &mut self.body else { return };
        let [_, vhost, name] = d.path.as_slice() else { return };
        let q = format!(
            "POST /api/queues/{}/{}/get\n{{\"count\": 10, \"ackmode\": \"ack_requeue_true\", \"encoding\": \"auto\", \"truncate\": 50000}}",
            crate::db_view::enc(vhost),
            crate::db_view::enc(name)
        );
        d.query.update(cx, |e, cx| e.set_text(&q, cx));
        d.inner = Inner::Query;
        self.run_query(&RunQuery, &mut None, cx);
    }

    // ------------------------------------------------------------ query

    /// ⌘↵: runs what's on show (a MongoDB collection's find or pipeline, else the query).
    fn run(&mut self, _: &RunQuery, _: &mut Window, cx: &mut Context<Self>) {
        if let Body::Data(d) = &self.body
            && d.inner == Inner::DataView
            && let Some(c) = &d.compass
        {
            match c.tab {
                CTab::Documents => return self.find(cx),
                CTab::Aggregations => return self.run_pipeline(cx),
                _ => return,
            }
        }
        self.run_query(&RunQuery, &mut None, cx);
    }

    fn run_query(&mut self, _: &RunQuery, _: &mut Option<()>, cx: &mut Context<Self>) {
        self.run_query_checked(false, cx);
    }

    /// Asks first when the query changes or deletes data (unless `confirmed`).
    fn run_query_checked(&mut self, confirmed: bool, cx: &mut Context<Self>) {
        let Body::Data(d) = &mut self.body else { return };
        if d.running.is_some() {
            return;
        }
        d.inner = Inner::Query;
        // The selection if there is one, else everything.
        let ed = d.query.read(cx);
        let sel = ed.buf.sel.range();
        let text = if sel.is_empty() { ed.text() } else { ed.buf.slice(sel) };
        if !confirmed && let Some(what) = destructive(d.conn.kind, &text) {
            let weak = cx.entity().downgrade();
            return confirm_destructive(&what, &d.conn.title(), weak, |v, cx| v.run_query_checked(true, cx), cx);
        }
        // Tables or collections made, dropped or renamed: the suggestions learn them again after.
        let lower = text.to_lowercase();
        let reshapes = ["create ", "drop", "rename", "alter ", "createcollection"].iter().any(|w| lower.contains(w));
        let scope = scope_of(d.conn.kind, &d.path);
        let work = engine::run(engine::query(d.conn.clone(), scope, text));
        d.running = Some(Instant::now());
        d.query_error = None;
        // The elapsed time ticks while it runs.
        d.tick = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(250)).await;
                let going = this.update(cx, |v, cx| {
                    cx.notify();
                    matches!(&v.body, Body::Data(d) if d.running.is_some())
                });
                if !matches!(going, Ok(true)) {
                    break;
                }
            }
        }));
        d.query_task = Some(cx.spawn(async move |this, cx| {
            let r = work.await;
            this.update(cx, |v, cx| {
                if let Body::Data(d) = &mut v.body {
                    d.took = d.running.take().map(|t| t.elapsed());
                    match r {
                        Ok(grids) => {
                            for g in &grids {
                                learn_fields(&d.names, g);
                            }
                            d.results = grids.into_iter().map(GridView::new).collect();
                            // The last result with rows is the interesting one.
                            d.result_ix = d.results.iter().rposition(|g| !g.grid.columns.is_empty()).unwrap_or(d.results.len().saturating_sub(1));
                        }
                        Err(e) => d.query_error = Some(e),
                    }
                }
                if reshapes {
                    v.learn_names(cx);
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// The grid on show: (which, its open row).
    fn shown_grid(&self) -> Option<(Option<usize>, Option<usize>)> {
        let Body::Data(d) = &self.body else { return None };
        match d.inner {
            Inner::DataView => d.grid.as_ref().map(|g| (None, g.open)),
            Inner::Query => d.results.get(d.result_ix).map(|g| (Some(d.result_ix), g.open)),
        }
    }

    fn row_up(&mut self, _: &RowUp, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((which, Some(r))) = self.shown_grid() {
            self.open_row(which, r.saturating_sub(1), cx);
        }
    }

    fn row_down(&mut self, _: &RowDown, _: &mut Window, cx: &mut Context<Self>) {
        match self.shown_grid() {
            Some((which, Some(r))) => self.open_row(which, r + 1, cx),
            Some((which, None)) => self.open_row(which, 0, cx),
            None => {}
        }
    }

    /// Esc: closes the menu, else the open row.
    fn close_detail(&mut self, _: &CloseDetail, _: &mut Window, cx: &mut Context<Self>) {
        let Body::Data(d) = &mut self.body else { return };
        if d.menu.take().is_some() {
            return cx.notify();
        }
        if let Some(c) = d.compass.as_deref_mut()
            && (c.history || c.explain.is_some())
        {
            c.history = false;
            c.explain = None;
            return cx.notify();
        }
        if let Some((which, Some(r))) = self.shown_grid()
            && let Some(gv) = self.grid_of_mut(which)
        {
            gv.open = None;
            gv.list.splice(r..r + 1, 1);
            cx.notify();
        }
    }

    /// MongoDB: List, JSON or Table (every grid's rows are measured again).
    fn set_doc_mode(&mut self, mode: DocMode, cx: &mut Context<Self>) {
        // A key or value being typed is kept; a document typed as JSON becomes its change, so
        // every view shows it (with Cancel and Update) until it's saved or dropped.
        if self.editing.is_some() && !self.commit_edit_quietly(cx) {
            return; // fix what's typed first (the bar says why)
        }
        let mut carried: Vec<(Option<usize>, usize, mongodb::bson::Document)> = Vec::new();
        if let Body::Data(d) = &self.body
            && let Some(g) = &d.grid
        {
            for (r, j) in &g.json_edits {
                let text = j.editor.read(cx).text();
                if text != j.initial
                    && let Ok(doc) = compass::parse_document(&text)
                {
                    carried.push((None, *r, doc));
                }
            }
        }
        // Opened and not typed in: just closed.
        if let Body::Data(d) = &mut self.body
            && let Some(g) = &mut d.grid
        {
            let untouched: Vec<usize> = g.json_edits.iter().filter(|(_, j)| j.editor.read(cx).text() == j.initial).map(|(r, _)| *r).collect();
            for r in untouched {
                g.json_edits.remove(&r);
            }
        }
        for (which, r, doc) in carried {
            if let Some(g) = self.grid_of_mut(which) {
                g.json_edits.remove(&r);
            }
            self.edit_op(which, r, compass::Op::Replace(doc), None, cx);
        }
        let Body::Data(d) = &mut self.body else { return };
        d.doc_mode = mode;
        self.sel.reset();
        let agg = d.compass.as_deref_mut().and_then(|c| c.agg.as_mut());
        for gv in d.grid.iter_mut().chain(d.results.iter_mut()).chain(agg) {
            // The same document stays at the top (the rows are measured again for the new view).
            let top = gv.list.logical_scroll_top().item_ix;
            gv.list.reset(gv.grid.rows.len());
            gv.list.scroll_to(gpui::ListOffset { item_ix: top, offset_in_item: px(0.) });
        }
        cx.notify();
    }

    /// Refresh, or Stop while reading (the rows shown stay).
    fn stop_or_load(&mut self, cx: &mut Context<Self>) {
        if let Body::Data(d) = &mut self.body
            && d.loading
        {
            d.task = None; // dropping it stops the work
            d.loading = false;
            d.spec = d.grid.as_ref().map(|_| d.loaded_spec.clone()).unwrap_or_else(|| d.spec.clone());
            return cx.notify();
        }
        self.learn_names(cx);
        self.load(cx);
    }

    /// Run, or Stop while running.
    fn stop_or_run(&mut self, cx: &mut Context<Self>) {
        if let Body::Data(d) = &mut self.body
            && d.running.is_some()
        {
            d.query_task = None;
            d.tick = None;
            d.took = d.running.take().map(|t| t.elapsed());
            d.query_error = Some("Stopped".into());
            return cx.notify();
        }
        self.run_query(&RunQuery, &mut None, cx);
    }

    /// ⌘A: every row shown (the page), as ⌘C copies it: tab-separated in a table, documents as text.
    fn select_all_rows(&mut self, _: &SelectAllRows, _: &mut Window, cx: &mut Context<Self>) {
        let Some((which, _)) = self.shown_grid() else { return };
        let Some(gv) = self.grid_of(which) else { return };
        let g = &gv.grid;
        let text = match (self.doc_mode(gv), &g.trees) {
            (DocMode::Table, _) | (_, None) => std::iter::once(g.columns.join("\t")).chain(g.rows.iter().map(|r| r.iter().map(|c| c.clone().unwrap_or_default()).collect::<Vec<_>>().join("\t"))).collect::<Vec<_>>().join("\n"),
            (mode, Some(trees)) => trees.iter().map(|t| t.text(if mode == DocMode::Json { TextStyle::Json } else { TextStyle::Shell })).collect::<Vec<_>>().join("\n"),
        };
        self.sel.select_all(text);
        cx.notify();
    }

    /// ⌘C: the text dragged over, else the selected cell.
    fn copy_cell(&mut self, _: &CopyCell, _: &mut Window, cx: &mut Context<Self>) {
        let dragged = self.sel.text();
        if !dragged.is_empty() {
            return cx.write_to_clipboard(ClipboardItem::new_string(dragged));
        }
        let Body::Data(d) = &self.body else { return };
        let gv = if d.inner == Inner::Query { d.results.get(d.result_ix) } else { d.grid.as_ref() };
        if let Some((_, v)) = gv.and_then(|g| g.selected_text()) {
            cx.write_to_clipboard(ClipboardItem::new_string(v.unwrap_or_else(|| "NULL".into())));
        }
    }
}

/// `/` → `%2F` (RabbitMQ paths).
pub fn enc(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// Where a tab's queries run: its database (and MongoDB collection).
fn scope_of(kind: Kind, path: &[String]) -> Scope {
    match kind {
        Kind::Postgres | Kind::SqlServer | Kind::Redis => Scope { database: path.first().cloned(), collection: None },
        Kind::MongoDb => Scope { database: path.first().cloned(), collection: path.get(1).cloned() },
        Kind::RabbitMq => Scope::default(),
    }
}

#[cfg(test)]
impl DbView {
    fn field(&self, name: &str) -> Option<Entity<Editor>> {
        let Body::Form(f) = &self.body else { return None };
        Some(match name {
            "name" => f.name.clone(),
            "group" => f.group.clone(),
            "host" => f.host.clone(),
            "port" => f.port.clone(),
            "user" => f.user.clone(),
            "password" => f.password.clone(),
            "database" => f.database.clone(),
            "url" => f.url.clone(),
            _ => return None,
        })
    }

    pub fn form_text(&self, name: &str, cx: &App) -> String {
        self.field(name).map(|e| e.read(cx).text()).unwrap_or_default()
    }

    pub fn fill(&mut self, name: &str, text: &str, cx: &mut Context<Self>) {
        if let Some(e) = self.field(name) {
            e.update(cx, |e, cx| e.set_text(text, cx));
        }
    }

    pub fn form_status(&self) -> Option<Result<String, String>> {
        match &self.body {
            Body::Form(f) => f.status.clone(),
            _ => None,
        }
    }

    /// The Data View's rows shown and total.
    pub fn data_summary(&self) -> Option<(Vec<String>, usize, Option<u64>)> {
        let Body::Data(d) = &self.body else { return None };
        let g = d.grid.as_ref()?;
        let total = match &d.page {
            Some(Ok(p)) => p.total,
            _ => None,
        };
        Some((g.grid.columns.clone(), g.grid.rows.len(), total))
    }

    pub fn query_result(&self) -> Option<Result<(Vec<String>, usize), String>> {
        let Body::Data(d) = &self.body else { return None };
        if let Some(e) = &d.query_error {
            return Some(Err(e.clone()));
        }
        let g = d.results.get(d.result_ix)?;
        Some(Ok((g.grid.columns.clone(), g.grid.rows.len())))
    }

    /// The open row, and how documents show.
    pub fn detail_state(&self, _cx: &App) -> (bool, Option<usize>, Option<DocMode>) {
        let Body::Data(d) = &self.body else { return (false, None, None) };
        let gv = if d.inner == Inner::Query { d.results.get(d.result_ix) } else { d.grid.as_ref() };
        let open = gv.and_then(|g| g.open);
        (open.is_some(), open, gv.filter(|g| g.grid.trees.is_some()).map(|_| d.doc_mode))
    }

    /// A shown document's text (MongoDB), as copied from its card.
    pub fn doc_text(&self, row: usize, style: TextStyle) -> Option<String> {
        let Body::Data(d) = &self.body else { return None };
        d.grid.as_ref()?.grid.trees.as_ref()?.get(row).map(|t| t.text(style))
    }

    /// The first document on screen in the Data View.
    #[cfg(test)]
    pub fn docs_scroll_top(&self) -> usize {
        match &self.body {
            Body::Data(d) => d.grid.as_ref().map(|g| g.list.logical_scroll_top().item_ix).unwrap_or(0),
            _ => 0,
        }
    }

    #[cfg(test)]
    pub fn scroll_docs_to_top(&mut self) {
        if let Body::Data(d) = &mut self.body
            && let Some(g) = &d.grid
        {
            g.list.scroll_to(gpui::ListOffset::default());
        }
    }

    pub fn set_query(&mut self, text: &str, cx: &mut Context<Self>) {
        if let Body::Data(d) = &self.body {
            let q = d.query.clone();
            q.update(cx, |e, cx| e.set_text(text, cx));
        }
    }

    pub fn search_for(&mut self, text: &str, cx: &mut Context<Self>) {
        if let Body::Data(d) = &self.body {
            let s = d.search.clone();
            s.update(cx, |e, cx| e.set_text(text, cx));
        }
        self.search_now(cx);
    }
}

// ---------------------------------------------------------------- the connect form

impl Form {
    fn new(conn: Conn, saved: bool, project: Option<PathBuf>, subs: &mut Vec<Subscription>, window: &mut Window, cx: &mut Context<DbView>) -> Form {
        let password = input("Password", "", cx);
        password.update(cx, |e, _| e.secret = true);
        let f = Form {
            name: input("Connection name", &conn.name, cx),
            group: input("Parent/Sub", &conn.group, cx),
            host: input("Host", &conn.host, cx),
            port: input("Port", &conn.port.to_string(), cx),
            user: input("Username", &conn.user, cx),
            password,
            database: input("", &conn.database, cx),
            url: input("", "", cx),
            project: conn.project.clone().or(project),
            conn,
            saved,
            status: None,
            busy: false,
            task: None,
        };
        // Enter in any field connects.
        for e in [&f.name, &f.group, &f.host, &f.port, &f.user, &f.password, &f.database, &f.url] {
            subs.push(cx.subscribe_in(e, window, |v: &mut DbView, _, ev: &EditorEvent, window, cx| {
                if matches!(ev, EditorEvent::Submit) {
                    v.connect(window, cx);
                }
            }));
        }
        f
    }

    /// The settings as typed, and the secret (password or connection string; None: unchanged).
    fn read(&self, cx: &App) -> Result<(Conn, Option<String>), String> {
        let t = |e: &Entity<Editor>| e.read(cx).text().trim().to_string();
        let mut c = self.conn.clone();
        c.name = t(&self.name);
        c.group = t(&self.group).trim_matches('/').to_string();
        c.host = t(&self.host);
        c.user = t(&self.user);
        c.database = t(&self.database);
        c.port = match t(&self.port).parse::<u16>() {
            Ok(p) if p > 0 => p,
            _ => return Err("The port should be a number from 1 to 65535".into()),
        };
        let secret = if c.use_url { self.url.read(cx).text().trim().to_string() } else { self.password.read(cx).text() };
        if c.use_url && secret.is_empty() {
            return Err("Paste the connection string".into());
        }
        if !c.use_url && c.host.is_empty() {
            return Err("The host is needed".into());
        }
        if c.kind == Kind::Redis && c.database.parse::<u32>().is_err() {
            return Err("The Redis database is a number (0 to 15 usually)".into());
        }
        // A saved password stays when the box is left empty.
        let secret = if self.saved && secret.is_empty() && !c.use_url { None } else { Some(secret) };
        Ok((c, secret))
    }

    fn set_kind(&mut self, kind: Kind, cx: &mut App) {
        let old = self.conn.kind;
        if old == kind {
            return;
        }
        // Fields still at the old type's defaults take the new one's.
        let swap = |e: &Entity<Editor>, from: &str, to: &str, cx: &mut App| {
            if e.read(cx).text().trim() == from {
                e.update(cx, |e, cx| e.set_text(to, cx));
            }
        };
        swap(&self.port, &old.default_port().to_string(), &kind.default_port().to_string(), cx);
        swap(&self.user, old.default_user(), kind.default_user(), cx);
        swap(&self.database, old.default_database(), kind.default_database(), cx);
        self.conn.kind = kind;
        if !matches!(kind, Kind::Postgres | Kind::SqlServer | Kind::MongoDb) {
            self.conn.use_url = false;
        }
        self.status = None;
    }
}

impl DbView {
    fn form_mut(&mut self) -> Option<&mut Form> {
        match &mut self.body {
            Body::Form(f) => Some(f),
            _ => None,
        }
    }

    fn test(&mut self, cx: &mut Context<Self>) {
        let Some(f) = self.form_mut() else { return };
        let (conn, secret) = match f.read(cx) {
            Ok(v) => v,
            Err(e) => {
                f.status = Some(Err(e));
                return cx.notify();
            }
        };
        f.busy = true;
        f.status = None;
        let work = engine::run(async move {
            let secret = match secret {
                Some(s) => s,
                None => engine::secret(conn.id.clone()).await?,
            };
            engine::test(conn, secret).await
        });
        f.task = Some(cx.spawn(async move |this, cx| {
            let r = work.await;
            this.update(cx, |v, cx| {
                if let Some(f) = v.form_mut() {
                    f.busy = false;
                    f.status = Some(r.map(|v| format!("Connected · {v}")));
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Saves; Some(id) when it did.
    fn save(&mut self, cx: &mut Context<Self>) -> Option<String> {
        let f = self.form_mut()?;
        let (conn, secret) = match f.read(cx) {
            Ok(v) => v,
            Err(e) => {
                f.status = Some(Err(e));
                cx.notify();
                return None;
            }
        };
        match db::save(&conn, secret.as_deref()) {
            Ok(()) => {
                f.conn = conn.clone();
                f.saved = true;
                f.status = Some(Ok("Saved".into()));
                self.open = Open::Edit(conn.id.clone());
                cx.emit(Request::Db(DbRequest::Changed { expand: None }));
                cx.notify();
                Some(conn.id)
            }
            Err(e) => {
                f.status = Some(Err(e));
                cx.notify();
                None
            }
        }
    }

    /// Deletes the saved connection (asking first), then closes the form.
    fn ask_delete(&mut self, cx: &mut Context<Self>) {
        let Some(f) = self.form_mut() else { return };
        let (id, name) = (f.conn.id.clone(), f.conn.title());
        let weak = cx.entity().downgrade();
        cx.emit(Request::Ask(crate::dialog::Ask {
            title: format!("Delete the connection “{name}”?"),
            detail: "Its saved settings and password are removed. The database itself isn't touched.".into(),
            buttons: vec!["Delete".into()],
            danger: true,
            cancel_default: false,
            cancel_label: None,
            on_answer: std::rc::Rc::new(move |_, _, cx: &mut App| {
                db::delete(&id).ok();
                if let Some(v) = weak.upgrade() {
                    v.update(cx, |_, cx| {
                        cx.emit(Request::Db(DbRequest::Changed { expand: None }));
                        cx.emit(Request::Db(DbRequest::Close));
                    });
                }
            }),
        }));
    }

    /// Saves, opens it in the panel, and closes the form.
    fn connect(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.save(cx) {
            cx.emit(Request::Db(DbRequest::Changed { expand: Some(id) }));
            cx.emit(Request::Db(DbRequest::Close));
        }
    }
}

// ---------------------------------------------------------------- drawing

fn button(id: impl Into<SharedString>, label: &str, icon: Option<&str>, primary: bool) -> gpui::Stateful<gpui::Div> {
    let id: SharedString = id.into();
    let sel = id.to_string();
    div()
        .id(id)
        .debug_selector(move || sel.clone())
        .flex()
        .flex_none()
        .items_center()
        .gap(px(5.))
        .h(px(26.))
        .px(px(10.))
        .rounded(px(4.))
        .cursor_pointer()
        .text_size(px(12.))
        .when(primary, |d| d.bg(theme::accent()).text_color(theme::on(theme::accent())).hover(|s| s.bg(theme::accent_hover())))
        .when(!primary, |d| d.bg(theme::bg_input()).border_1().border_color(theme::border_input()).text_color(theme::fg()).hover(|s| s.bg(theme::hover())))
        .when_some(icon, |d, i| d.child(ui::small_icon(i, if primary { theme::on(theme::accent()) } else { theme::fg() })))
        .child(label.to_string())
}

fn field_box(e: &Entity<Editor>, w: f32) -> gpui::Div {
    div().w(px(w)).h(px(26.)).px(px(6.)).flex().items_center().rounded(px(4.)).bg(theme::bg_input()).border_1().border_color(theme::border_input()).child(e.clone())
}

fn switch(id: &'static str, on: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .debug_selector(move || id.to_string())
        .flex()
        .flex_none()
        .items_center()
        .w(px(30.))
        .h(px(16.))
        .px(px(2.))
        .rounded(px(8.))
        .cursor_pointer()
        .bg(if on { theme::accent() } else { theme::badge() })
        .when(on, |d| d.justify_end())
        .child(div().size(px(12.)).rounded(px(6.)).bg(theme::c(0xffffff)))
}

fn label(text: &str) -> gpui::Div {
    div().w(px(130.)).flex_none().text_color(theme::fg_dim()).child(text.to_string())
}

/// A type's icon, in its color.
pub fn kind_badge(kind: Kind) -> gpui::Svg {
    ui::icon(&format!("db-{}", kind.id()), theme::c(kind.color()))
}

impl DbView {
    fn render_form(&self, f: &Form, cx: &mut Context<Self>) -> gpui::AnyElement {
        let kind = f.conn.kind;
        let row = || div().flex().items_center().gap(px(10.)).min_h(px(30.));
        let types = div().flex().flex_wrap().gap(px(6.)).children(Kind::ALL.map(|k| {
            let on = k == kind;
            let id = format!("db-type-{}", k.id());
            div()
                .id(SharedString::from(id.clone()))
                .debug_selector(move || id.clone())
                .flex()
                .items_center()
                .gap(px(6.))
                .h(px(28.))
                .px(px(10.))
                .rounded(px(4.))
                .cursor_pointer()
                .border_1()
                .border_color(if on { theme::focus_border() } else { theme::border_input() })
                .when(on, |d| d.bg(theme::selected_focused()))
                .when(!on, |d| d.hover(|s| s.bg(theme::hover())))
                .on_click(cx.listener(move |v, _, _, cx| {
                    if let Some(f) = v.form_mut() {
                        f.set_kind(k, cx);
                    }
                    cx.notify();
                }))
                .child(kind_badge(k))
                .child(div().text_color(if on { theme::fg_bright() } else { theme::fg() }).child(k.label()))
        }));
        let scope_choice = |id: &'static str, text: &'static str, on: bool, enabled: bool, workspace: bool, cx: &mut Context<Self>| {
            div()
                .id(id)
                .debug_selector(move || id.to_string())
                .px(px(12.))
                .py(px(4.))
                .rounded(px(4.))
                .text_size(px(12.))
                .when(on, |d| d.bg(theme::accent()).text_color(theme::on(theme::accent())))
                .when(!on && enabled, |d| d.cursor_pointer().text_color(theme::fg()).hover(|s| s.bg(theme::hover())))
                .when(!enabled, |d| d.text_color(theme::fg_muted()))
                .when(enabled, |d| {
                    d.on_click(cx.listener(move |v, _, _, cx| {
                        if let Some(f) = v.form_mut() {
                            f.conn.project = if workspace { f.project.clone() } else { None };
                        }
                        cx.notify();
                    }))
                })
                .child(text)
        };
        let workspace = f.conn.project.is_some();
        let toggle = |id: &'static str, text: &'static str, on: bool, set: fn(&mut Conn, bool), cx: &mut Context<Self>| {
            div().flex().items_center().gap(px(8.)).child(div().text_color(theme::fg_dim()).child(text)).child(switch(id, on).on_click(cx.listener(move |v, _, _, cx| {
                if let Some(f) = v.form_mut() {
                    set(&mut f.conn, !on);
                    f.status = None;
                }
                cx.notify();
            })))
        };
        let url_ok = matches!(kind, Kind::Postgres | Kind::SqlServer | Kind::MongoDb);
        let (db_label, db_hint) = match kind {
            Kind::Postgres => ("Database", "postgres"),
            Kind::SqlServer => ("Database", "master"),
            Kind::MongoDb => ("Auth database", "admin"),
            Kind::Redis => ("Database number", "0"),
            Kind::RabbitMq => ("Virtual host", "/"),
        };
        f.database.update(cx, |e, _| e.placeholder = db_hint.into());
        let url_hint = match kind {
            Kind::Postgres => "postgresql://user:password@host:5432/database?sslmode=require",
            Kind::SqlServer => "Server=tcp:host,1433;Database=db;User Id=sa;Password=…;Encrypt=true",
            _ => "mongodb+srv://user:password@cluster.example.net/?retryWrites=true",
        };
        f.url.update(cx, |e, _| e.placeholder = url_hint.into());
        let note: Option<&str> = match kind {
            Kind::SqlServer => Some("Signs in with a SQL Server login (user name and password). Windows (NTLM) and Microsoft Entra sign-in aren't supported."),
            Kind::RabbitMq => Some("Uses RabbitMQ's management plugin (its HTTP API, port 15672 by default). If nothing answers, enable it on the server: rabbitmq-plugins enable rabbitmq_management"),
            Kind::MongoDb => Some("Queries are written like in mongosh: db.users.find({age: {$gt: 30}}), updateOne, aggregate([…])…"),
            Kind::Redis => Some("Leave Username empty unless the server uses ACL users. Queries are commands, one per line."),
            Kind::Postgres => None,
        };
        let status = f.status.clone();
        let tls_label = match kind {
            Kind::SqlServer => "Encrypt",
            Kind::RabbitMq => "HTTPS",
            Kind::Postgres => "SSL",
            _ => "TLS",
        };
        let fields = if f.conn.use_url && url_ok {
            div().flex().flex_col().gap(px(8.)).child(row().child(label("Connection string")).child(field_box(&f.url, 520.))).child(
                div().pl(px(140.)).text_size(px(11.)).text_color(theme::fg_muted()).child("Kept in your Keychain, like passwords."),
            )
        } else {
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(row().child(label("Host")).child(field_box(&f.host, 240.)).child(div().w(px(16.))).child(div().w(px(80.)).text_color(theme::fg_dim()).child(if kind == Kind::RabbitMq { "API port" } else { "Port" })).child(field_box(&f.port, 90.)))
                .child(
                    row()
                        .child(label("Username"))
                        .child(field_box(&f.user, 240.))
                        .child(div().w(px(16.)))
                        .child(div().w(px(80.)).text_color(theme::fg_dim()).child("Password"))
                        .child(field_box(&f.password, 200.)),
                )
                .when(f.saved, |d| d.child(div().pl(px(470.)).text_size(px(11.)).text_color(theme::fg_muted()).child("Saved in your Keychain: leave it to keep it")))
                .child(row().child(label(db_label)).child(field_box(&f.database, 240.)))
        };
        let options = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(22.))
            .pl(px(140.))
            .when(!(f.conn.use_url && url_ok), |d| {
                d.child(toggle("db-tls", tls_label, f.conn.tls, |c, on| c.tls = on, cx))
                    .when(f.conn.tls || kind == Kind::SqlServer, |d| d.child(toggle("db-trust", "Trust server certificate", f.conn.trust_cert, |c, on| c.trust_cert = on, cx)))
                    .when(kind == Kind::MongoDb, |d| d.child(toggle("db-direct", "Direct connection", f.conn.direct, |c, on| c.direct = on, cx)))
            })
            .when(url_ok, |d| d.child(toggle("db-url", "Use connection string", f.conn.use_url, |c, on| c.use_url = on, cx)));
        div()
            .id("db-form")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .justify_center()
            .child(
                div()
                    .w(px(760.))
                    .max_w_full()
                    .flex()
                    .flex_col()
                    .gap(px(14.))
                    .px(px(24.))
                    .py(px(26.))
                    .text_size(px(13.))
                    .child(div().flex().items_center().gap(px(10.)).child(ui::icon("database", theme::accent()).size(px(26.))).child(div().text_size(px(20.)).text_color(theme::fg_bright()).child(if f.saved { "Edit connection" } else { "Connect to server" })))
                    .child(
                        row()
                            .child(label("Name"))
                            .child(field_box(&f.name, 200.))
                            .child(div().w(px(6.)))
                            .child(div().text_color(theme::fg_dim()).child("Group"))
                            .child(field_box(&f.group, 140.))
                            .child(div().w(px(6.)))
                            .child(div().text_color(theme::fg_dim()).child("Scope"))
                            .child(
                                div()
                                    .flex()
                                    .p(px(2.))
                                    .rounded(px(5.))
                                    .border_1()
                                    .border_color(theme::border_input())
                                    .child(scope_choice("db-scope-global", "Global", !workspace, true, false, cx))
                                    .child(scope_choice("db-scope-workspace", "Workspace", workspace, f.project.is_some(), true, cx)),
                            ),
                    )
                    .child(div().pl(px(140.)).text_size(px(11.)).text_color(theme::fg_muted()).child(if workspace {
                        format!("Shown only in this workspace ({}).", f.project.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_default())
                    } else {
                        "Shown in every window.".to_string()
                    }))
                    .child(div().h(px(1.)).bg(theme::border()))
                    .child(div().text_color(theme::fg_dim()).child("Server type"))
                    .child(types)
                    .child(div().h(px(4.)))
                    .child(fields)
                    .child(options)
                    .when_some(note, |d, n| d.child(div().ml(px(140.)).p(px(8.)).rounded(px(4.)).bg(theme::bg_input()).text_size(px(12.)).text_color(theme::fg_dim()).child(n)))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .pl(px(140.))
                            .pt(px(6.))
                            .child(button("db-test", if f.busy { "Testing…" } else { "Test" }, Some("sync"), false).on_click(cx.listener(|v, _, _, cx| v.test(cx))))
                            .child(button("db-save", "Save", Some("check"), false).on_click(cx.listener(|v, _, _, cx| {
                                v.save(cx);
                            })))
                            .child(button("db-connect", "Connect", Some("plus"), true).on_click(cx.listener(|v, _, w, cx| v.connect(w, cx))))
                            .child(button("db-close", "Close", Some("close"), false).on_click(cx.listener(|_, _, _, cx| cx.emit(Request::Db(DbRequest::Close)))))
                            .when(f.saved, |el| el.child(div().flex_1()).child(button("db-delete", "Delete", Some("trash"), false).text_color(theme::git_deleted()).on_click(cx.listener(|v, _, _, cx| v.ask_delete(cx))))),
                    )
                    .when_some(status, |d, s| {
                        let (ok, text) = match s {
                            Ok(t) => (true, t),
                            Err(e) => (false, e),
                        };
                        d.child(
                            div()
                                .debug_selector(|| "db-form-status".into())
                                .ml(px(140.))
                                .flex()
                                .gap(px(6.))
                                .items_start()
                                .text_color(if ok { theme::git_added() } else { theme::git_deleted() })
                                .child(ui::small_icon(if ok { "check" } else { "error" }, if ok { theme::git_added() } else { theme::git_deleted() }))
                                // Long errors wrap, within the form.
                                .child(div().flex_1().min_w_0().child(text)),
                        )
                    }),
            )
            .into_any_element()
    }

    /// A grid of rows (Table) or, for MongoDB documents, cards (List / JSON). Rows open in
    /// place when clicked; only what's on screen is drawn.
    fn render_grid(&self, which: Option<usize>, gv: &GridView, sortable: bool, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mode = self.doc_mode(gv);
        // Only text drawn in this frame is under the pointer (rows scrolled away aren't).
        self.sel.begin_frame();
        // A drag past the top or bottom of the rows scrolls them, the selection following.
        let edge_scroll = {
            let (sel, list) = (self.sel.clone(), gv.list.clone());
            move |ev: &gpui::MouseMoveEvent, window: &mut Window, _: &mut App| {
                if !ev.dragging() || !sel.dragging() {
                    return;
                }
                let vb = list.viewport_bounds();
                let step = if ev.position.y < vb.top() + px(12.) {
                    -px(18.)
                } else if ev.position.y > vb.bottom() - px(12.) {
                    px(18.)
                } else {
                    return;
                };
                list.scroll_by(step);
                sel.drag_to(ev.position, window);
                window.refresh();
            }
        };
        let body = list(gv.list.clone(), cx.processor(move |v: &mut DbView, ix: usize, _w, cx| v.render_item(which, ix, cx))).with_sizing_behavior(gpui::ListSizingBehavior::Auto).flex_1();
        if mode != DocMode::Table {
            let sel = self.sel.clone();
            return div()
                .id(SharedString::from(format!("db-docs-{}", which.map(|i| i.to_string()).unwrap_or_default())))
                .debug_selector(|| "db-grid".into())
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                // A press beside the text (an empty part of a line) starts a selection there too.
                .on_mouse_down(MouseButton::Left, move |ev, window, _| sel.press(ev, window))
                .on_mouse_move(edge_scroll)
                .child(body)
                .into_any_element();
        }
        let g = gv.grid.clone();
        let total_w: f32 = gv.widths.iter().sum::<f32>() + 44.;
        let sort = match &self.body {
            Body::Data(d) if sortable => d.spec.sort.clone(),
            _ => None,
        };
        // Scrolled sideways: the row numbers and an id column stay on the left (drawn over the rest).
        let shift = -gv.hscroll.offset().x;
        let pin_first = pinned_first(&g.columns);
        let pinned_head = (shift > px(0.)).then(|| {
            div()
                .absolute()
                .top(px(0.))
                .left(shift)
                .h_full()
                .flex()
                .bg(theme::bg_side())
                .border_r_1()
                .border_color(theme::border())
                .child(div().w(px(44.)).flex_none())
                .when(pin_first, |d| {
                    d.child(div().w(px(gv.widths[0])).flex_none().flex().items_center().px(px(8.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::fg_bright()).overflow_hidden().whitespace_nowrap().child(g.columns[0].clone()))
                })
        });
        let header = div()
            .relative()
            .flex()
            .flex_none()
            .h(px(28.))
            .border_b_1()
            .border_color(theme::border())
            .bg(theme::bg_side())
            .child(div().w(px(44.)).flex_none())
            .children(g.columns.iter().enumerate().map(|(i, c)| {
                let arrow = match &sort {
                    Some((s, false)) if s == c => " ↑",
                    Some((s, true)) if s == c => " ↓",
                    _ if sortable => " ↕",
                    _ => "",
                };
                let col = c.clone();
                let id = format!("db-col-{i}");
                let ty = gv.types.get(i).cloned().unwrap_or_default();
                div()
                    .id(SharedString::from(id.clone()))
                    .debug_selector(move || id.clone())
                    .flex_basis(px(gv.widths[i]))
                    .flex_grow(1.)
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .px(px(8.))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .when(sortable, |d| d.cursor_pointer().hover(|s| s.bg(theme::hover())).on_click(cx.listener(move |v, _, _, cx| v.sort_by(col.clone(), cx))))
                    .child(div().min_w_0().overflow_hidden().text_ellipsis().font_weight(FontWeight::SEMIBOLD).text_color(theme::fg_bright()).child(c.clone()))
                    .when(!ty.is_empty(), |d| d.child(div().flex_none().text_size(px(11.)).text_color(theme::fg_muted()).child(ty)))
                    .child(div().flex_none().text_color(theme::fg_muted()).child(arrow))
                    // Its right edge: dragged, the column's width; double-clicked, its width as at first.
                    .relative()
                    .child(
                        div()
                            .id(("db-col-resize", i))
                            .debug_selector(move || format!("db-col-resize-{i}"))
                            .absolute()
                            .top(px(0.))
                            .right(px(0.))
                            .w(px(6.))
                            .h_full()
                            .cursor(gpui::CursorStyle::ResizeLeftRight)
                            .hover(|s| s.bg(theme::focus_border()))
                            .on_mouse_down(MouseButton::Left, cx.listener(move |v, ev: &gpui::MouseDownEvent, _, cx| {
                                cx.stop_propagation();
                                let Some(gv) = v.grid_of_mut(which) else { return };
                                if ev.click_count == 2 {
                                    gv.widths[i] = gv.auto_widths[i];
                                    v.col_drag = None;
                                } else {
                                    v.col_drag = Some((which, i, ev.position.x, gv.widths[i]));
                                }
                                cx.notify();
                            }))
                            .on_click(|_, _, cx| cx.stop_propagation()),
                    )
            }))
            .children(pinned_head);
        div()
            .id(SharedString::from(format!("db-hscroll-{}", which.map(|i| i.to_string()).unwrap_or_default())))
            .debug_selector(|| "db-grid".into())
            .flex_1()
            .min_h_0()
            .overflow_x_scroll()
            // Up and down scroll the rows, never the columns sideways.
            .restrict_scroll_to_axis()
            .track_scroll(&gv.hscroll)
            .on_mouse_down(MouseButton::Left, { let sel = self.sel.clone(); move |ev, window, _| sel.press(ev, window) })
            .on_mouse_move(edge_scroll)
            // At least as wide as the pane: few columns share its width.
            .child(div().min_w(px(total_w)).w_full().h_full().flex().flex_col().text_size(px(12.5)).child(header).child(body))
            .into_any_element()
    }

    /// How a grid's rows show: MongoDB documents as chosen, the rest as a table.
    fn doc_mode(&self, gv: &GridView) -> DocMode {
        match &self.body {
            Body::Data(d) if gv.grid.trees.is_some() => d.doc_mode,
            _ => DocMode::Table,
        }
    }

    fn grid_of(&self, which: Option<usize>) -> Option<&GridView> {
        let Body::Data(d) = &self.body else { return None };
        match which {
            Some(AGG) => d.compass.as_ref().and_then(|c| c.agg.as_ref()),
            Some(i) => d.results.get(i),
            None => d.grid.as_ref(),
        }
    }

    fn grid_of_mut(&mut self, which: Option<usize>) -> Option<&mut GridView> {
        let Body::Data(d) = &mut self.body else { return None };
        match which {
            Some(AGG) => d.compass.as_deref_mut().and_then(|c| c.agg.as_mut()),
            Some(i) => d.results.get_mut(i),
            None => d.grid.as_mut(),
        }
    }

    /// Opens a row in place (or closes it, if it's the open one).
    fn toggle_row(&mut self, which: Option<usize>, row: usize, col: Option<usize>, cx: &mut Context<Self>) {
        let Some(gv) = self.grid_of_mut(which) else { return };
        if row >= gv.grid.rows.len() {
            return;
        }
        let before = gv.open;
        gv.open = if before == Some(row) && col == gv.sel.map(|s| s.1) { None } else { Some(row) };
        gv.sel = Some((row, col.or(gv.sel.map(|s| s.1)).unwrap_or(0)));
        for r in [before, gv.open].into_iter().flatten() {
            gv.list.splice(r..r + 1, 1);
        }
        cx.notify();
    }

    /// Opens another row (↑ ↓), keeping it in view.
    fn open_row(&mut self, which: Option<usize>, row: usize, cx: &mut Context<Self>) {
        let Some(gv) = self.grid_of_mut(which) else { return };
        if row >= gv.grid.rows.len() {
            return;
        }
        let before = gv.open;
        let top = gv.list.logical_scroll_top();
        gv.open = Some(row);
        gv.sel = Some((row, gv.sel.map(|s| s.1).unwrap_or(0)));
        for r in [before, gv.open].into_iter().flatten() {
            gv.list.splice(r..r + 1, 1);
        }
        match before {
            // The open section stays where it is on screen; the rows move through it.
            Some(b) if b != row => {
                let item_ix = (top.item_ix as isize + row as isize - b as isize).clamp(0, gv.grid.rows.len().saturating_sub(1) as isize) as usize;
                gv.list.scroll_to(gpui::ListOffset { item_ix, offset_in_item: top.offset_in_item });
            }
            _ => gv.list.scroll_to_reveal_item(row),
        }
        cx.notify();
    }

    // ------------------------------------------------------------ editing documents in place

    /// The row's changes (begun with its document as shown).
    fn doc_edit(&mut self, which: Option<usize>, row: usize) -> Option<&mut DocEdit> {
        let gv = self.grid_of_mut(which)?;
        let t = gv.grid.trees.as_ref()?.get(row)?.clone();
        Some(gv.edits.entry(row).or_insert_with(|| DocEdit { original: t.clone(), working: t, ops: Vec::new(), changed: HashSet::new(), saving: false, error: None }))
    }

    /// Redraws a card (its height may change).
    fn remeasure(&mut self, which: Option<usize>, row: usize, cx: &mut Context<Self>) {
        if let Some(gv) = self.grid_of_mut(which) {
            gv.list.splice(row..row + 1, 1);
        }
        cx.notify();
    }

    /// Applies a change to a card's document; `mark` is the field to highlight.
    fn edit_op(&mut self, which: Option<usize>, row: usize, op: compass::Op, mark: Option<Vec<Seg>>, cx: &mut Context<Self>) -> bool {
        if let Some(t) = self.doc_tree(which, row)
            && let Err(e) = compass::editable(&t)
        {
            cx.emit(Request::Error(e));
            return false;
        }
        let Some(e) = self.doc_edit(which, row) else { return false };
        let ok = match compass::apply_tree(&mut e.working, &op) {
            Ok(()) => {
                // Inserting or removing in an array moves the highlights after it; keep it simple: forget those.
                if matches!(op, compass::Op::Remove(_) | compass::Op::Insert(..)) {
                    let parent: Vec<Seg> = match &op {
                        compass::Op::Remove(p) => p[..p.len().saturating_sub(1)].to_vec(),
                        compass::Op::Insert(p, ..) => p.clone(),
                        _ => Vec::new(),
                    };
                    e.changed.retain(|c| !(c.len() > parent.len() && c.starts_with(&parent) && matches!(c[parent.len()], Seg::Idx(_))));
                }
                if let Some(m) = mark {
                    e.changed.insert(m);
                }
                e.ops.push(op);
                e.error = None;
                // Back to the document as loaded: nothing to save, nothing highlighted.
                if e.working == e.original {
                    e.ops.clear();
                    e.changed.clear();
                }
                true
            }
            Err(err) => {
                e.error = Some(err);
                false
            }
        };
        self.remeasure(which, row, cx);
        ok
    }

    /// Double-click: types into a key (or value) in place.
    fn start_edit(&mut self, which: Option<usize>, row: usize, path: Vec<Seg>, key: bool, fresh: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_edit(window, cx);
        if let Some(t) = self.doc_tree(which, row)
            && let Err(e) = compass::editable(&t)
        {
            return cx.emit(Request::Error(e));
        }
        // A string too long to show whole (it ends in …): edited as JSON, where it's whole.
        let long = self.grid_of(which).and_then(|g| g.edits.get(&row).map(|e| e.working.clone()).or_else(|| g.grid.trees.as_ref()?.get(row).cloned())).and_then(|t| {
            let mut t = Some(t);
            for s in &path {
                t = match (t, s) {
                    (Some(Tree::Object(f)), Seg::Key(k)) => f.into_iter().find(|(n, _)| n == k).map(|(_, v)| v),
                    (Some(Tree::Array(a)), Seg::Idx(i)) => a.into_iter().nth(*i),
                    _ => None,
                };
            }
            t
        });
        if !key && matches!(&long, Some(Tree::Str(s)) if s.len() > 100_000) {
            return cx.emit(Request::Error("This value is too long to edit here: edit the document as JSON (the pencil, in the JSON view)".into()));
        }
        let Some(e) = self.doc_edit(which, row) else { return };
        let mut ty = "String";
        let text = match (key, path.last()) {
            (true, Some(Seg::Key(k))) => k.clone(),
            (true, _) => return,
            (false, _) => {
                let mut t = Some(&e.working);
                for s in &path {
                    t = match (t, s) {
                        (Some(Tree::Object(f)), Seg::Key(k)) => f.iter().find(|(n, _)| n == k).map(|(_, v)| v),
                        (Some(Tree::Array(a)), Seg::Idx(i)) => a.get(*i),
                        _ => None,
                    };
                }
                if let Some(t) = t {
                    ty = compass::type_of(t);
                }
                t.map(compass::edit_text).unwrap_or_default()
            }
        };
        let editor = input("", &text, cx);
        editor.update(cx, |ed, cx| {
            ed.pairs = true;
            ed.buf.select_all();
            cx.notify();
        });
        window.focus(&editor.read(cx).focus_target(cx), cx);
        let sub = cx.subscribe_in(&editor, window, |v: &mut DbView, _, ev: &EditorEvent, window, cx| match ev {
            EditorEvent::Submit => v.commit_edit(window, cx),
            EditorEvent::Cancel => v.cancel_edit(cx),
            _ => {}
        });
        self.editing = Some(Editing { which, row, path, key, fresh, editor, _sub: sub, ty, ty_chosen: false, menu: false, anchor_col: self.edit_anchor });
        self.remeasure(which, row, cx);
    }

    /// Enter: the typed key or value goes into the document.
    fn commit_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ed) = self.editing.take() else { return };
        let text = ed.editor.read(cx).text();
        let (which, row) = (ed.which, ed.row);
        if ed.key {
            let new = text.trim().to_string();
            let Some(Seg::Key(old)) = ed.path.last().cloned() else { return };
            if new.is_empty() {
                if let Some(e) = self.doc_edit(which, row) {
                    e.error = Some("A field needs a name".into());
                }
                // Asked again.
                self.editing = Some(ed);
                return self.remeasure(which, row, cx);
            }
            let mut renamed = ed.path.clone();
            *renamed.last_mut().unwrap() = Seg::Key(new.clone());
            if new != old && !self.edit_op(which, row, compass::Op::Rename(ed.path.clone(), new), Some(renamed.clone()), cx) {
                self.editing = Some(ed);
                return;
            }
            // A new field: its value next.
            if ed.fresh {
                self.start_edit(which, row, renamed, false, true, window, cx);
            }
            return self.remeasure(which, row, cx);
        }
        let was = self.doc_edit(which, row).and_then(|e| {
            let mut t = Some(&e.working);
            for s in &ed.path {
                t = match (t, s) {
                    (Some(Tree::Object(f)), Seg::Key(k)) => f.iter().find(|(n, _)| n == k).map(|(_, v)| v),
                    (Some(Tree::Array(a)), Seg::Idx(i)) => a.get(*i),
                    _ => None,
                };
            }
            t.cloned()
        });
        // A field just added (empty) takes whatever is typed, read as a value if it is one.
        let prev = was.filter(|_| !ed.fresh);
        // The type picked (or the field's own); a new field's text is read as whatever value it is.
        let value = if ed.fresh && !ed.ty_chosen { compass::edit_value(&text, prev.as_ref()) } else { compass::typed_value(&text, ed.ty) };
        match value {
            Ok(v) => {
                let unchanged = prev.as_ref().is_some_and(|p| *p == crate::db::compass_tree(&v));
                if !unchanged {
                    self.edit_op(which, row, compass::Op::Set(ed.path.clone(), v), Some(ed.path.clone()), cx);
                }
            }
            Err(e) => {
                if let Some(d) = self.doc_edit(which, row) {
                    d.error = Some(e);
                }
                self.editing = Some(ed);
            }
        }
        self.remeasure(which, row, cx);
    }

    /// The value's type, a menu of them (Compass's) when clicked.
    fn type_picker(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(ed) = &self.editing else { return div().into_any_element() };
        if ed.key {
            return div().into_any_element();
        }
        let (ty, open) = (ed.ty, ed.menu);
        let menu = open.then(|| {
            deferred(
                div()
                    .id("db-type-menu")
                    .debug_selector(|| "db-type-menu".into())
                    .absolute()
                    .top(px(22.))
                    .right(px(0.))
                    .w(px(190.))
                    .max_h(px(320.))
                    .overflow_y_scroll()
                    .py(px(4.))
                    .rounded(px(5.))
                    .bg(theme::bg_popup())
                    .border_1()
                    .border_color(theme::border_input())
                    .shadow_lg()
                    .font_family(theme::UI_FONT)
                    .children(compass::TYPES.iter().map(|t| {
                        let t: &'static str = t;
                        div()
                            .id(SharedString::from(format!("db-type-{t}")))
                            .debug_selector(move || format!("db-type-{t}"))
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .px(px(10.))
                            .py(px(3.))
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::hover()))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |v, _, window, cx| {
                                if let Some(ed) = v.editing.as_mut() {
                                    ed.ty = t;
                                    ed.ty_chosen = true;
                                    ed.menu = false;
                                    window.focus(&ed.editor.read(cx).focus_target(cx), cx);
                                }
                                cx.notify();
                            }))
                            .child(div().w(px(12.)).text_color(theme::accent()).child(if t == ty { "✓" } else { "" }))
                            .child(div().when(t == ty, |d| d.font_weight(FontWeight::SEMIBOLD)).child(t))
                    })),
            )
            .with_priority(2)
        });
        div()
            .relative()
            .flex_none()
            .child(
                div()
                    .id("db-type-button")
                    .debug_selector(|| "db-type-button".into())
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .h(px(19.))
                    .px(px(6.))
                    .rounded(px(3.))
                    .bg(theme::bg_input())
                    .border_1()
                    .border_color(theme::border_input())
                    .cursor_pointer()
                    .font_family(theme::UI_FONT)
                    .text_size(px(11.5))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|v, _, _, cx| {
                        if let Some(ed) = v.editing.as_mut() {
                            ed.menu = !ed.menu;
                        }
                        cx.notify();
                    }))
                    .child(ty)
                    .child(ui::small_icon("chevron-down", theme::fg_dim())),
            )
            .children(menu)
            .into_any_element()
    }

    /// Table: the edit box over a cell, with its type, add a field after it, remove it.
    fn cell_editor(&self, which: Option<usize>, r: usize, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(ed) = &self.editing else { return div().into_any_element() };
        let path = ed.path.clone();
        let (p_add, p_del) = (path.clone(), path.clone());
        // A field being added has no column: say what's being typed.
        let label = match (ed.key, ed.fresh, path.last()) {
            (true, _, _) => Some("New field name".to_string()),
            (false, true, Some(Seg::Key(k))) => Some(format!("{k} =")),
            _ => None,
        };
        deferred(
            div()
                .debug_selector(|| "db-cell-editor".into())
                .absolute()
                .top(px(-4.))
                .left(px(0.))
                .w(px(440.))
                .flex()
                .items_center()
                .gap(px(6.))
                .p(px(4.))
                .rounded(px(5.))
                .bg(theme::bg_popup())
                .border_1()
                .border_color(theme::border_input())
                .shadow_lg()
                .id("db-cell-editor")
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                // Clicks in it aren't clicks on the cell under it.
                .on_click(|_, _, cx| cx.stop_propagation())
                .when_some(label, |d, l| d.child(div().debug_selector(|| "db-cell-label".into()).flex_none().font_family(theme::UI_FONT).text_size(px(11.5)).text_color(theme::fg_dim()).child(l)))
                .child(div().flex_1().min_w_0().h(px(22.)).px(px(6.)).flex().items_center().rounded(px(3.)).bg(theme::bg_input()).border_1().border_color(theme::focus_border()).font_family(theme::MONO_FONT).child(ed.editor.clone()))
                .child(self.type_picker(cx))
                .child(
                    div()
                        .id("db-cell-add")
                        .debug_selector(|| "db-cell-add".into())
                        .p(px(3.))
                        .rounded(px(3.))
                        .border_1()
                        .border_color(theme::border_input())
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .tooltip(ui::tooltip("Add a field after this one"))
                        .on_click(cx.listener(move |v, _, window, cx| v.add_after(which, r, p_add.clone(), window, cx)))
                        .child(ui::small_icon("plus", theme::fg())),
                )
                .child(
                    div()
                        .id("db-cell-remove")
                        .debug_selector(|| "db-cell-remove".into())
                        .p(px(3.))
                        .rounded(px(3.))
                        .border_1()
                        .border_color(theme::border_input())
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .tooltip(ui::tooltip("Remove this field"))
                        .on_click(cx.listener(move |v, _, _, cx| {
                            v.editing = None;
                            v.edit_op(which, r, compass::Op::Remove(p_del.clone()), None, cx);
                        }))
                        .child(ui::small_icon("trash", theme::fg())),
                ),
        )
        .with_priority(1)
        .into_any_element()
    }

    /// Cancel and Update under a document (a card or a table row), when it has changes.
    fn doc_bar(&self, which: Option<usize>, r: usize, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let gv = self.grid_of(which)?;
        let e = gv.edits.get(&r)?;
        let typing = self.editing.as_ref().is_some_and(|ed| ed.which == which && ed.row == r);
        // Modified only when it differs from the document as loaded (a value put back isn't a change).
        let modified = e.working != e.original;
        let (saving, error) = (e.saving, e.error.clone());
        // Fields added (a table has no column for them yet).
        let added: Vec<String> = match (&e.working, &e.original) {
            (Tree::Object(w), Tree::Object(o)) => w.iter().filter(|(k, _)| !k.is_empty() && !o.iter().any(|(n, _)| n == k)).map(|(k, _)| k.clone()).collect(),
            _ => Vec::new(),
        };
        (modified || typing || error.is_some()).then(|| {
            div()
                .debug_selector(move || format!("db-doc-bar-{r}"))
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(14.))
                .py(px(6.))
                .bg(if error.is_some() { theme::git_deleted().opacity(0.25) } else { theme::git_modified().opacity(0.25) })
                .font_family(theme::UI_FONT)
                .when(!modified && error.is_none(), |d| d.bg(theme::bg_side()))
                .child(div().flex_1().min_w_0().text_color(theme::fg_bright()).child(match &error {
                    Some(e) => e.clone(),
                    None if modified && !added.is_empty() => format!("Document modified. New field{}: {}", if added.len() == 1 { "" } else { "s" }, added.join(", ")),
                    None if modified => "Document modified.".to_string(),
                    None => String::new(),
                }))
                .child(button(SharedString::from(format!("db-doc-cancel-{r}")), "Cancel", None, false).on_click(cx.listener(move |v, _, _, cx| v.cancel_doc(which, r, cx))))
                // Nothing changed yet: Update waits (dimmed).
                .child(
                    button(SharedString::from(format!("db-doc-update-{r}")), if saving { "Updating…" } else { "Update" }, None, true)
                        .when(!modified, |b| b.opacity(0.4).cursor_default())
                        .on_click(cx.listener(move |v, _, _, cx| v.update_doc(which, r, cx))),
                )
        })
    }

    // ------------------------------------------------------------ a document as JSON text, cloned, deleted

    fn doc_tree(&self, which: Option<usize>, r: usize) -> Option<Tree> {
        self.grid_of(which)?.grid.trees.as_ref()?.get(r).cloned()
    }

    /// The pencil in JSON: the card becomes its document as text, checked as it's typed.
    /// The text is the document as it is on the server (with the changes made so far), in exact
    /// Extended JSON: every type and character kept, so saving it changes only what was typed.
    fn start_json_edit(&mut self, which: Option<usize>, r: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Body::Data(d) = &self.body else { return };
        let [db, coll] = d.path.as_slice() else { return };
        let (conn, db, coll) = (d.conn.clone(), db.clone(), coll.clone());
        let edit = self.grid_of(which).and_then(|g| g.edits.get(&r)).map(|e| (e.original.clone(), e.ops.clone()));
        let Some((original, ops)) = edit.or_else(|| self.doc_tree(which, r).map(|t| (t, Vec::new()))) else { return };
        if let Err(e) = compass::editable(&original) {
            return cx.emit(Request::Error(e));
        }
        let work = engine::run(engine::mongo(conn, move |cl| async move { compass::doc_json(&cl, &db, &coll, &original, &ops).await }));
        cx.spawn_in(window, async move |this, cx| {
            let text = work.await;
            this.update_in(cx, |v, window, cx| match text {
                Ok(text) => v.open_json_editor(which, r, text, window, cx),
                Err(e) => cx.emit(Request::Error(e)),
            })
            .ok();
        })
        .detach();
    }

    fn open_json_editor(&mut self, which: Option<usize>, r: usize, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let editor = cx.new(|cx| {
            let mut ed = Editor::new(EditorMode::Full, cx);
            ed.set_text(&text, cx);
            ed.set_language("json", cx);
            ed
        });
        window.focus(&editor.read(cx).focus_target(cx), cx);
        let sub = cx.subscribe(&editor, move |v: &mut DbView, ed, ev: &EditorEvent, cx| {
            if matches!(ev, EditorEvent::Changed) {
                let invalid = compass::parse_document(&ed.read(cx).text()).err();
                if let Some(e) = v.grid_of_mut(which).and_then(|g| g.json_edits.get_mut(&r)) {
                    e.invalid = invalid;
                }
                cx.notify();
            }
        });
        if let Some(gv) = self.grid_of_mut(which) {
            gv.json_edits.insert(r, JsonEdit { editor, initial: text, invalid: None, error: None, saving: false, _sub: sub });
        }
        self.remeasure(which, r, cx);
    }

    fn cancel_json_edit(&mut self, which: Option<usize>, r: usize, cx: &mut Context<Self>) {
        if let Some(gv) = self.grid_of_mut(which) {
            gv.json_edits.remove(&r);
        }
        self.remeasure(which, r, cx);
    }

    /// Runs a call on the tab's collection, then reloads the documents (or shows why it failed).
    fn doc_call<F, Fut>(&mut self, which: Option<usize>, r: usize, done: &'static str, f: F, cx: &mut Context<Self>)
    where
        F: FnOnce(mongodb::Client, String, String) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        let Body::Data(d) = &self.body else { return };
        let [db, coll] = d.path.as_slice() else { return };
        let (conn, db, coll) = (d.conn.clone(), db.clone(), coll.clone());
        let work = engine::run(engine::mongo(conn, move |cl| f(cl, db, coll)));
        cx.spawn(async move |this, cx| {
            let r2 = work.await;
            this.update(cx, |v, cx| {
                match r2 {
                    Ok(()) => {
                        if let Some(gv) = v.grid_of_mut(which) {
                            gv.json_edits.remove(&r);
                        }
                        cx.emit(Request::Message(done.into()));
                        v.load(cx);
                    }
                    Err(e) => {
                        match v.grid_of_mut(which).and_then(|g| g.json_edits.get_mut(&r)) {
                            Some(j) => {
                                j.saving = false;
                                j.error = Some(e);
                            }
                            None => cx.emit(Request::Error(e)),
                        }
                        v.remeasure(which, r, cx);
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    fn save_json_edit(&mut self, which: Option<usize>, r: usize, cx: &mut Context<Self>) {
        // Checked against the document as it was loaded (changes made in other views included in the text).
        let loaded = self.grid_of(which).and_then(|g| g.edits.get(&r)).map(|e| e.original.clone());
        let Some(original) = loaded.or_else(|| self.doc_tree(which, r)) else { return };
        let Some(j) = self.grid_of_mut(which).and_then(|g| g.json_edits.get_mut(&r)) else { return };
        if j.saving || j.invalid.is_some() {
            return;
        }
        j.saving = true;
        let text = j.editor.read(cx).text();
        self.doc_call(which, r, "Document updated", move |cl, db, coll| async move { compass::replace_doc(&cl, &db, &coll, &original, &text).await }, cx);
        cx.notify();
    }

    fn clone_doc(&mut self, which: Option<usize>, r: usize, cx: &mut Context<Self>) {
        let Some(original) = self.doc_tree(which, r) else { return };
        self.doc_call(which, r, "Document cloned", move |cl, db, coll| async move { compass::clone_doc(&cl, &db, &coll, &original).await }, cx);
    }

    fn ask_delete_doc(&mut self, which: Option<usize>, r: usize, cx: &mut Context<Self>) {
        // The document clicked, kept now: the rows may be read again while the question is open.
        let Some(target) = self.doc_tree(which, r) else { return };
        let weak = cx.entity().downgrade();
        cx.emit(Request::Ask(crate::dialog::Ask {
            title: "Delete this document?".into(),
            detail: "It's removed from the collection. This can't be undone.".into(),
            buttons: vec!["Delete".into()],
            danger: true,
            cancel_default: true,
            cancel_label: None,
            on_answer: std::rc::Rc::new(move |_, _, cx: &mut App| {
                if let Some(v) = weak.upgrade() {
                    let original = target.clone();
                    v.update(cx, |v, cx| {
                        v.doc_call(which, r, "Document deleted", move |cl, db, coll| async move { compass::delete_doc(&cl, &db, &coll, &original).await }, cx);
                    });
                }
            }),
        }));
    }

    #[cfg(test)]
    pub fn json_editor(&self, row: usize) -> Option<(Entity<Editor>, Option<String>)> {
        self.grid_of(None)?.json_edits.get(&row).map(|j| (j.editor.clone(), j.invalid.clone().or(j.error.clone())))
    }

    /// What's typed in an edit box goes into the document (switching views).
    /// False when it can't be (the edit box stays open, the reason shown in the bar).
    fn commit_edit_quietly(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(ed) = self.editing.take() else { return true };
        let text = ed.editor.read(cx).text();
        let (which, row) = (ed.which, ed.row);
        let op = if ed.key {
            let new = text.trim().to_string();
            match ed.path.last() {
                Some(Seg::Key(old)) if !new.is_empty() && new != *old => {
                    let mut renamed = ed.path.clone();
                    *renamed.last_mut().unwrap() = Seg::Key(new.clone());
                    Some((compass::Op::Rename(ed.path.clone(), new), renamed))
                }
                _ => None,
            }
        } else {
            let value = if ed.fresh && !ed.ty_chosen { compass::edit_value(&text, None) } else { compass::typed_value(&text, ed.ty) };
            match value {
                Ok(v) => Some((compass::Op::Set(ed.path.clone(), v), ed.path.clone())),
                Err(e) => {
                    if let Some(d) = self.doc_edit(which, row) {
                        d.error = Some(e);
                    }
                    self.editing = Some(ed);
                    self.remeasure(which, row, cx);
                    return false;
                }
            }
        };
        match op {
            Some((op, mark)) => self.edit_op(which, row, op, Some(mark), cx),
            None => true,
        }
    }

    /// Esc: the key or value as it was.
    fn cancel_edit(&mut self, cx: &mut Context<Self>) {
        if let Some(ed) = self.editing.take() {
            self.remeasure(ed.which, ed.row, cx);
        }
    }

    /// The plus on a line: a new field (or array item) right after it, its name asked for.
    fn add_after(&mut self, which: Option<usize>, row: usize, path: Vec<Seg>, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_edit(window, cx);
        let Some((last, parent)) = path.split_last() else { return };
        let parent = parent.to_vec();
        match last {
            Seg::Idx(i) => {
                let mut at = parent.clone();
                at.push(Seg::Idx(i + 1));
                if self.edit_op(which, row, compass::Op::Insert(parent, i + 1, None, mongodb::bson::Bson::String(String::new())), Some(at.clone()), cx) {
                    self.start_edit(which, row, at, false, true, window, cx);
                }
            }
            Seg::Key(k) => {
                let Some(e) = self.doc_edit(which, row) else { return };
                let mut t = Some(&e.working);
                for s in &parent {
                    t = match (t, s) {
                        (Some(Tree::Object(f)), Seg::Key(k)) => f.iter().find(|(n, _)| n == k).map(|(_, v)| v),
                        (Some(Tree::Array(a)), Seg::Idx(i)) => a.get(*i),
                        _ => None,
                    };
                }
                let Some(Tree::Object(fields)) = t else { return };
                if fields.iter().any(|(n, _)| n.is_empty()) {
                    return; // one unnamed field at a time
                }
                let ix = fields.iter().position(|(n, _)| n == k).map_or(fields.len(), |i| i + 1);
                let mut at = parent.clone();
                at.push(Seg::Key(String::new()));
                if self.edit_op(which, row, compass::Op::Insert(parent, ix, Some(String::new()), mongodb::bson::Bson::String(String::new())), Some(at.clone()), cx) {
                    self.start_edit(which, row, at, true, true, window, cx);
                }
            }
        }
    }

    fn cancel_doc(&mut self, which: Option<usize>, row: usize, cx: &mut Context<Self>) {
        if self.editing.as_ref().is_some_and(|e| e.which == which && e.row == row) {
            self.editing = None;
        }
        if let Some(gv) = self.grid_of_mut(which) {
            gv.edits.remove(&row);
        }
        self.remeasure(which, row, cx);
    }

    /// Update: saves the card's changes (the document is checked unchanged meanwhile), then shows it as saved.
    fn update_doc(&mut self, which: Option<usize>, row: usize, cx: &mut Context<Self>) {
        let Body::Data(d) = &self.body else { return };
        let [db, coll] = d.path.as_slice() else { return };
        let (conn, db, coll) = (d.conn.clone(), db.clone(), coll.clone());
        // What's still in an edit box is part of it.
        if self.editing.as_ref().is_some_and(|e| e.which == which && e.row == row) && !self.commit_edit_quietly(cx) {
            return; // what's typed isn't a value of its type: said in the bar
        }
        let Some(e) = self.doc_edit(which, row) else { return };
        if e.saving || e.ops.is_empty() || e.working == e.original {
            return;
        }
        e.saving = true;
        let (original, ops) = (e.original.clone(), e.ops.clone());
        let work = engine::run(engine::mongo(conn, move |cl| async move { compass::update_doc(&cl, &db, &coll, &original, &ops).await }));
        cx.spawn(async move |this, cx| {
            let r = work.await;
            this.update(cx, |v, cx| {
                match r {
                    Ok(()) => {
                        if let Some(gv) = v.grid_of_mut(which) {
                            gv.edits.remove(&row);
                        }
                        cx.emit(Request::Message("Document updated".into()));
                        v.load(cx);
                    }
                    Err(err) => {
                        if let Some(e) = v.doc_edit(which, row) {
                            e.saving = false;
                            e.error = Some(err);
                        }
                    }
                }
                v.remeasure(which, row, cx);
            })
            .ok();
        })
        .detach();
        self.remeasure(which, row, cx);
    }

    /// How far the table is scrolled sideways (tests).
    #[cfg(test)]
    pub fn table_scroll_x(&self) -> f32 {
        self.grid_of(None).map(|g| -g.hscroll.offset().x / gpui::px(1.)).unwrap_or(0.)
    }

    #[cfg(test)]
    pub fn set_table_scroll_x(&self, x: f32) {
        if let Some(g) = self.grid_of(None) {
            g.hscroll.set_offset(gpui::point(gpui::px(-x), gpui::px(0.)));
        }
    }

    #[cfg(test)]
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.load(cx);
    }

    #[cfg(test)]
    pub fn choose_type(&mut self, ty: &'static str) {
        if let Some(ed) = self.editing.as_mut() {
            ed.ty = ty;
            ed.ty_chosen = true;
        }
    }

    #[cfg(test)]
    pub fn col_width(&self, i: usize) -> f32 {
        self.grid_of(None).map(|g| g.widths[i]).unwrap_or(0.)
    }

    #[cfg(test)]
    pub fn scroll_docs_to(&self, item: usize) {
        if let Some(g) = self.grid_of(None) {
            g.list.scroll_to(gpui::ListOffset { item_ix: item, offset_in_item: px(0.) });
        }
    }

    #[cfg(test)]
    pub fn added_fields(&self, row: usize) -> Vec<String> {
        let Some(e) = self.grid_of(None).and_then(|g| g.edits.get(&row)) else { return Vec::new() };
        match (&e.working, &e.original) {
            (Tree::Object(w), Tree::Object(o)) => w.iter().filter(|(k, _)| !o.iter().any(|(n, _)| n == k)).map(|(k, _)| k.clone()).collect(),
            _ => Vec::new(),
        }
    }

    #[cfg(test)]
    pub fn query_status(&self) -> (bool, Option<String>) {
        match &self.body {
            Body::Data(d) => (d.running.is_some(), d.query_error.clone()),
            _ => (false, None),
        }
    }

    #[cfg(test)]
    pub fn bar_sort(&self, cx: &App) -> String {
        match &self.body {
            Body::Data(d) => d.compass.as_ref().map(|c| c.sort_text(cx)).unwrap_or_default(),
            _ => String::new(),
        }
    }

    #[cfg(test)]
    pub fn sort_for_test(&mut self, col: &str, desc: bool, cx: &mut Context<Self>) {
        if let Body::Data(d) = &mut self.body {
            d.spec.sort = Some((col.to_string(), desc));
        }
        self.load(cx);
    }

    #[cfg(test)]
    pub fn run_shell_for_test(&mut self, cx: &mut Context<Self>) {
        self.run_query_checked(false, cx);
    }

    #[cfg(test)]
    pub fn card_modified(&self, row: usize) -> bool {
        self.grid_of(None).and_then(|g| g.edits.get(&row)).is_some_and(|e| e.working != e.original)
    }

    #[cfg(test)]
    pub fn card_error(&self, row: usize) -> Option<String> {
        self.grid_of(None)?.edits.get(&row)?.error.clone()
    }

    #[cfg(test)]
    pub fn editor_in_card(&self) -> Option<Entity<Editor>> {
        self.editing.as_ref().map(|e| e.editor.clone())
    }

    /// Folds or unfolds a nested value of a document.
    fn toggle_fold(&mut self, which: Option<usize>, row: usize, path: String, cx: &mut Context<Self>) {
        let Some(gv) = self.grid_of_mut(which) else { return };
        if !gv.unfolded.remove(&path) {
            gv.unfolded.insert(path);
        }
        gv.list.splice(row..row + 1, 1);
        cx.notify();
    }

    fn render_item(&mut self, which: Option<usize>, ix: usize, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(gv) = self.grid_of(which) else { return div().into_any_element() };
        match self.doc_mode(gv) {
            DocMode::Table => self.render_row(which, ix, cx),
            mode => self.render_card(which, ix, mode == DocMode::Json, cx),
        }
    }

    fn render_row(&mut self, which: Option<usize>, r: usize, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(gv) = self.grid_of(which) else { return div().into_any_element() };
        let first = match (&self.body, which) {
            (Body::Data(d), None) => d.spec.offset,
            _ => 0,
        };
        let (grid, widths, sel, open) = (gv.grid.clone(), gv.widths.clone(), gv.sel, gv.open);
        // The part of the table in view (it scrolls sideways): an opened row's fields stay in it.
        let (shift, view_w) = (-gv.hscroll.offset().x, gv.hscroll.bounds().size.width);
        let Some(row) = grid.rows.get(r) else { return div().into_any_element() };
        // Edited and not saved yet: shown as edited.
        let working = gv.edits.get(&r).map(|e| e.working.clone());
        let tree = working.as_ref().or_else(|| grid.trees.as_ref().and_then(|t| t.get(r)));
        let editable = which.is_none() && matches!(&self.body, Body::Data(d) if d.compass.is_some());
        let edit_col = self.editing.as_ref().filter(|e| e.which == which && e.row == r).and_then(|e| e.anchor_col);
        let fields: Option<&Vec<(String, Tree)>> = match tree {
            Some(Tree::Object(f)) => Some(f),
            _ => None,
        };
        let line = div()
            .flex()
            .w_full()
            .h(px(26.))
            .border_b_1()
            .border_color(theme::border())
            .when(open == Some(r), |d| d.bg(theme::selected()))
            .hover(|s| s.bg(theme::hover()))
            .child(div().w(px(44.)).flex_none().flex().items_center().justify_end().pr(px(8.)).text_color(theme::fg_muted()).text_size(px(11.)).child((first + r + 1).to_string()))
            .children(row.iter().enumerate().map(|(c, v)| {
                let on = sel == Some((r, c));
                // MongoDB: the value with its type (colored); else the text.
                let typed = fields.and_then(|f| f.iter().find(|(k, _)| Some(k) == grid.columns.get(c)).map(|(_, t)| t));
                let (text, color, italic) = match (typed, v) {
                    (Some(t), _) => (table_text(t), tree_color(t), matches!(t, Tree::Null)),
                    (None, Some(s)) => (s.replace(['\n', '\r', '\t'], " "), theme::fg(), false),
                    (None, None) => ("NULL".into(), theme::fg_muted(), true),
                };
                let text = if fields.is_some() && typed.is_none() { String::new() } else { text };
                let container_cell = matches!(typed, Some(Tree::Array(_) | Tree::Object(_)));
                div()
                    .id(("db-cell", r * 1000 + c))
                    .flex_basis(px(widths.get(c).copied().unwrap_or(100.)))
                    .flex_grow(1.)
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .px(px(8.))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .cursor_pointer()
                    .when(on, |d| d.border_1().border_color(theme::focus_border()))
                    .text_color(color)
                    .when(italic, |d| d.italic())
                    .on_click(cx.listener(move |v, ev: &ClickEvent, window, cx| {
                        // A collection's cell double-clicked: typed into, like Compass.
                        // (Not an array or object: those open with the row, to look at.)
                        if editable && ev.click_count() == 2 && !container_cell {
                            let col = v.grid_of(which).and_then(|g| g.grid.columns.get(c).cloned());
                            if let Some(col) = col.filter(|c| c != "_id") {
                                // The first click of the two opened (or closed) the row: as it was.
                                if let Some(before) = v.open_before.take()
                                    && let Some(gv) = v.grid_of_mut(which)
                                    && gv.open != before
                                {
                                    for row in [gv.open, before].into_iter().flatten() {
                                        gv.list.splice(row..row + 1, 1);
                                    }
                                    gv.open = before;
                                }
                                v.edit_anchor = Some(c);
                                return v.start_edit(which, r, vec![Seg::Key(col)], false, false, window, cx);
                            }
                        }
                        // More clicks select a word or the line (the first opened the row).
                        if ev.click_count() > 1 {
                            return;
                        }
                        window.focus(&v.focus, cx);
                        // A drag selected text: not a click on the row.
                        if !v.sel.has_selection() {
                            v.open_before = v.grid_of(which).map(|g| g.open);
                            v.toggle_row(which, r, Some(c), cx);
                        }
                    }))
                    .when(edit_col == Some(c), |d| d.relative().overflow_hidden().child(self.cell_editor(which, r, cx)))
                    .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(self.sel.text_piece(((r * 100_000) as u64, c as u32), text, color, false).join("\t")))
            }));
        // Scrolled sideways: its number and id stay on the left (over the cells scrolled under them).
        let line = if shift > px(0.) {
            let pin_first = pinned_first(&grid.columns);
            let id_text = pin_first.then(|| match fields.and_then(|f| f.iter().find(|(k, _)| Some(k) == grid.columns.first()).map(|(_, t)| t)) {
                Some(t) => (table_text(t), tree_color(t)),
                None => (row.first().cloned().flatten().unwrap_or_default(), theme::fg()),
            });
            let w0 = widths.first().copied().unwrap_or(100.);
            line.relative().child(
                div()
                    .id(("db-pinned", r))
                    .debug_selector(move || format!("db-pinned-{r}"))
                    .absolute()
                    .top(px(0.))
                    .left(shift)
                    .h_full()
                    .flex()
                    .bg(if open == Some(r) { theme::selected() } else { theme::bg_editor() })
                    .border_r_1()
                    .border_color(theme::border())
                    .cursor_pointer()
                    .on_click(cx.listener(move |v, _, window, cx| {
                        window.focus(&v.focus, cx);
                        v.toggle_row(which, r, Some(0), cx);
                    }))
                    .child(div().w(px(44.)).flex_none().flex().items_center().justify_end().pr(px(8.)).text_color(theme::fg_muted()).text_size(px(11.)).child((first + r + 1).to_string()))
                    .when_some(id_text, |d, (t, c)| d.child(div().w(px(w0)).flex_none().flex().items_center().px(px(8.)).overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(c).child(t))),
            )
        } else {
            line
        };
        let bar = self.doc_bar(which, r, cx).map(|b| if view_w > px(0.) { b.ml(shift).w(view_w) } else { b });
        if open != Some(r) {
            return match bar {
                Some(b) => div().flex().flex_col().w_full().child(line).child(b.justify_end()).into_any_element(),
                None => line.into_any_element(),
            };
        }
        // Open: every field one under another; long values stay on one line and scroll.
        let detail: Vec<gpui::AnyElement> = match tree {
            Some(t @ Tree::Object(_)) => self.tree_lines(which, r, t, false, cx),
            _ => grid
                .columns
                .iter()
                .zip(row.iter())
                .enumerate()
                .map(|(i, (col, val))| {
                    let copy = val.clone().unwrap_or_else(|| "NULL".into());
                    div()
                        .id(("db-field", i))
                        .group("db-field")
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .h(px(24.))
                        .child(div().w(px(200.)).flex_none().overflow_hidden().text_ellipsis().whitespace_nowrap().text_color(theme::fg_dim()).child(col.clone()))
                        .child(one_line(("db-field-value", i), match val {
                            Some(v) => div().child(self.sel.text_piece(((r * 100_000 + 1 + i) as u64, 1), v.replace(['\n', '\r'], " ⏎ "), theme::fg_bright(), false).join("\t")),
                            None => div().italic().text_color(theme::fg_muted()).child("NULL"),
                        }))
                        .child(copy_button(("db-field-copy", i), copy))
                        .into_any_element()
                })
                .collect(),
        };
        div()
            .flex()
            .flex_col()
            .w_full()
            .child(line)
            .child(
                div()
                    .debug_selector(|| "db-fields".into())
                    .flex()
                    .flex_col()
                    .ml(shift)
                    .when(view_w > px(0.), |d| d.w(view_w))
                    .when(view_w <= px(0.), |d| d.w_full())
                    .px(px(52.))
                    .py(px(8.))
                    .border_b_1()
                    .border_color(theme::border())
                    .bg(theme::bg_side())
                    .text_size(px(12.5))
                    .relative()
                    .children(detail)
                    // Closes it again (like clicking the row).
                    .child(
                        div()
                            .id(("db-fields-close", r))
                            .debug_selector(move || format!("db-fields-close-{r}"))
                            // In the margin left of the fields: it never covers a value.
                            .absolute()
                            .top(px(8.))
                            .left(px(14.))
                            .flex()
                            .items_center()
                            .p(px(3.))
                            .rounded(px(4.))
                            .border_1()
                            .border_color(theme::border_input())
                            .bg(theme::bg_editor())
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::hover()))
                            .tooltip(ui::tooltip("Collapse (Esc)"))
                            .text_size(px(11.5))
                            .text_color(theme::fg_dim())
                            .on_click(cx.listener(move |v, _, _, cx| {
                                cx.stop_propagation();
                                if let Some(gv) = v.grid_of_mut(which)
                                    && gv.open == Some(r)
                                {
                                    gv.open = None;
                                    gv.list.splice(r..r + 1, 1);
                                }
                                cx.notify();
                            }))
                            .child(ui::small_icon("collapse-all", theme::fg_dim())),
                    ),
            )
            .children(bar)
            .into_any_element()
    }

    /// A MongoDB document as a card: its fields one per line (List), or as JSON.
    fn render_card(&mut self, which: Option<usize>, r: usize, json: bool, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(gv) = self.grid_of(which) else { return div().into_any_element() };
        let edit = gv.edits.get(&r).map(|e| (e.working.clone(), !e.ops.is_empty(), e.saving, e.error.clone()));
        let Some(t) = edit.as_ref().map(|e| e.0.clone()).or_else(|| gv.grid.trees.as_ref().and_then(|t| t.get(r)).cloned()) else { return div().into_any_element() };
        let copy = t.text(if json { TextStyle::Json } else { TextStyle::Shell });
        let lines = self.tree_lines(which, r, &t, json, cx);
        let bar = self.doc_bar(which, r, cx).map(|b| b.mt(px(8.)).mx(px(-14.)).mb(px(-10.)).rounded_b(px(6.)));
        let editable = which.is_none() && matches!(&self.body, Body::Data(d) if d.compass.is_some());
        // JSON typed whole: the text, why it isn't a document yet, Cancel and Update.
        let json_edit = gv.json_edits.get(&r).map(|j| (j.editor.clone(), j.invalid.clone(), j.error.clone(), j.saving));
        if let Some((editor, invalid, error, saving)) = json_edit {
            let lines = editor.read(cx).text().lines().count().max(3);
            let problem = invalid.clone().or(error);
            return div()
                .pb(px(8.))
                .pr(px(4.))
                .child(
                    div()
                        .debug_selector(move || format!("db-json-edit-{r}"))
                        .flex()
                        .flex_col()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(if invalid.is_some() { theme::git_deleted() } else { theme::focus_border() })
                        .bg(theme::bg_side())
                        .overflow_hidden()
                        .child(div().h(px((lines as f32 * 19. + 16.).min(560.))).child(editor))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .px(px(14.))
                                .py(px(6.))
                                .bg(if problem.is_some() { theme::git_deleted().opacity(0.25) } else { theme::git_modified().opacity(0.25) })
                                .child(div().debug_selector(move || format!("db-json-status-{r}")).flex_1().min_w_0().text_color(theme::fg_bright()).child(problem.unwrap_or_else(|| "Document modified.".into())))
                                .child(button(SharedString::from(format!("db-json-cancel-{r}")), "Cancel", None, false).on_click(cx.listener(move |v, _, _, cx| v.cancel_json_edit(which, r, cx))))
                                .child(
                                    button(SharedString::from(format!("db-json-update-{r}")), if saving { "Updating…" } else { "Update" }, None, true)
                                        .when(invalid.is_some(), |b| b.opacity(0.4))
                                        .on_click(cx.listener(move |v, _, _, cx| v.save_json_edit(which, r, cx))),
                                ),
                        ),
                )
                .into_any_element();
        }
        let tool = |id: &'static str, icon: &'static str, tip: &'static str| {
            div().id((id, r)).debug_selector(move || format!("{id}-{r}")).p(px(4.)).rounded(px(4.)).border_1().border_color(theme::border_input()).bg(theme::bg_editor()).cursor_pointer().hover(|s| s.bg(theme::hover())).tooltip(ui::tooltip(tip)).child(ui::small_icon(icon, theme::fg()))
        };
        let tools = div()
            .absolute()
            .top(px(6.))
            .right(px(8.))
            .flex()
            .gap(px(6.))
            .invisible()
            .group_hover("db-card", |s| s.visible())
            .when(editable && json, |d| d.child(tool("db-doc-edit", "pencil", "Edit as JSON").on_click(cx.listener(move |v, _, window, cx| v.start_json_edit(which, r, window, cx)))))
            .child(tool("db-card-copy", "copy", "Copy").on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))))
            .when(editable, |d| {
                d.child(tool("db-doc-clone", "files", "Clone: insert a copy").on_click(cx.listener(move |v, _, _, cx| v.clone_doc(which, r, cx))))
                    .child(tool("db-doc-delete", "trash", "Delete").on_click(cx.listener(move |v, _, _, cx| v.ask_delete_doc(which, r, cx))))
            });
        div()
            .pb(px(8.))
            .pr(px(4.))
            .child(
                div()
                    .id(("db-card", r))
                    .debug_selector(move || format!("db-card-{r}"))
                    .group("db-card")
                    .relative()
                    .flex()
                    .flex_col()
                    .px(px(14.))
                    .py(px(10.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(theme::border_input())
                    .bg(theme::bg_side())
                    .font_family(theme::MONO_FONT)
                    .text_size(px(12.5))
                    .children(lines)
                    .children(bar)
                    .child(tools),
            )
            .into_any_element()
    }

    /// A document's fields as lines: `key: value`, nested objects and arrays folded until
    /// clicked (List), or as JSON with its brackets. A collection's List can be edited in place:
    /// double-click a key or value, the trash and plus on a line remove it or add one after it.
    fn tree_lines(&self, which: Option<usize>, row: usize, t: &Tree, json: bool, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let Some(gv) = self.grid_of(which) else { return Vec::new() };
        // Edited in the List view only: JSON and a table's opened row are to look at (and select).
        let editable = !json && which.is_none() && self.doc_mode(gv) == DocMode::List && matches!(&self.body, Body::Data(d) if d.compass.is_some());
        let changed = gv.edits.get(&row).map(|e| e.changed.clone()).unwrap_or_default();
        let editing = self.editing.as_ref().filter(|e| e.which == which && e.row == row).map(|e| (e.path.clone(), e.key, e.editor.clone()));
        let mut out = Vec::new();
        let mut n = 0usize;
        struct W<'a> {
            which: Option<usize>,
            row: usize,
            json: bool,
            style: TextStyle,
            unfolded: &'a HashSet<String>,
            editable: bool,
            changed: &'a HashSet<Vec<Seg>>,
            editing: Option<(Vec<Seg>, bool, Entity<Editor>)>,
        }
        fn walk(v: &DbView, w: &W, entries: Vec<(String, &Tree)>, path: &str, segs: &[Seg], depth: usize, out: &mut Vec<gpui::AnyElement>, n: &mut usize, cx: &mut Context<DbView>) {
            let (which, row, json, style) = (w.which, w.row, w.json, w.style);
            let count = entries.len();
            for (i, (key, val)) in entries.into_iter().enumerate() {
                let p = format!("{path}/{key}");
                let seg = match key.strip_prefix(['\u{0}', '\u{1}']) {
                    Some(ix) => Seg::Idx(ix.parse().unwrap_or(0)),
                    None => Seg::Key(key.clone()),
                };
                let here: Vec<Seg> = segs.iter().cloned().chain(std::iter::once(seg.clone())).collect();
                let container = matches!(val, Tree::Array(a) if !a.is_empty()) || matches!(val, Tree::Object(f) if !f.is_empty());
                let open = container && w.unfolded.contains(&format!("{row}{p}"));
                let comma = if json && i + 1 < count { "," } else { "" };
                let key_text = match key.strip_prefix('\u{1}') {
                    // An array item's index (List): as is.
                    Some(i) => i.to_string(),
                    None if key.starts_with('\u{0}') => String::new(),
                    None => Tree::key(&key, if json { TextStyle::Json } else { TextStyle::Shell }),
                };
                let (summary, color) = match val {
                    Tree::Array(a) if container && !open => (if json { format!("[…{}]{comma}", a.len()) } else { format!("Array ({})", a.len()) }, theme::fg_dim()),
                    Tree::Object(f) if container && !open => (if json { format!("{{…}}{comma}") } else { format!("Object ({} field{})", f.len(), if f.len() == 1 { "" } else { "s" }) }, theme::fg_dim()),
                    Tree::Array(_) if open => (if json { "[".into() } else { String::new() }, theme::fg_dim()),
                    Tree::Object(_) if open => (if json { "{".into() } else { String::new() }, theme::fg_dim()),
                    other => (format!("{}{comma}", other.short(style)), tree_color(other)),
                };
                let fold = format!("{row}{p}");
                *n += 1;
                let id = *n;
                let top_id = depth == 0 && key == "_id";
                let edit_key = w.editing.as_ref().filter(|(ep, k, _)| *ep == here && *k).map(|(_, _, e)| e.clone());
                let edit_value = w.editing.as_ref().filter(|(ep, k, _)| *ep == here && !*k).map(|(_, _, e)| e.clone());
                // The box as wide as the name in it (the whole name shows), as tall as the text needs.
                let field_box = |e: Entity<Editor>, wide: bool, cx: &mut Context<DbView>| {
                    let chars = e.read(cx).text().chars().count().max(8) as f32;
                    div().when(wide, |d| d.flex_1()).when(!wide, |d| d.w(px(chars * 7.8 + 24.)).flex_none()).h(px(24.)).px(px(5.)).flex().items_center().rounded(px(3.)).bg(theme::bg_input()).border_1().border_color(theme::focus_border()).child(e)
                };
                let editing_here = w.editing.as_ref().is_some_and(|(ep, _, _)| *ep == here);
                let key_el = match (edit_key, key_text.is_empty()) {
                    (Some(e), _) => Some(div().flex().items_center().gap(px(4.)).mr(px(6.)).child(field_box(e, false, cx)).child(":").into_any_element()),
                    (None, false) => {
                        let piece = v.sel.text_piece(((row * 100_000 + id) as u64, 0), format!("{key_text}: "), theme::fg_bright(), true);
                        let renamable = w.editable && matches!(seg, Seg::Key(_)) && !top_id;
                        let path_k = here.clone();
                        let refold = container.then(|| fold.clone());
                        Some(
                            div()
                                .id(("db-key", id))
                                .debug_selector(move || format!("db-key-{row}-{id}"))
                                .flex_none()
                                .when(renamable, |d| {
                                    d.on_click(cx.listener(move |v, ev: &ClickEvent, window, cx| {
                                        if ev.click_count() == 2 {
                                            cx.stop_propagation();
                                            // The first click of the two folded or unfolded it: put it back.
                                            if let Some(f) = &refold {
                                                v.toggle_fold(which, row, f.clone(), cx);
                                            }
                                            v.edit_anchor = None;
                                            v.start_edit(which, row, path_k.clone(), true, false, window, cx);
                                        }
                                    }))
                                })
                                .child(piece)
                                .into_any_element(),
                        )
                    }
                    (None, true) => None,
                };
                let value_el = match edit_value {
                    Some(e) => div().flex_1().flex().items_center().gap(px(6.)).child(field_box(e, true, cx)).child(v.type_picker(cx)).into_any_element(),
                    None => {
                        let path_v = here.clone();
                        let editable_value = w.editable && !container && !top_id;
                        one_line(("db-tree-value", id), div().child(v.sel.text_piece(((row * 100_000 + id) as u64, 1), summary, color, false)))
                            .debug_selector(move || format!("db-value-{row}-{id}"))
                            .when(editable_value, |d| {
                                d.on_click(cx.listener(move |v, ev: &ClickEvent, window, cx| {
                                    if ev.click_count() == 2 {
                                        cx.stop_propagation();
                                        v.edit_anchor = None;
                                        v.start_edit(which, row, path_v.clone(), false, false, window, cx);
                                    }
                                }))
                            })
                            .into_any_element()
                    }
                };
                let gutter = w.editable.then(|| {
                    let (path_del, path_add) = (here.clone(), here.clone());
                    div()
                        .w(px(36.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(2.))
                        .invisible()
                        .group_hover("db-line", |s| s.visible())
                        .when(!top_id, |d| {
                            d.child(
                                div()
                                    .id(("db-line-remove", id))
                                    .debug_selector(move || format!("db-remove-{row}-{id}"))
                                    .p(px(1.))
                                    .rounded(px(3.))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(theme::hover()))
                                    .tooltip(ui::tooltip("Remove"))
                                    .on_click(cx.listener(move |v, _, _, cx| {
                                        cx.stop_propagation();
                                        v.edit_op(which, row, compass::Op::Remove(path_del.clone()), None, cx);
                                    }))
                                    .child(ui::small_icon("trash", theme::fg_dim())),
                            )
                        })
                        .child(
                            div()
                                .id(("db-line-add", id))
                                .debug_selector(move || format!("db-add-{row}-{id}"))
                                .p(px(1.))
                                .rounded(px(3.))
                                .border_1()
                                .border_color(theme::border_input())
                                .cursor_pointer()
                                .hover(|s| s.bg(theme::hover()))
                                .tooltip(ui::tooltip("Add a field after this one"))
                                .on_click(cx.listener(move |v, _, window, cx| {
                                    cx.stop_propagation();
                                    v.edit_anchor = None;
                                    v.add_after(which, row, path_add.clone(), window, cx);
                                }))
                                .child(ui::small_icon("plus", theme::fg_dim())),
                        )
                });
                out.push(
                    div()
                        .id(("db-tree-line", id))
                        .group("db-line")
                        .flex()
                        .items_center()
                        .h(px(if editing_here { 28. } else { 21. }))
                        .whitespace_nowrap()
                        // Changed, or being changed: highlighted.
                        .when(w.changed.contains(&here) || editing_here, |d| d.bg(theme::git_modified().opacity(0.18)))
                        .children(gutter)
                        .child(div().w(px(depth as f32 * 18.)).flex_none())
                        // One click opens or closes; a second and third select a word, the line.
                        .when(container, |d| {
                            d.cursor_pointer().on_click(cx.listener(move |v, ev: &ClickEvent, _, cx| {
                                if ev.click_count() == 1 {
                                    v.toggle_fold(which, row, fold.clone(), cx)
                                }
                            }))
                        })
                        .child(div().w(px(14.)).flex_none().text_color(theme::fg_muted()).child(if container { if open { "▾" } else { "▸" } } else { "" }))
                        .children(key_el)
                        .child(value_el)
                        .into_any_element(),
                );
                if open {
                    let children: Vec<(String, &Tree)> = match val {
                        // Array items by index (no key shown in JSON).
                        Tree::Array(a) => a.iter().enumerate().map(|(i, x)| (if json { format!("\u{0}{i}") } else { format!("\u{1}{i}") }, x)).collect(),
                        Tree::Object(f) => f.iter().map(|(k, x)| (k.clone(), x)).collect(),
                        _ => Vec::new(),
                    };
                    walk(v, w, children, &p, &here, depth + 1, out, n, cx);
                    if json {
                        *n += 1;
                        let close = format!("{}{comma}", if matches!(val, Tree::Array(_)) { "]" } else { "}" });
                        out.push(div().h(px(21.)).flex().items_center().pl(px(depth as f32 * 18. + 14.)).child(v.sel.text_piece(((row * 100_000 + *n) as u64, 0), close, theme::fg_dim(), false)).into_any_element());
                    }
                }
            }
        }
        let Tree::Object(fields) = t else { return out };
        let unfolded = gv.unfolded.clone();
        let w = W { which, row, json, style: if json { TextStyle::Json } else { TextStyle::Shell }, unfolded: &unfolded, editable, changed: &changed, editing };
        if json {
            out.push(div().h(px(21.)).flex().items_center().child(self.sel.text_piece(((row * 100_000) as u64, 0), "{", theme::fg_dim(), false)).into_any_element());
        }
        walk(self, &w, fields.iter().map(|(k, v)| (k.clone(), v)).collect(), "", &[], if json { 1 } else { 0 }, &mut out, &mut n, cx);
        if json {
            out.push(div().h(px(21.)).flex().items_center().child(self.sel.text_piece(((row * 100_000 + 99_999) as u64, 0), "}", theme::fg_dim(), false)).into_any_element());
        }
        out
    }

    fn render_data_view(&self, d: &Data, cx: &mut Context<Self>) -> gpui::AnyElement {
        let kind = d.conn.kind;
        let what = match (kind, d.path.len()) {
            (Kind::MongoDb, _) => "Collection",
            (Kind::Redis, _) => "Key",
            (Kind::RabbitMq, _) if d.path.first().is_some_and(|f| f == "Queues") => "Queue",
            (Kind::RabbitMq, _) => "Exchange",
            _ => "Table",
        };
        let name = d.path.last().cloned().unwrap_or_default();
        // Redis searches hashes and sets (by field or member); RabbitMQ shows details only.
        let searchable = match kind {
            Kind::RabbitMq => false,
            Kind::Redis => d.grid.as_ref().is_some_and(|g| matches!(g.grid.columns.first().map(String::as_str), Some("field" | "member"))),
            _ => true,
        };
        let pageable = kind != Kind::RabbitMq;
        let (total, estimate) = match &d.page {
            Some(Ok(p)) => (p.total, p.estimate),
            _ => (None, false),
        };
        let shown = d.grid.as_ref().map(|g| g.grid.rows.len()).unwrap_or(0);
        let showing = match total {
            _ if d.loading && d.grid.is_none() => "Loading…".to_string(),
            Some(0) => "No records".to_string(),
            Some(t) => format!("Showing {}–{} of {}{} records", d.spec.offset + 1, d.spec.offset + shown, if estimate { "≈ " } else { "" }, group_digits(t)),
            None => format!("{shown} rows"),
        };
        let pages = total.map(|t| (t as usize).div_ceil(d.spec.limit.max(1)).max(1));
        let page_no = d.spec.offset / d.spec.limit.max(1) + 1;
        let col_label = d.spec.column.clone().unwrap_or_else(|| "All columns".into());
        let dropdown = |id: &'static str, text: String, menu: Menu, cx: &mut Context<Self>| {
            div()
                .id(id)
                .debug_selector(move || id.to_string())
                .flex()
                .items_center()
                .gap(px(6.))
                .h(px(28.))
                .px(px(10.))
                .rounded(px(4.))
                .bg(theme::bg_input())
                .border_1()
                .border_color(theme::border_input())
                .cursor_pointer()
                .hover(|s| s.bg(theme::hover()))
                .on_click(cx.listener(move |v, _, _, cx| {
                    if let Body::Data(d) = &mut v.body {
                        d.menu = if d.menu == Some(menu) { None } else { Some(menu) };
                    }
                    cx.notify();
                }))
                .child(text)
                .child(ui::small_icon("chevron-down", theme::fg_dim()))
        };
        let menu = match d.menu {
            None => None,
            Some(m) => {
                let items: Vec<(String, Option<String>, Option<usize>)> = match m {
                    Menu::Columns => std::iter::once(("All columns".to_string(), None, None))
                        .chain(d.grid.as_ref().map(|g| g.grid.columns.clone()).unwrap_or_default().into_iter().map(|c| (c.clone(), Some(c), None)))
                        .collect(),
                    Menu::Sizes => PAGE_SIZES.iter().map(|n| (format!("{n} rows"), None, Some(*n))).collect(),
                };
                let mut list = Vec::new();
                for (i, (text, col, size)) in items.into_iter().enumerate() {
                    list.push(
                        div()
                            .id(("db-menu-item", i))
                            .px(px(10.))
                            .py(px(4.))
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::hover()))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |v, _, _, cx| {
                                if let Body::Data(d) = &mut v.body {
                                    d.menu = None;
                                    match m {
                                        Menu::Columns => d.spec.column = col.clone(),
                                        Menu::Sizes => d.spec.limit = size.unwrap_or(25),
                                    }
                                    d.spec.offset = 0;
                                }
                                v.load(cx);
                            }))
                            .child(text),
                    );
                }
                Some(deferred(
                    div()
                        .id("db-menu")
                        .debug_selector(|| "db-menu".into())
                        .absolute()
                        .top(px(34.))
                        .when(m == Menu::Columns, |d| d.right(px(110.)))
                        .when(m == Menu::Sizes, |d| d.right(px(0.)))
                        .w(px(200.))
                        .max_h(px(300.))
                        .overflow_y_scroll()
                        .py(px(4.))
                        .rounded(px(5.))
                        .bg(theme::bg_popup())
                        .border_1()
                        .border_color(theme::border_input())
                        .shadow_lg()
                        .children(list),
                ).with_priority(1))
            }
        };
        // The rows menu opens under its own button: in the search row, or (MongoDB) in the toolbar.
        let (menu, toolbar_menu) = if d.compass.is_some() { (None, menu) } else { (menu, None) };
        let body: gpui::AnyElement = match (&d.page, &d.grid) {
            (Some(Err(e)), _) => div().p(px(14.)).min_w_0().text_color(theme::git_deleted()).child(e.clone()).into_any_element(),
            (_, Some(gv)) if !gv.grid.columns.is_empty() => div().flex().flex_col().flex_1().min_h_0().child(self.render_grid(None, gv, kind != Kind::RabbitMq && kind != Kind::Redis, cx)).into_any_element(),
            (_, Some(gv)) => div().p(px(14.)).text_color(theme::fg_dim()).child(gv.grid.note.clone()).into_any_element(),
            _ => div().into_any_element(),
        };
        let note = d.grid.as_ref().map(|g| g.grid.note.clone()).filter(|n| !n.is_empty() && kind == Kind::Redis);
        div()
            .flex()
            .flex_col()
            .size_full()
            .px(px(20.))
            .pt(px(14.))
            .gap(px(10.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_size(px(17.)).text_color(theme::fg_bright()).child(format!("{what}: {name}")))
                    .when(kind == Kind::RabbitMq && what == "Queue", |d| d.child(button("db-peek", "Peek messages", Some("eye"), false).on_click(cx.listener(|v, _, _, cx| v.peek_messages(cx)))))
                    // While reading: Stop.
                    .child(button("db-refresh", if d.loading { "Stop" } else { "Refresh" }, Some(if d.loading { "stop" } else { "refresh" }), true).on_click(cx.listener(|v, _, _, cx| v.stop_or_load(cx))))
                    .when(pageable, |d2| d2.child(button("db-export", if d.exporting { "Exporting…" } else { "Export CSV" }, Some("arrow-down"), true).on_click(cx.listener(|v, _, _, cx| v.export_csv(cx))))),
            )
            .when_some(d.compass.as_deref(), |el, c| el.child(self.render_query_bar(c, cx)))
            .when(d.compass.is_none() && (searchable || pageable), |el| {
                el.child(
                    div()
                        .relative()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .when(searchable, |el| el.child(div().flex_1().h(px(28.)).px(px(8.)).flex().items_center().rounded(px(4.)).bg(theme::bg_input()).border_1().border_color(theme::border_input()).child(d.search.clone())))
                        .when(!searchable, |el| el.child(div().flex_1()))
                        .when(searchable && kind != Kind::Redis, |el| el.child(dropdown("db-columns", col_label, Menu::Columns, cx)))
                        .when(pageable, |el| el.child(dropdown("db-sizes", format!("{} rows", d.spec.limit), Menu::Sizes, cx)))
                        .children(menu),
                )
            })
            .when(!matches!(d.page, Some(Err(_))), |el| {
                let docs = d.grid.as_ref().is_some_and(|g| g.grid.trees.is_some());
                el.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .text_size(px(12.))
                        .text_color(theme::fg_dim())
                        .child(showing)
                        .when_some(note, |d, n| d.child(div().text_color(theme::fg_muted()).child(format!("· {n}"))))
                        .child(div().flex_1())
                        .when(d.compass.is_some(), |el| {
                            let limit = d.spec.limit.max(1);
                            let offset = d.spec.offset;
                            let last = pages.unwrap_or(1);
                            let arrow = |id: &'static str, icon: &'static str, on: bool| {
                                div().id(id).debug_selector(move || id.to_string()).p(px(4.)).rounded(px(4.)).when(on, |d| d.cursor_pointer().hover(|s| s.bg(theme::hover()))).when(!on, |d| d.opacity(0.35)).child(ui::small_icon(icon, theme::fg()))
                            };
                            el.child(div().relative().child(dropdown("db-sizes", format!("{}", d.spec.limit), Menu::Sizes, cx)).children(toolbar_menu))
                                .child(div().debug_selector(|| "db-range".into()).text_color(theme::fg()).child(match total {
                                    Some(0) | None if shown == 0 => "0 – 0 of 0".to_string(),
                                    Some(t) => format!("{} – {} of {}{}", group_digits((offset + 1) as u64), group_digits((offset + shown) as u64), if estimate { "≈ " } else { "" }, group_digits(t)),
                                    None => format!("{} – {}", offset + 1, offset + shown),
                                }))
                                .child(arrow("db-reload", "refresh", true).tooltip(ui::tooltip("Refresh")).on_click(cx.listener(|v, _, _, cx| v.load(cx))))
                                .child(arrow("db-prev", "chevron-left", page_no > 1).when(page_no > 1, |b| b.on_click(cx.listener(move |v, _, _, cx| v.set_page(offset.saturating_sub(limit), cx)))))
                                .child(arrow("db-next", "chevron-right", page_no < last).when(page_no < last, |b| b.on_click(cx.listener(move |v, _, _, cx| v.set_page(offset + limit, cx)))))
                        })
                        .when(docs || d.compass.is_some(), |el| el.child(mode_switch(d.doc_mode, cx))),
                )
            })
            .child(match d.compass.as_deref().and_then(|c| self.render_explain(c, cx)) {
                Some(explain) => explain,
                None => body,
            })
            .when(d.compass.is_none() && pageable && pages.is_some_and(|p| p > 1), |el| {
                let pages = pages.unwrap_or(1);
                let limit = d.spec.limit.max(1);
                let offset = d.spec.offset;
                el.child(
                    div()
                        .flex()
                        .flex_none()
                        .justify_center()
                        .items_center()
                        .gap(px(10.))
                        .pb(px(10.))
                        .child(button("db-prev", "Previous", Some("chevron-left"), false).when(page_no > 1, |b| b.on_click(cx.listener(move |v, _, _, cx| v.set_page(offset.saturating_sub(limit), cx)))).when(page_no <= 1, |b| b.opacity(0.4)))
                        .child(div().text_size(px(12.)).text_color(theme::fg_dim()).child(format!("Page {} of {}{}", group_digits(page_no as u64), if estimate { "≈ " } else { "" }, group_digits(pages as u64))))
                        .child(button("db-next", "Next", Some("chevron-right"), false).when(page_no < pages, |b| b.on_click(cx.listener(move |v, _, _, cx| v.set_page(offset + limit, cx)))).when(page_no >= pages, |b| b.opacity(0.4))),
                )
            })
            .when(d.compass.is_some() || !(pageable && pages.is_some_and(|p| p > 1)), |el| el.child(div().h(px(10.))))
            .into_any_element()
    }

    fn render_query(&self, d: &Data, cx: &mut Context<Self>) -> gpui::AnyElement {
        let scope = scope_of(d.conn.kind, &d.path);
        let where_ = match (&scope.database, &scope.collection) {
            (Some(db), Some(c)) => format!("{db} · {c}"),
            (Some(db), None) => db.clone(),
            _ => d.conn.title(),
        };
        let hint = match d.conn.kind {
            Kind::Postgres | Kind::SqlServer => "SQL — runs the selection, or everything",
            Kind::MongoDb => "db.collection.find({…}), updateOne, aggregate([…])…",
            Kind::Redis => "Commands, one per line",
            Kind::RabbitMq => "API calls: GET /api/queues, or POST /api/… with a JSON body below",
        };
        let status: String = match (&d.running, &d.query_error, d.results.get(d.result_ix)) {
            (Some(t), _, _) => format!("Running… {:.1}s", t.elapsed().as_secs_f32()),
            (_, Some(_), _) => "Failed".into(),
            (_, _, Some(g)) => {
                let n = g.grid.rows.len();
                let rows = if g.grid.columns.is_empty() { g.grid.note.clone() } else { format!("{n}{} row{}", if g.grid.more { "+" } else { "" }, if n == 1 { "" } else { "s" }) };
                format!("{rows}{}", d.took.map(|t| format!(" · {} ms", t.as_millis())).unwrap_or_default())
            }
            _ => String::new(),
        };
        let results: gpui::AnyElement = match (&d.query_error, d.results.get(d.result_ix)) {
            (Some(e), _) => div().debug_selector(|| "db-query-error".into()).p(px(12.)).text_color(theme::git_deleted()).child(e.clone()).into_any_element(),
            (_, Some(gv)) if !gv.grid.columns.is_empty() => div()
                .flex()
                .flex_1()
                .min_h_0()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(self.render_grid(Some(d.result_ix), gv, false, cx))
                        .when(gv.grid.more, |el| el.child(div().px(px(10.)).py(px(4.)).text_size(px(11.)).text_color(theme::fg_muted()).child(format!("Showing the first {} rows; add a LIMIT (or a filter) for others.", db::MAX_ROWS)))),
                )
                .into_any_element(),
            (_, Some(gv)) => div().debug_selector(|| "db-query-note".into()).p(px(12.)).text_color(theme::fg()).child(gv.grid.note.clone()).into_any_element(),
            _ => div().p(px(12.)).text_color(theme::fg_muted()).child("Run a query with ⌘↵").into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(div().h(px(220.)).flex_none().border_b_1().border_color(theme::border()).child(d.query.clone()))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(10.))
                    .px(px(12.))
                    .h(px(38.))
                    .border_b_1()
                    .border_color(theme::border())
                    .child(button("db-run", if d.running.is_some() { "Stop" } else { "Run" }, Some(if d.running.is_some() { "stop" } else { "arrow-right" }), true).on_click(cx.listener(|v, _, _, cx| v.stop_or_run(cx))))
                    .child(div().text_size(px(11.)).text_color(theme::fg_muted()).child("⌘↵"))
                    .child(div().text_size(px(12.)).text_color(theme::fg_dim()).child(format!("on {where_}")))
                    .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_size(px(11.)).text_color(theme::fg_muted()).child(hint))
                    .when(d.results.get(d.result_ix).is_some_and(|g| g.grid.trees.is_some()), |el| el.child(mode_switch(d.doc_mode, cx)))
                    .child(div().debug_selector(|| "db-query-status".into()).text_size(px(12.)).text_color(if d.query_error.is_some() { theme::git_deleted() } else { theme::fg_dim() }).child(status)),
            )
            .when(d.results.len() > 1, |el| {
                el.child(div().flex().flex_none().gap(px(2.)).px(px(8.)).pt(px(4.)).children(d.results.iter().enumerate().map(|(i, g)| {
                    let on = i == d.result_ix;
                    let text = if g.grid.columns.is_empty() { format!("{} · {}", i + 1, g.grid.note) } else { format!("Result {}", i + 1) };
                    div()
                        .id(("db-result-tab", i))
                        .px(px(10.))
                        .py(px(3.))
                        .rounded_t(px(4.))
                        .text_size(px(12.))
                        .cursor_pointer()
                        .when(on, |d| d.bg(theme::selected_focused()).text_color(theme::fg_bright()))
                        .when(!on, |d| d.text_color(theme::fg_dim()).hover(|s| s.bg(theme::hover())))
                        .on_click(cx.listener(move |v, _, _, cx| {
                            if let Body::Data(d) = &mut v.body {
                                d.result_ix = i;
                            }
                            cx.notify();
                        }))
                        .child(text)
                })))
            })
            .child(results)
            .into_any_element()
    }
}

/// A value in a table cell, the way MongoDB Compass shows it.
fn table_text(t: &Tree) -> String {
    match t {
        Tree::Str(s) => format!("\"{}\"", s.replace(['\n', '\r', '\t'], " ")),
        other => other.short(TextStyle::Shell),
    }
}

/// A value's color, by its type (the theme's syntax colors).
fn tree_color(t: &Tree) -> gpui::Hsla {
    use crate::theme::Syntax;
    match t {
        Tree::Str(_) => theme::syntax(Syntax::String),
        Tree::Num(_) | Tree::Typed("Double" | "Long" | "Decimal128", _) => theme::syntax(Syntax::Number),
        Tree::Typed("Date" | "Timestamp", _) => theme::syntax(Syntax::Type),
        Tree::Typed(_, _) => theme::syntax(Syntax::Function),
        Tree::Bool(_) | Tree::Null => theme::syntax(Syntax::Keyword),
        Tree::Array(_) | Tree::Object(_) => theme::fg_dim(),
    }
}

/// A long value on one line: it scrolls sideways in its own box instead of wrapping.
fn one_line(id: impl Into<gpui::ElementId>, child: gpui::Div) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_1()
        .min_w_0()
        .overflow_x_scroll()
        .whitespace_nowrap()
        // A sideways swipe stays in the value; an up/down one goes on to the list around it.
        .on_scroll(|ev, window, cx| {
            let d = ev.delta.pixel_delta(window.line_height());
            if d.x.abs() > d.y.abs() {
                cx.stop_propagation();
            }
        })
        .child(child)
}

fn copy_button(id: impl Into<gpui::ElementId>, text: String) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(20.))
        .rounded(px(3.))
        .cursor_pointer()
        .hover(|s| s.bg(theme::hover()))
        .tooltip(ui::tooltip("Copy"))
        .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(text.clone())))
        .child(ui::small_icon("copy", theme::fg_dim()))
}

/// Each column's type, from the documents (Mixed when they differ, Null ones aside).
fn column_types(g: &Grid) -> Vec<String> {
    let Some(trees) = &g.trees else { return Vec::new() };
    g.columns
        .iter()
        .map(|c| {
            let mut ty: Option<&str> = None;
            for t in trees {
                if let Tree::Object(f) = t
                    && let Some((_, v)) = f.iter().find(|(k, _)| k == c)
                    && !matches!(v, Tree::Null)
                {
                    match ty {
                        None => ty = Some(v.type_name()),
                        Some(prev) if prev != v.type_name() => return "Mixed".to_string(),
                        _ => {}
                    }
                }
            }
            ty.unwrap_or("Null").to_string()
        })
        .collect()
}

/// List · JSON · Table, for MongoDB documents.
fn mode_switch(current: DocMode, cx: &mut Context<DbView>) -> gpui::Div {
    let item = |id: &'static str, icon: &'static str, tip: &'static str, mode: DocMode, cx: &mut Context<DbView>| {
        let on = current == mode;
        div()
            .id(id)
            .debug_selector(move || id.to_string())
            .flex()
            .items_center()
            .justify_center()
            .w(px(30.))
            .h(px(22.))
            .rounded(px(3.))
            .cursor_pointer()
            .tooltip(ui::tooltip(tip))
            .when(on, |d| d.bg(theme::accent()))
            .when(!on, |d| d.hover(|s| s.bg(theme::hover())))
            .on_click(cx.listener(move |v, _, _, cx| v.set_doc_mode(mode, cx)))
            .child(ui::small_icon(icon, if on { theme::on(theme::accent()) } else { theme::fg_dim() }))
    };
    div()
        .flex()
        .flex_none()
        .gap(px(2.))
        .p(px(2.))
        .rounded(px(4.))
        .border_1()
        .border_color(theme::border_input())
        .child(item("db-mode-list", "list-flat", "List", DocMode::List, cx))
        .child(item("db-mode-json", "braces", "JSON", DocMode::Json, cx))
        .child(item("db-mode-table", "table", "Table", DocMode::Table, cx))
}

fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

impl Focusable for DbView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for DbView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body: gpui::AnyElement = match &self.body {
            Body::Form(f) => self.render_form(f, cx),
            Body::Overview(o) => self.render_overview(o, cx),
            Body::Missing(m) => div().p(px(24.)).text_color(theme::fg_dim()).child(m.clone()).into_any_element(),
            Body::Data(d) => {
                let tab = |id: &'static str, text: &'static str, icon: &'static str, which: Inner, cx: &mut Context<Self>| {
                    let on = d.inner == which;
                    div()
                        .id(id)
                        .debug_selector(move || id.to_string())
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .px(px(16.))
                        .h_full()
                        .cursor_pointer()
                        .when(on, |d| d.border_b_2().border_color(theme::accent()).text_color(theme::fg_bright()))
                        .when(!on, |d| d.text_color(theme::fg_dim()).hover(|s| s.text_color(theme::fg())))
                        .on_click(cx.listener(move |v, _, window, cx| {
                            if let Body::Data(d) = &mut v.body {
                                d.inner = which;
                                if which == Inner::Query {
                                    window.focus(&d.query.read(cx).focus_target(cx), cx);
                                }
                            }
                            cx.notify();
                        }))
                        .child(ui::small_icon(icon, if on { theme::accent() } else { theme::fg_dim() }))
                        .child(text)
                };
                let inner = match (d.inner, d.compass.as_deref()) {
                    (Inner::DataView, Some(c)) => self.render_compass_tab(d, c, cx),
                    (Inner::DataView, None) => self.render_data_view(d, cx),
                    (Inner::Query, _) => self.render_query(d, cx),
                };
                div()
                    .flex()
                    .flex_col()
                    .size_full()
                    .when_some(d.compass.as_deref(), |el, c| el.child(self.render_compass_tabs(d, c, cx)))
                    .when(d.has_data && d.compass.is_none(), |el| {
                        el.child(
                            div()
                                .flex()
                                .flex_none()
                                .h(px(36.))
                                .px(px(8.))
                                .border_b_1()
                                .border_color(theme::border())
                                .text_size(px(13.))
                                .child(tab("db-tab-data", "Data View", "table", Inner::DataView, cx))
                                .child(tab("db-tab-query", "Query", "terminal", Inner::Query, cx)),
                        )
                    })
                    .child(div().flex_1().min_h_0().child(inner))
                    .into_any_element()
            }
        };
        div()
            .key_context("DbView")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::run))
            .on_action(cx.listener(Self::copy_cell))
            .on_action(cx.listener(Self::select_all_rows))
            .on_action(cx.listener(Self::refresh_data))
            .on_action(cx.listener(Self::row_up))
            .on_action(cx.listener(Self::row_down))
            .on_action(cx.listener(Self::close_detail))
            // A column's edge dragged: its width follows the pointer.
            .on_mouse_move(cx.listener(|v, ev: &gpui::MouseMoveEvent, _, cx| {
                let Some((which, col, x0, w0)) = v.col_drag else { return };
                if !ev.dragging() {
                    v.col_drag = None;
                    return;
                }
                if let Some(gv) = v.grid_of_mut(which) {
                    gv.widths[col] = (w0 + (ev.position.x - x0) / px(1.)).max(48.);
                }
                cx.notify();
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|v, _, _, _| v.col_drag = None))
            // A press anywhere starts afresh: the old selection goes (a drag makes a new one).
            .capture_any_mouse_down(cx.listener(|v, _, _, cx| {
                if v.sel.has_selection() {
                    v.sel.clear();
                    cx.notify();
                }
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(|v, _, _, cx| {
                if let Body::Data(d) = &mut v.body {
                    let history = d.compass.as_deref_mut().map(|c| std::mem::take(&mut c.history)).unwrap_or(false);
                    if d.menu.take().is_some() || history {
                        cx.notify();
                    }
                }
            }))
            .size_full()
            .bg(theme::bg_editor())
            .text_color(theme::fg())
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn queries_that_change_data_are_spotted() {
        use crate::db::Kind;
        assert!(super::destructive(Kind::MongoDb, "db.users.find({})\ndb.users.deleteMany({})").unwrap().contains("deleteMany"));
        assert!(super::destructive(Kind::MongoDb, "db.dropDatabase()").is_some());
        assert!(super::destructive(Kind::MongoDb, "[{$match: {}}, {$out: 'copy'}]").is_some());
        assert!(super::destructive(Kind::MongoDb, "db.users.find({status: 'drop'})").is_none());
        assert!(super::destructive(Kind::Postgres, "SELECT updated_at FROM t;\nDELETE FROM t").is_some());
        assert!(super::destructive(Kind::Postgres, "select update_count from t").is_none(), "a column name isn't a statement");
        assert!(super::destructive(Kind::Postgres, "-- drop table t\nselect 1").is_none(), "comments don't count");
        assert!(super::destructive(Kind::Redis, "FLUSHDB").is_some());
    }

    #[test]
    fn columns_keep_their_order() {
        let mut g = crate::db::Grid { columns: vec!["_id".into(), "zzz".into(), "n".into()], rows: vec![vec![Some("1".into()), Some("z".into()), Some("5".into())]], ..Default::default() };
        super::keep_column_order(&["_id".into(), "n".into(), "name".into()], &mut g);
        assert_eq!(g.columns, ["_id", "n", "zzz"]);
        assert_eq!(g.rows[0], [Some("1".into()), Some("5".into()), Some("z".into())]);
    }

    #[test]
    fn counts_read_easily() {
        assert_eq!(super::group_digits(1234567), "1,234,567");
        assert_eq!(super::group_digits(999), "999");
    }
}

