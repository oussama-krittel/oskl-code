//! Database tabs: the connect form, a table's (collection's, key's, queue's) Data View and
//! Query, and query tabs. Results show in a grid that draws only the rows on screen.

use crate::db::{self, Conn, Grid, Kind, Page, PageSpec, Scope, TextStyle, Tree, engine};
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

actions!(db_view, [RunQuery, CopyCell, RefreshData, RowUp, RowDown, CloseDetail]);

pub fn bind_keys() {
    crate::keybindings::collect([
        crate::keybindings::kb("cmd-enter", RunQuery, Some("DbView")),
        crate::keybindings::kb("cmd-enter", RunQuery, Some("DbView > Editor")),
        crate::keybindings::kb("cmd-c", CopyCell, Some("DbView")),
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
}

enum Body {
    Form(Box<Form>),
    Data(Box<Data>),
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
    widths: Vec<f32>,
    types: Vec<String>,
    list: ListState,
    hscroll: ScrollHandle,
    sel: Option<(usize, usize)>,
    open: Option<usize>,
    /// "<row>/<field>/…" of nested values shown open.
    unfolded: HashSet<String>,
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
        let widths = widths.into_iter().enumerate().map(|(c, w): (usize, f32)| {
            let head = grid.columns[c].chars().count() + types.get(c).map(|t| t.len() + 1).unwrap_or(0) + 3;
            w.max((head as f32 * 7.6 + 26.).min(420.))
        }).collect();
        let list = ListState::new(grid.rows.len(), ListAlignment::Top, gpui::px(600.));
        GridView { grid: Arc::new(grid), widths, types, list, hscroll: ScrollHandle::new(), sel: None, open: None, unfolded: HashSet::new() }
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
            Open::Data { conn, path } | Open::Query { conn, path } => match find(conn) {
                Some(c) => {
                    let has_data = matches!(open, Open::Data { .. });
                    let starter = db::starter_query(c.kind, path);
                    let search = input("Search…", "", cx);
                    let query = cx.new(|cx| {
                        let mut ed = Editor::new(EditorMode::Full, cx);
                        ed.set_text(&starter, cx);
                        ed
                    });
                    subs.push(cx.subscribe_in(&search, window, |v: &mut DbView, _, ev: &EditorEvent, _, cx| match ev {
                        EditorEvent::Submit => v.search_now(cx),
                        EditorEvent::Changed => v.search_soon(cx),
                        _ => {}
                    }));
                    let data = Data {
                        conn: c,
                        path: path.clone(),
                        has_data,
                        inner: if has_data { Inner::DataView } else { Inner::Query },
                        spec: PageSpec { limit: PAGE_SIZES[0], ..Default::default() },
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
                    };
                    Body::Data(Box::new(data))
                }
                None => Body::Missing("This connection was deleted.".into()),
            },
        };
        let mut v = DbView { focus: cx.focus_handle(), open, body, _subs: subs };
        v.load(cx);
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
            Body::Missing(_) => ("Database".into(), String::new()),
        }
    }


    /// Its type (the form's follows the type chosen).
    pub fn kind(&self) -> Option<Kind> {
        match &self.body {
            Body::Form(f) => Some(f.conn.kind),
            Body::Data(d) => Some(d.conn.kind),
            Body::Missing(_) => None,
        }
    }

    /// The connection it uses (data and query tabs).
    pub fn conn_id(&self) -> Option<String> {
        match &self.body {
            Body::Data(d) => Some(d.conn.id.clone()),
            _ => None,
        }
    }

    /// Its connection was connected: loads what it shows.
    pub fn reconnected(&mut self, cx: &mut Context<Self>) {
        let waiting = matches!(&self.body, Body::Data(d) if d.page.as_ref().is_none_or(|p| p.is_err()));
        if waiting {
            self.load(cx);
        }
    }

    /// Whether this tab shows `open` (to switch to it instead of opening another).
    pub fn shows(&self, open: &Open) -> bool {
        matches!(open, Open::Edit(_) | Open::Data { .. }) && &self.open == open
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

    fn load(&mut self, cx: &mut Context<Self>) {
        let Body::Data(d) = &mut self.body else { return };
        if !d.has_data {
            return;
        }
        d.loading = true;
        let work = engine::run(engine::page(d.conn.clone(), d.path.clone(), d.spec.clone()));
        d.task = Some(cx.spawn(async move |this, cx| {
            let r = work.await;
            this.update(cx, |v, cx| {
                if let Body::Data(d) = &mut v.body {
                    d.loading = false;
                    let keep_cols = d.grid.as_ref().map(|g| g.grid.columns.clone());
                    d.grid = r.as_ref().ok().map(|p| {
                        let mut g = GridView::new(p.grid.clone());
                        // Same columns (another page, a search): keep their widths, so nothing jumps.
                        if keep_cols.as_ref() == Some(&p.grid.columns)
                            && let Some(old) = &d.grid
                        {
                            g.widths = old.widths.clone();
                        }
                        g
                    });
                    d.page = Some(r);
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
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
            self.load(cx);
        }
    }

    fn refresh_data(&mut self, _: &RefreshData, _: &mut Window, cx: &mut Context<Self>) {
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

    fn run(&mut self, _: &RunQuery, _: &mut Window, cx: &mut Context<Self>) {
        self.run_query(&RunQuery, &mut None, cx);
    }

    fn run_query(&mut self, _: &RunQuery, _: &mut Option<()>, cx: &mut Context<Self>) {
        let Body::Data(d) = &mut self.body else { return };
        if d.running.is_some() {
            return;
        }
        d.inner = Inner::Query;
        // The selection if there is one, else everything.
        let ed = d.query.read(cx);
        let sel = ed.buf.sel.range();
        let text = if sel.is_empty() { ed.text() } else { ed.buf.slice(sel) };
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
                            d.results = grids.into_iter().map(GridView::new).collect();
                            // The last result with rows is the interesting one.
                            d.result_ix = d.results.iter().rposition(|g| !g.grid.columns.is_empty()).unwrap_or(d.results.len().saturating_sub(1));
                        }
                        Err(e) => d.query_error = Some(e),
                    }
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
        let Body::Data(d) = &mut self.body else { return };
        d.doc_mode = mode;
        for gv in d.grid.iter_mut().chain(d.results.iter_mut()) {
            gv.list.reset(gv.grid.rows.len());
        }
        cx.notify();
    }

    fn copy_cell(&mut self, _: &CopyCell, _: &mut Window, cx: &mut Context<Self>) {
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
        let body = list(gv.list.clone(), cx.processor(move |v: &mut DbView, ix: usize, _w, cx| v.render_item(which, ix, cx))).with_sizing_behavior(gpui::ListSizingBehavior::Auto).flex_1();
        if mode != DocMode::Table {
            return div().id(SharedString::from(format!("db-docs-{}", which.map(|i| i.to_string()).unwrap_or_default()))).debug_selector(|| "db-grid".into()).flex().flex_col().flex_1().min_h_0().child(body).into_any_element();
        }
        let g = gv.grid.clone();
        let total_w: f32 = gv.widths.iter().sum::<f32>() + 44.;
        let sort = match &self.body {
            Body::Data(d) if sortable => d.spec.sort.clone(),
            _ => None,
        };
        let header = div()
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
            }));
        div()
            .id(SharedString::from(format!("db-hscroll-{}", which.map(|i| i.to_string()).unwrap_or_default())))
            .debug_selector(|| "db-grid".into())
            .flex_1()
            .min_h_0()
            .overflow_x_scroll()
            .track_scroll(&gv.hscroll)
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
            Some(i) => d.results.get(i),
            None => d.grid.as_ref(),
        }
    }

    fn grid_of_mut(&mut self, which: Option<usize>) -> Option<&mut GridView> {
        let Body::Data(d) = &mut self.body else { return None };
        match which {
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
        let Some(row) = grid.rows.get(r) else { return div().into_any_element() };
        let tree = grid.trees.as_ref().and_then(|t| t.get(r));
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
                    .on_click(cx.listener(move |v, _: &ClickEvent, window, cx| {
                        window.focus(&v.focus, cx);
                        v.toggle_row(which, r, Some(c), cx);
                    }))
                    .child(div().min_w_0().overflow_hidden().text_ellipsis().child(text))
            }));
        if open != Some(r) {
            return line.into_any_element();
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
                            Some(v) => div().text_color(theme::fg_bright()).child(v.replace(['\n', '\r'], " ⏎ ")),
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
                    .w_full()
                    .px(px(52.))
                    .py(px(8.))
                    .border_b_1()
                    .border_color(theme::border())
                    .bg(theme::bg_side())
                    .text_size(px(12.5))
                    .children(detail),
            )
            .into_any_element()
    }

    /// A MongoDB document as a card: its fields one per line (List), or as JSON.
    fn render_card(&mut self, which: Option<usize>, r: usize, json: bool, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(gv) = self.grid_of(which) else { return div().into_any_element() };
        let Some(t) = gv.grid.trees.as_ref().and_then(|t| t.get(r)).cloned() else { return div().into_any_element() };
        let copy = t.text(if json { TextStyle::Json } else { TextStyle::Shell });
        let lines = self.tree_lines(which, r, &t, json, cx);
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
                    .child(div().absolute().top(px(6.)).right(px(8.)).invisible().group_hover("db-card", |s| s.visible()).child(copy_button(("db-card-copy", r), copy))),
            )
            .into_any_element()
    }

    /// A document's fields as lines: `key: value`, nested objects and arrays folded until
    /// clicked (List), or as JSON with its brackets.
    fn tree_lines(&self, which: Option<usize>, row: usize, t: &Tree, json: bool, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let Some(gv) = self.grid_of(which) else { return Vec::new() };
        let style = if json { TextStyle::Json } else { TextStyle::Shell };
        let mut out = Vec::new();
        let mut n = 0usize;
        fn walk(v: &DbView, which: Option<usize>, row: usize, entries: Vec<(String, &Tree)>, path: &str, depth: usize, json: bool, style: TextStyle, unfolded: &HashSet<String>, out: &mut Vec<gpui::AnyElement>, n: &mut usize, cx: &mut Context<DbView>) {
            let count = entries.len();
            for (i, (key, val)) in entries.into_iter().enumerate() {
                let p = format!("{path}/{key}");
                let container = matches!(val, Tree::Array(a) if !a.is_empty()) || matches!(val, Tree::Object(f) if !f.is_empty());
                let open = container && unfolded.contains(&format!("{row}{p}"));
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
                out.push(
                    div()
                        .id(("db-tree-line", id))
                        .flex()
                        .items_center()
                        .h(px(21.))
                        .pl(px(depth as f32 * 18.))
                        .whitespace_nowrap()
                        .when(container, |d| d.cursor_pointer().on_click(cx.listener(move |v, _, _, cx| v.toggle_fold(which, row, fold.clone(), cx))))
                        .child(div().w(px(14.)).flex_none().text_color(theme::fg_muted()).child(if container { if open { "▾" } else { "▸" } } else { "" }))
                        .when(!key_text.is_empty(), |d| d.child(div().flex_none().font_weight(FontWeight::SEMIBOLD).text_color(theme::fg_bright()).child(format!("{key_text}: "))))
                        .child(one_line(("db-tree-value", id), div().text_color(color).child(summary)))
                        .into_any_element(),
                );
                if open {
                    let children: Vec<(String, &Tree)> = match val {
                        // Array items by index (no key shown in JSON).
                        Tree::Array(a) => a.iter().enumerate().map(|(i, x)| (if json { format!("\u{0}{i}") } else { format!("\u{1}{i}") }, x)).collect(),
                        Tree::Object(f) => f.iter().map(|(k, x)| (k.clone(), x)).collect(),
                        _ => Vec::new(),
                    };
                    walk(v, which, row, children, &p, depth + 1, json, style, unfolded, out, n, cx);
                    if json {
                        *n += 1;
                        out.push(div().h(px(21.)).pl(px(depth as f32 * 18. + 14.)).text_color(theme::fg_dim()).child(format!("{}{comma}", if matches!(val, Tree::Array(_)) { "]" } else { "}" })).into_any_element());
                    }
                }
            }
        }
        let Tree::Object(fields) = t else { return out };
        let unfolded = gv.unfolded.clone();
        if json {
            out.push(div().h(px(21.)).text_color(theme::fg_dim()).child("{").into_any_element());
        }
        walk(self, which, row, fields.iter().map(|(k, v)| (k.clone(), v)).collect(), "", if json { 1 } else { 0 }, json, style, &unfolded, &mut out, &mut n, cx);
        if json {
            out.push(div().h(px(21.)).text_color(theme::fg_dim()).child("}").into_any_element());
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
                    .child(button("db-refresh", if d.loading { "Loading…" } else { "Refresh" }, Some("refresh"), true).on_click(cx.listener(|v, _, _, cx| v.load(cx))))
                    .when(pageable, |d2| d2.child(button("db-export", if d.exporting { "Exporting…" } else { "Export CSV" }, Some("arrow-down"), true).on_click(cx.listener(|v, _, _, cx| v.export_csv(cx))))),
            )
            .when(searchable || pageable, |el| {
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
                        .when(docs, |el| el.child(mode_switch(d.doc_mode, cx))),
                )
            })
            .child(body)
            .when(pageable && pages.is_some_and(|p| p > 1), |el| {
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
            .when(!(pageable && pages.is_some_and(|p| p > 1)), |el| el.child(div().h(px(10.))))
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
                    .child(button("db-run", if d.running.is_some() { "Running…" } else { "Run" }, Some("arrow-right"), true).on_click(cx.listener(|v, _, _, cx| v.run_query(&RunQuery, &mut None, cx))))
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
                let inner = match d.inner {
                    Inner::DataView => self.render_data_view(d, cx),
                    Inner::Query => self.render_query(d, cx),
                };
                div()
                    .flex()
                    .flex_col()
                    .size_full()
                    .when(d.has_data, |el| {
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
            .on_action(cx.listener(Self::refresh_data))
            .on_action(cx.listener(Self::row_up))
            .on_action(cx.listener(Self::row_down))
            .on_action(cx.listener(Self::close_detail))
            .on_mouse_down(MouseButton::Left, cx.listener(|v, _, _, cx| {
                if let Body::Data(d) = &mut v.body
                    && d.menu.is_some()
                {
                    d.menu = None;
                    cx.notify();
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
    fn counts_read_easily() {
        assert_eq!(super::group_digits(1234567), "1,234,567");
        assert_eq!(super::group_digits(999), "999");
    }
}
