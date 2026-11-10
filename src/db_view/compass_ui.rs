//! A MongoDB collection's tab, the way Compass shows it: Documents (a query bar with its
//! options, Explain, history), Aggregations, Schema, Indexes and Validation.

use super::{Body, Data, DbView, GridView, Inner, button, input};
use crate::db::{self, FindQuery, Scope, compass, engine};
use crate::editor::{Editor, EditorEvent, EditorMode};
use crate::request::Request;
use crate::theme;
use crate::ui::{self, px};
use gpui::prelude::*;
use gpui::{App, Context, Entity, FontWeight, SharedString, Subscription, Task, Window, deferred, div};
use std::path::PathBuf;
use std::time::Instant;

/// The aggregation's results, as a grid of its own (`which` for the grid functions).
pub(super) const AGG: usize = usize::MAX;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum CTab {
    Documents,
    Aggregations,
    Schema,
    Indexes,
    Validation,
}

pub(super) struct Compass {
    pub(super) tab: CTab,
    filter: Entity<Editor>,
    project: Entity<Editor>,
    sort: Entity<Editor>,
    collation: Entity<Editor>,
    hint: Entity<Editor>,
    max_time: Entity<Editor>,
    skip: Entity<Editor>,
    limit: Entity<Editor>,
    pub(super) options: bool,
    pub(super) history: bool,
    bar_error: Option<String>,
    pub(super) explain: Option<Result<compass::Explain, String>>,
    explain_raw: bool,
    explain_task: Option<Task<()>>,
    // Aggregations
    pipeline: Entity<Editor>,
    pub(super) agg: Option<GridView>,
    agg_error: Option<String>,
    agg_running: Option<Instant>,
    agg_took: Option<std::time::Duration>,
    agg_task: Option<Task<()>>,
    // Schema
    schema: Option<Result<compass::Schema, String>>,
    schema_task: Option<Task<()>>,
    // Indexes
    indexes: Option<Result<Vec<compass::Index>, String>>,
    idx_task: Option<Task<()>>,
    new_index: bool,
    idx_keys: Entity<Editor>,
    idx_options: Entity<Editor>,
    idx_status: Option<Result<String, String>>,
    // Validation
    validator: Entity<Editor>,
    level: String,
    action: String,
    val_loaded: Option<Result<(), String>>,
    val_status: Option<Result<String, String>>,
    val_task: Option<Task<()>>,
}

impl Compass {
    pub(super) fn new(names: &super::assist::Shared, subs: &mut Vec<Subscription>, window: &mut Window, cx: &mut Context<DbView>) -> Compass {
        let filter = input("Type a query: { field: 'value' }", "", cx);
        let project = input("{ field: 0 }", "", cx);
        let sort = input("{ field: -1 } or [['field', -1]]", "", cx);
        let collation = input("{ locale: 'simple' }", "", cx);
        let hint = input("{ field: -1 } or \"indexName\"", "", cx);
        let max_time = input("60000", "", cx);
        let skip = input("0", "", cx);
        let limit = input("0", "", cx);
        // Enter in any field of the bar finds.
        for e in [&filter, &project, &sort, &collation, &hint, &max_time, &skip, &limit] {
            subs.push(cx.subscribe_in(e, window, |v: &mut DbView, _, ev: &EditorEvent, _, cx| {
                if matches!(ev, EditorEvent::Submit) {
                    v.find(cx);
                }
            }));
        }
        let pipeline = cx.new(|cx| {
            let mut ed = Editor::new(EditorMode::Full, cx);
            ed.set_text("[\n  { $match: {} }\n]", cx);
            ed
        });
        let validator = cx.new(|cx| Editor::new(EditorMode::Full, cx));
        // Colored as the shell's JavaScript, with suggestions; brackets and quotes close themselves.
        let suggest = super::assist::mongo(names.clone());
        let idx_keys = input("{ email: 1 }", "", cx);
        let idx_options = input("{ unique: true, expireAfterSeconds: 3600, partialFilterExpression: {…} }", "", cx);
        for e in [&filter, &project, &sort, &collation, &hint, &pipeline, &validator, &idx_keys, &idx_options] {
            e.update(cx, |e, cx| {
                e.set_language("js", cx);
                e.suggest = Some(suggest.clone());
                e.pairs = true;
            });
        }
        Compass {
            tab: CTab::Documents,
            filter,
            project,
            sort,
            collation,
            hint,
            max_time,
            skip,
            limit,
            options: false,
            history: false,
            bar_error: None,
            explain: None,
            explain_raw: false,
            explain_task: None,
            pipeline,
            agg: None,
            agg_error: None,
            agg_running: None,
            agg_took: None,
            agg_task: None,
            schema: None,
            schema_task: None,
            indexes: None,
            idx_task: None,
            new_index: false,
            idx_keys,
            idx_options,
            idx_status: None,
            validator,
            level: "strict".into(),
            action: "error".into(),
            val_loaded: None,
            val_status: None,
            val_task: None,
        }
    }

    /// The bar as typed.
    fn query(&self, cx: &App) -> FindQuery {
        let t = |e: &Entity<Editor>| e.read(cx).text().trim().to_string();
        FindQuery {
            filter: t(&self.filter),
            project: t(&self.project),
            sort: t(&self.sort),
            collation: t(&self.collation),
            hint: t(&self.hint),
            max_time_ms: t(&self.max_time),
            skip: t(&self.skip),
            limit: t(&self.limit),
        }
    }

    fn fill(&self, q: &FindQuery, cx: &mut App) {
        for (e, text) in [(&self.filter, &q.filter), (&self.project, &q.project), (&self.sort, &q.sort), (&self.collation, &q.collation), (&self.hint, &q.hint), (&self.max_time, &q.max_time_ms), (&self.skip, &q.skip), (&self.limit, &q.limit)] {
            e.update(cx, |e, cx| e.set_text(text, cx));
        }
    }

    pub(super) fn focus_target(&self, cx: &App) -> Option<gpui::FocusHandle> {
        match self.tab {
            CTab::Aggregations => Some(self.pipeline.read(cx).focus_target(cx)),
            CTab::Validation => Some(self.validator.read(cx).focus_target(cx)),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------- query history

#[derive(serde::Serialize, serde::Deserialize, Clone, PartialEq)]
struct Recent {
    conn: String,
    db: String,
    coll: String,
    q: FindQuery,
}

fn history_file() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/oskl/mongo-queries.json"))
}

fn read_history() -> Vec<Recent> {
    history_file().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// Remembers a query (newest first; 20 per collection, 300 in all).
fn remember(r: Recent) {
    let mut all = read_history();
    all.retain(|x| *x != r);
    all.insert(0, r.clone());
    let mut per = 0;
    all.retain(|x| {
        if x.conn == r.conn && x.db == r.db && x.coll == r.coll {
            per += 1;
            per <= 20
        } else {
            true
        }
    });
    all.truncate(300);
    if let Some(p) = history_file() {
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        std::fs::write(p, serde_json::to_string(&all).unwrap_or_default()).ok();
    }
}

/// One line for a query in the history: its filter, then the options that are set.
fn summary(q: &FindQuery) -> String {
    let mut parts = vec![if q.filter.trim().is_empty() { "{}".to_string() } else { q.filter.clone() }];
    for (name, v) in [("project", &q.project), ("sort", &q.sort), ("collation", &q.collation), ("hint", &q.hint), ("maxTimeMS", &q.max_time_ms), ("skip", &q.skip), ("limit", &q.limit)] {
        if !v.trim().is_empty() {
            parts.push(format!("{name} {v}"));
        }
    }
    parts.join(" · ")
}

// ---------------------------------------------------------------- actions

impl DbView {
    #[cfg(test)]
    fn compass(&self) -> Option<&Compass> {
        match &self.body {
            Body::Data(d) => d.compass.as_deref(),
            _ => None,
        }
    }

    fn compass_mut(&mut self) -> Option<&mut Compass> {
        match &mut self.body {
            Body::Data(d) => d.compass.as_deref_mut(),
            _ => None,
        }
    }

    /// The connection, database and collection, for a call.
    fn target(&self) -> Option<(db::Conn, String, String)> {
        let Body::Data(d) = &self.body else { return None };
        let [db, coll] = d.path.as_slice() else { return None };
        Some((d.conn.clone(), db.clone(), coll.clone()))
    }

    /// Find: the bar's query, from the first page.
    pub(super) fn find(&mut self, cx: &mut Context<Self>) {
        let Body::Data(d) = &mut self.body else { return };
        let Some(c) = d.compass.as_deref_mut() else { return };
        let q = c.query(cx);
        c.history = false;
        c.explain = None;
        if let Err(e) = compass::check(&q) {
            c.bar_error = Some(e);
            return cx.notify();
        }
        c.bar_error = None;
        if !q.is_empty() {
            let [db, coll] = d.path.as_slice() else { return };
            remember(Recent { conn: d.conn.id.clone(), db: db.clone(), coll: coll.clone(), q: q.clone() });
        }
        d.spec.mongo = Some(q);
        d.spec.offset = 0;
        self.load(cx);
    }

    /// Reset: an empty bar, every document.
    fn reset_bar(&mut self, cx: &mut Context<Self>) {
        if let Some(c) = self.compass_mut() {
            let empty = FindQuery::default();
            c.fill(&empty, cx);
            c.bar_error = None;
            c.explain = None;
        }
        self.find(cx);
    }

    fn explain(&mut self, cx: &mut Context<Self>) {
        let Some((conn, db, coll)) = self.target() else { return };
        let Some(c) = self.compass_mut() else { return };
        let q = c.query(cx);
        if let Err(e) = compass::check(&q) {
            c.bar_error = Some(e);
            return cx.notify();
        }
        c.bar_error = None;
        c.explain_raw = false;
        let work = engine::run(engine::mongo(conn, move |cl| async move { compass::explain(&cl, &db, &coll, &q).await }));
        c.explain_task = Some(cx.spawn(async move |this, cx| {
            let r = work.await;
            this.update(cx, |v, cx| {
                if let Some(c) = v.compass_mut() {
                    c.explain = Some(r);
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub(super) fn run_pipeline(&mut self, cx: &mut Context<Self>) {
        let Some((conn, db, coll)) = self.target() else { return };
        let Some(c) = self.compass_mut() else { return };
        if c.agg_running.is_some() {
            return;
        }
        let text = c.pipeline.read(cx).text();
        if !text.trim_start().starts_with('[') {
            c.agg_error = Some("A pipeline is an array of stages: [ { $match: {…} }, { $group: {…} } ]".into());
            return cx.notify();
        }
        let work = engine::run(engine::query(conn, Scope { database: Some(db), collection: Some(coll) }, text));
        c.agg_running = Some(Instant::now());
        c.agg_error = None;
        c.agg_task = Some(cx.spawn(async move |this, cx| {
            let r = work.await;
            this.update(cx, |v, cx| {
                if let Some(c) = v.compass_mut() {
                    c.agg_took = c.agg_running.take().map(|t| t.elapsed());
                    match r {
                        Ok(mut grids) => c.agg = grids.pop().map(GridView::new),
                        Err(e) => c.agg_error = Some(e),
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Analyzes a sample of the documents the bar's filter finds.
    fn analyze(&mut self, cx: &mut Context<Self>) {
        let Some((conn, db, coll)) = self.target() else { return };
        let Some(c) = self.compass_mut() else { return };
        let filter = c.filter.read(cx).text();
        let work = engine::run(engine::mongo(conn, move |cl| async move { compass::schema(&cl, &db, &coll, &filter, 1000).await }));
        c.schema = None;
        c.schema_task = Some(cx.spawn(async move |this, cx| {
            let r = work.await;
            this.update(cx, |v, cx| {
                if let Some(c) = v.compass_mut() {
                    c.schema = Some(r);
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub(super) fn load_indexes(&mut self, cx: &mut Context<Self>) {
        let Some((conn, db, coll)) = self.target() else { return };
        let Some(c) = self.compass_mut() else { return };
        let work = engine::run(engine::mongo(conn, move |cl| async move { compass::indexes(&cl, &db, &coll).await }));
        c.idx_task = Some(cx.spawn(async move |this, cx| {
            let r = work.await;
            this.update(cx, |v, cx| {
                if let Some(c) = v.compass_mut() {
                    c.indexes = Some(r);
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn create_index(&mut self, cx: &mut Context<Self>) {
        let Some((conn, db, coll)) = self.target() else { return };
        let Some(c) = self.compass_mut() else { return };
        let (keys, options) = (c.idx_keys.read(cx).text(), c.idx_options.read(cx).text());
        let work = engine::run(engine::mongo(conn, move |cl| async move { compass::create_index(&cl, &db, &coll, &keys, &options).await }));
        c.idx_status = Some(Ok("Creating…".into()));
        c.idx_task = Some(cx.spawn(async move |this, cx| {
            let r = work.await;
            this.update(cx, |v, cx| {
                let ok = r.is_ok();
                if let Some(c) = v.compass_mut() {
                    c.idx_status = Some(r.map(|n| format!("Created {n}")));
                    if ok {
                        c.new_index = false;
                        c.idx_keys.update(cx, |e, cx| e.set_text("", cx));
                        c.idx_options.update(cx, |e, cx| e.set_text("", cx));
                    }
                }
                if ok {
                    v.load_indexes(cx);
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn ask_drop_index(&mut self, name: String, cx: &mut Context<Self>) {
        let Some((conn, db, coll)) = self.target() else { return };
        let weak = cx.entity().downgrade();
        cx.emit(Request::Ask(crate::dialog::Ask {
            title: format!("Drop the index “{name}”?"),
            detail: format!("Queries on {coll} that use it get slower. It can be created again later."),
            buttons: vec!["Drop Index".into()],
            danger: true,
            cancel_default: false,
            cancel_label: None,
            on_answer: std::rc::Rc::new(move |_, _, cx: &mut App| {
                let (conn, db, coll, name) = (conn.clone(), db.clone(), coll.clone(), name.clone());
                let Some(v) = weak.upgrade() else { return };
                v.update(cx, |v, cx| {
                    let shown = name.clone();
                    let work = engine::run(engine::mongo(conn, move |cl| async move { compass::drop_index(&cl, &db, &coll, &name).await }));
                    if let Some(c) = v.compass_mut() {
                        c.idx_task = Some(cx.spawn(async move |this, cx| {
                            let r = work.await;
                            this.update(cx, |v, cx| {
                                if let Some(c) = v.compass_mut() {
                                    c.idx_status = Some(r.map(|_| format!("Dropped {shown}")));
                                }
                                v.load_indexes(cx);
                            })
                            .ok();
                        }));
                    }
                });
            }),
        }));
    }

    fn load_validation(&mut self, cx: &mut Context<Self>) {
        let Some((conn, db, coll)) = self.target() else { return };
        let Some(c) = self.compass_mut() else { return };
        let work = engine::run(engine::mongo(conn, move |cl| async move { compass::validation(&cl, &db, &coll).await }));
        c.val_task = Some(cx.spawn(async move |this, cx| {
            let r = work.await;
            this.update(cx, |v, cx| {
                if let Some(c) = v.compass_mut() {
                    match r {
                        Ok(val) => {
                            c.validator.update(cx, |e, cx| e.set_text(&val.validator, cx));
                            c.level = val.level;
                            c.action = val.action;
                            c.val_loaded = Some(Ok(()));
                        }
                        Err(e) => c.val_loaded = Some(Err(e)),
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn update_validation(&mut self, cx: &mut Context<Self>) {
        let Some((conn, db, coll)) = self.target() else { return };
        let Some(c) = self.compass_mut() else { return };
        let val = compass::Validation { validator: c.validator.read(cx).text(), level: c.level.clone(), action: c.action.clone() };
        c.val_status = Some(Ok("Updating…".into()));
        let work = engine::run(engine::mongo(conn, move |cl| async move { compass::set_validation(&cl, &db, &coll, &val).await }));
        c.val_task = Some(cx.spawn(async move |this, cx| {
            let r = work.await;
            this.update(cx, |v, cx| {
                if let Some(c) = v.compass_mut() {
                    c.val_status = Some(r.map(|_| "Validation rules updated".into()));
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Opens a tab; the first time, it loads what it shows.
    pub(super) fn open_compass_tab(&mut self, tab: CTab, window: &mut Window, cx: &mut Context<Self>) {
        let Body::Data(d) = &mut self.body else { return };
        d.inner = Inner::DataView;
        let Some(c) = d.compass.as_deref_mut() else { return };
        c.tab = tab;
        let first_time = match tab {
            CTab::Schema => c.schema.is_none() && c.schema_task.is_none(),
            CTab::Indexes => c.indexes.is_none(),
            CTab::Validation => c.val_loaded.is_none(),
            _ => false,
        };
        if let Some(f) = c.focus_target(cx) {
            window.focus(&f, cx);
        }
        if first_time {
            match tab {
                CTab::Schema => self.analyze(cx),
                CTab::Indexes => self.load_indexes(cx),
                CTab::Validation => self.load_validation(cx),
                _ => {}
            }
        }
        cx.notify();
    }

    // ------------------------------------------------------------ test hooks

    /// The query bar's filter box, and the Shell tab's editor (tests).
    #[cfg(test)]
    pub fn editors(&self) -> (Entity<Editor>, Entity<Editor>) {
        let Body::Data(d) = &self.body else { panic!("not a data tab") };
        (d.compass.as_ref().expect("a collection").filter.clone(), d.query.clone())
    }

    #[cfg(test)]
    pub fn compass_tab(&self) -> Option<CTab> {
        self.compass().map(|c| c.tab)
    }

    /// Types into the query bar's field (filter, project, sort, collation, hint, maxTime, skip, limit).
    #[cfg(test)]
    pub fn type_in_bar(&mut self, field: &str, text: &str, cx: &mut Context<Self>) {
        let Some(c) = self.compass() else { return };
        let e = match field {
            "filter" => &c.filter,
            "project" => &c.project,
            "sort" => &c.sort,
            "collation" => &c.collation,
            "hint" => &c.hint,
            "maxTime" => &c.max_time,
            "skip" => &c.skip,
            "limit" => &c.limit,
            "pipeline" => &c.pipeline,
            "indexKeys" => &c.idx_keys,
            "indexOptions" => &c.idx_options,
            _ => panic!("no field {field}"),
        }
        .clone();
        e.update(cx, |e, cx| e.set_text(text, cx));
    }

    #[cfg(test)]
    pub fn compass_state(&self) -> (Option<String>, Option<Result<Vec<String>, String>>, Option<Result<usize, String>>, Option<usize>, Option<Result<compass::Explain, String>>) {
        let Some(c) = self.compass() else { return (None, None, None, None, None) };
        (
            c.bar_error.clone(),
            c.indexes.clone().map(|r| r.map(|l| l.into_iter().map(|i| i.name).collect())),
            c.schema.clone().map(|r| r.map(|s| s.fields.len())),
            c.agg.as_ref().map(|g| g.grid.rows.len()),
            c.explain.clone(),
        )
    }

    // ------------------------------------------------------------ drawing

    /// The tabs across the top: Documents (with the count), Aggregations, Schema, Indexes, Validation, and the shell.
    pub(super) fn render_compass_tabs(&self, d: &Data, c: &Compass, cx: &mut Context<Self>) -> gpui::Div {
        let total = match &d.page {
            Some(Ok(p)) => p.total,
            _ => None,
        };
        let n_indexes = c.indexes.as_ref().and_then(|r| r.as_ref().ok()).map(|l| l.len() as u64);
        let tab = |id: &'static str, text: &'static str, badge: Option<u64>, on: bool, cx: &mut Context<Self>| {
            div()
                .id(id)
                .debug_selector(move || id.to_string())
                .flex()
                .items_center()
                .gap(px(6.))
                .px(px(14.))
                .h_full()
                .cursor_pointer()
                .when(on, |d| d.border_b_2().border_color(theme::accent()).text_color(theme::fg_bright()).font_weight(FontWeight::SEMIBOLD))
                .when(!on, |d| d.text_color(theme::fg_dim()).hover(|s| s.text_color(theme::fg())))
                .child(text)
                .when_some(badge, |d, n| d.child(div().px(px(6.)).rounded(px(8.)).bg(theme::bg_input()).border_1().border_color(theme::border_input()).text_size(px(11.)).text_color(theme::fg()).child(db::short_count(n))))
                .on_click(cx.listener(move |v, _, window, cx| {
                    let t = match id {
                        "db-tab-aggregations" => CTab::Aggregations,
                        "db-tab-schema" => CTab::Schema,
                        "db-tab-indexes" => CTab::Indexes,
                        "db-tab-validation" => CTab::Validation,
                        _ => CTab::Documents,
                    };
                    v.open_compass_tab(t, window, cx);
                }))
        };
        let data = d.inner == Inner::DataView;
        div()
            .flex()
            .flex_none()
            .h(px(36.))
            .px(px(8.))
            .border_b_1()
            .border_color(theme::border())
            .text_size(px(13.))
            .child(tab("db-tab-data", "Documents", total, data && c.tab == CTab::Documents, cx))
            .child(tab("db-tab-aggregations", "Aggregations", None, data && c.tab == CTab::Aggregations, cx))
            .child(tab("db-tab-schema", "Schema", None, data && c.tab == CTab::Schema, cx))
            .child(tab("db-tab-indexes", "Indexes", n_indexes, data && c.tab == CTab::Indexes, cx))
            .child(tab("db-tab-validation", "Validation", None, data && c.tab == CTab::Validation, cx))
            .child(div().flex_1())
            .child(
                div()
                    .id("db-tab-query")
                    .debug_selector(|| "db-tab-query".into())
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(14.))
                    .h_full()
                    .cursor_pointer()
                    .when(!data, |d| d.border_b_2().border_color(theme::accent()).text_color(theme::fg_bright()))
                    .when(data, |d| d.text_color(theme::fg_dim()).hover(|s| s.text_color(theme::fg())))
                    .tooltip(ui::tooltip("mongosh-style commands: db.coll.find(…), updateMany, runCommand…"))
                    .on_click(cx.listener(|v, _, window, cx| {
                        if let Body::Data(d) = &mut v.body {
                            d.inner = Inner::Query;
                            window.focus(&d.query.read(cx).focus_target(cx), cx);
                        }
                        cx.notify();
                    }))
                    .child(ui::small_icon("terminal", if data { theme::fg_dim() } else { theme::accent() }))
                    .child("Shell"),
            )
    }

    /// The query bar: history, filter, Explain · Reset · Find · Options, and the options when open.
    pub(super) fn render_query_bar(&self, c: &Compass, cx: &mut Context<Self>) -> gpui::AnyElement {
        let field = |e: &Entity<Editor>| div().flex_1().min_w_0().h(px(28.)).px(px(8.)).flex().items_center().rounded(px(4.)).bg(theme::bg_input()).border_1().border_color(theme::border_input()).font_family(theme::MONO_FONT).child(e.clone());
        let label = |t: &'static str| div().w(px(92.)).flex_none().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::fg()).child(t);
        let small = |t: &'static str| div().flex_none().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::fg()).child(t);
        let history: Vec<Recent> = if c.history {
            let Body::Data(d) = &self.body else { return div().into_any_element() };
            let [db, coll] = d.path.as_slice() else { return div().into_any_element() };
            read_history().into_iter().filter(|r| r.conn == d.conn.id && &r.db == db && &r.coll == coll).collect()
        } else {
            Vec::new()
        };
        let history_menu = c.history.then(|| {
            let rows: Vec<gpui::AnyElement> = if history.is_empty() {
                vec![div().px(px(12.)).py(px(8.)).text_color(theme::fg_muted()).child("No recent queries on this collection yet").into_any_element()]
            } else {
                history
                    .into_iter()
                    .enumerate()
                    .map(|(i, r)| {
                        let q = r.q.clone();
                        div()
                            .id(("db-recent", i))
                            .debug_selector(move || format!("db-recent-{i}"))
                            .px(px(12.))
                            .py(px(6.))
                            .font_family(theme::MONO_FONT)
                            .text_size(px(12.))
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::hover()))
                            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |v, _, _, cx| {
                                if let Some(c) = v.compass_mut() {
                                    c.fill(&q, cx);
                                    c.options |= [&q.project, &q.sort, &q.collation, &q.hint, &q.max_time_ms, &q.skip, &q.limit].iter().any(|f| !f.trim().is_empty());
                                }
                                v.find(cx);
                            }))
                            .child(summary(&r.q))
                            .into_any_element()
                    })
                    .collect()
            };
            deferred(
                div()
                    .id("db-history")
                    .debug_selector(|| "db-history".into())
                    .absolute()
                    .top(px(40.))
                    .left(px(8.))
                    .w(px(520.))
                    .max_h(px(320.))
                    .overflow_y_scroll()
                    .py(px(4.))
                    .rounded(px(5.))
                    .bg(theme::bg_popup())
                    .border_1()
                    .border_color(theme::border_input())
                    .shadow_lg()
                    .child(div().px(px(12.)).py(px(4.)).text_size(px(11.)).text_color(theme::fg_muted()).child("RECENT"))
                    .children(rows),
            )
            .with_priority(1)
        });
        let link = |id: &'static str, text: String| {
            div().id(id).debug_selector(move || id.to_string()).flex().items_center().gap(px(4.)).px(px(6.)).h(px(26.)).cursor_pointer().text_size(px(12.)).text_color(theme::accent()).hover(|s| s.text_color(theme::accent_hover())).child(text)
        };
        div()
            .relative()
            .flex()
            .flex_col()
            .gap(px(8.))
            .p(px(8.))
            .rounded(px(6.))
            .border_1()
            .border_color(theme::border_input())
            .bg(theme::bg_side())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .id("db-history-button")
                            .debug_selector(|| "db-history-button".into())
                            .flex()
                            .items_center()
                            .gap(px(2.))
                            .h(px(28.))
                            .px(px(6.))
                            .rounded(px(4.))
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::hover()))
                            .tooltip(ui::tooltip("Recent queries"))
                            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(|v, _, _, cx| {
                                if let Some(c) = v.compass_mut() {
                                    c.history = !c.history;
                                }
                                cx.notify();
                            }))
                            .child(ui::small_icon("history", theme::fg()))
                            .child(ui::small_icon("chevron-down", theme::fg_dim())),
                    )
                    .child(field(&c.filter).debug_selector(|| "db-filter".into()))
                    .child(button("db-explain", "Explain", None, false).on_click(cx.listener(|v, _, _, cx| v.explain(cx))))
                    .child(button("db-reset", "Reset", None, false).on_click(cx.listener(|v, _, _, cx| v.reset_bar(cx))))
                    .child(button("db-find", "Find", None, true).on_click(cx.listener(|v, _, _, cx| v.find(cx))))
                    .child(link("db-options", format!("Options {}", if c.options { "▾" } else { "▸" })).on_click(cx.listener(|v, _, _, cx| {
                        if let Some(c) = v.compass_mut() {
                            c.options = !c.options;
                        }
                        cx.notify();
                    }))),
            )
            .when(c.options, |el| {
                el.child(
                    div()
                        .debug_selector(|| "db-options-panel".into())
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .pl(px(8.))
                        .child(div().flex().items_center().gap(px(12.)).child(label("Project")).child(field(&c.project)).child(small("Max Time MS")).child(field(&c.max_time).flex_none().w(px(150.))))
                        .child(div().flex().items_center().gap(px(12.)).child(label("Sort")).child(field(&c.sort)).child(small("Skip")).child(field(&c.skip).flex_none().w(px(80.))).child(small("Limit")).child(field(&c.limit).flex_none().w(px(80.))))
                        .child(div().flex().items_center().gap(px(12.)).child(label("Collation")).child(field(&c.collation)).child(div().w(px(150.) + px(100.))))
                        .child(div().flex().items_center().gap(px(12.)).child(label("Index Hint")).child(field(&c.hint)).child(div().w(px(150.) + px(100.)))),
                )
            })
            .when_some(c.bar_error.clone(), |el, e| el.child(div().debug_selector(|| "db-bar-error".into()).px(px(4.)).text_size(px(12.)).text_color(theme::git_deleted()).child(e)))
            .children(history_menu)
            .into_any_element()
    }

    /// Explain Plan: what the query did, in place of the documents.
    pub(super) fn render_explain(&self, c: &Compass, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let r = c.explain.as_ref()?;
        let head = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(div().flex_1().text_size(px(15.)).text_color(theme::fg_bright()).child("Explain Plan"))
            .when(r.is_ok(), |d| {
                d.child(button("db-explain-raw", if c.explain_raw { "Visual Tree" } else { "Raw Output" }, None, false).on_click(cx.listener(|v, _, _, cx| {
                    if let Some(c) = v.compass_mut() {
                        c.explain_raw = !c.explain_raw;
                    }
                    cx.notify();
                })))
            })
            .child(button("db-explain-close", "Close", Some("close"), false).on_click(cx.listener(|v, _, _, cx| {
                if let Some(c) = v.compass_mut() {
                    c.explain = None;
                }
                cx.notify();
            })));
        let body: gpui::AnyElement = match r {
            Err(e) => div().text_color(theme::git_deleted()).child(e.clone()).into_any_element(),
            Ok(e) if c.explain_raw => div().font_family(theme::MONO_FONT).text_size(px(12.)).children(e.raw.text(db::TextStyle::Json).lines().map(|l| div().whitespace_nowrap().child(l.replace(' ', "\u{a0}")))).into_any_element(),
            Ok(e) => {
                let stat = |label: &'static str, n: i64| {
                    div().flex().flex_col().gap(px(2.)).px(px(14.)).py(px(10.)).rounded(px(6.)).border_1().border_color(theme::border_input()).bg(theme::bg_side()).child(div().text_size(px(20.)).text_color(theme::fg_bright()).child(super::group_digits(n.max(0) as u64))).child(div().text_size(px(12.)).text_color(theme::fg_dim()).child(label))
                };
                let verdict = match e.indexes.as_slice() {
                    [] => ("No index available for this query (it scanned the collection)".to_string(), theme::git_modified()),
                    list => (format!("Query used the index {}", list.join(", ")), theme::git_added()),
                };
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .child(div().flex().gap(px(10.)).flex_wrap().child(stat("Documents returned", e.returned)).child(stat("Index keys examined", e.keys_examined)).child(stat("Documents examined", e.docs_examined)).child(stat("Execution time (ms)", e.ms)))
                    .child(div().debug_selector(|| "db-explain-verdict".into()).text_color(verdict.1).child(verdict.0))
                    .when(e.in_memory_sort, |d| d.child(div().text_color(theme::git_modified()).child("Sorted in memory: no index gave the order")))
                    .child(div().text_size(px(12.)).text_color(theme::fg_dim()).child("WINNING PLAN"))
                    .child(div().flex().flex_col().gap(px(4.)).font_family(theme::MONO_FONT).text_size(px(12.5)).children(e.stages.iter().enumerate().map(|(i, s)| {
                        div().flex().items_center().gap(px(6.)).pl(px(i as f32 * 18.)).child(div().text_color(theme::fg_muted()).child(if i == 0 { "●" } else { "└" })).child(div().px(px(8.)).py(px(2.)).rounded(px(4.)).bg(theme::bg_side()).border_1().border_color(theme::border_input()).text_color(theme::fg_bright()).child(s.clone()))
                    })))
                    .into_any_element()
            }
        };
        Some(div().id("db-explain-panel").debug_selector(|| "db-explain-panel".into()).flex().flex_col().gap(px(12.)).flex_1().min_h_0().overflow_y_scroll().p(px(14.)).rounded(px(6.)).border_1().border_color(theme::border_input()).child(head).child(body).into_any_element())
    }

    pub(super) fn render_compass_tab(&self, d: &Data, c: &Compass, cx: &mut Context<Self>) -> gpui::AnyElement {
        let page = div().flex().flex_col().size_full().px(px(20.)).pt(px(14.)).pb(px(10.)).gap(px(10.));
        let name = d.path.last().cloned().unwrap_or_default();
        match c.tab {
            CTab::Documents => self.render_data_view(d, cx),
            CTab::Aggregations => {
                let status = match (&c.agg_running, &c.agg_error, &c.agg) {
                    (Some(t), _, _) => format!("Running… {:.1}s", t.elapsed().as_secs_f32()),
                    (_, Some(_), _) => "Failed".into(),
                    (_, _, Some(g)) => format!("{}{} document{}{}", g.grid.rows.len(), if g.grid.more { "+" } else { "" }, if g.grid.rows.len() == 1 { "" } else { "s" }, c.agg_took.map(|t| format!(" · {} ms", t.as_millis())).unwrap_or_default()),
                    _ => "Write the pipeline, then Run (⌘↵)".into(),
                };
                let results: gpui::AnyElement = match (&c.agg_error, &c.agg) {
                    (Some(e), _) => div().debug_selector(|| "db-agg-error".into()).p(px(10.)).text_color(theme::git_deleted()).child(e.clone()).into_any_element(),
                    (_, Some(g)) if !g.grid.columns.is_empty() => div().flex().flex_col().flex_1().min_h_0().child(self.render_grid(Some(AGG), g, false, cx)).into_any_element(),
                    (_, Some(g)) => div().p(px(10.)).text_color(theme::fg_dim()).child(if g.grid.note.is_empty() { "No documents".to_string() } else { g.grid.note.clone() }).into_any_element(),
                    _ => div().into_any_element(),
                };
                page.child(div().text_size(px(17.)).text_color(theme::fg_bright()).child(format!("Aggregations: {name}")))
                    .child(div().h(px(200.)).flex_none().rounded(px(6.)).border_1().border_color(theme::border_input()).overflow_hidden().child(c.pipeline.clone()))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(button("db-agg-run", if c.agg_running.is_some() { "Running…" } else { "Run" }, Some("arrow-right"), true).on_click(cx.listener(|v, _, _, cx| v.run_pipeline(cx))))
                            .child(div().text_size(px(11.)).text_color(theme::fg_muted()).child("⌘↵"))
                            .child(div().flex_1().debug_selector(|| "db-agg-status".into()).text_size(px(12.)).text_color(if c.agg_error.is_some() { theme::git_deleted() } else { theme::fg_dim() }).child(status))
                            .when(c.agg.as_ref().is_some_and(|g| g.grid.trees.is_some()), |el| el.child(super::mode_switch(d.doc_mode, cx))),
                    )
                    .child(results)
                    .into_any_element()
            }
            CTab::Schema => {
                let body: gpui::AnyElement = match &c.schema {
                    None => div().text_color(theme::fg_dim()).child("Analyzing documents…").into_any_element(),
                    Some(Err(e)) => div().text_color(theme::git_deleted()).child(e.clone()).into_any_element(),
                    Some(Ok(s)) if s.fields.is_empty() => div().text_color(theme::fg_dim()).child("No documents to analyze").into_any_element(),
                    Some(Ok(s)) => div()
                        .id("db-schema")
                        .debug_selector(|| "db-schema".into())
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .children(s.fields.iter().enumerate().map(|(i, f)| schema_row(i, f, s.sampled)))
                        .into_any_element(),
                };
                let sampled = c.schema.as_ref().and_then(|r| r.as_ref().ok()).map(|s| format!("Analyzed a sample of {} documents{}", s.sampled, if c.filter.read(cx).text().trim().is_empty() { String::new() } else { " that match the filter".into() })).unwrap_or_default();
                page.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(div().flex_1().text_size(px(17.)).text_color(theme::fg_bright()).child(format!("Schema: {name}")))
                        .child(button("db-schema-analyze", "Analyze Schema", Some("refresh"), true).on_click(cx.listener(|v, _, _, cx| v.analyze(cx)))),
                )
                .child(div().text_size(px(12.)).text_color(theme::fg_dim()).child(sampled))
                .child(body)
                .into_any_element()
            }
            CTab::Indexes => {
                let body: gpui::AnyElement = match &c.indexes {
                    None => div().text_color(theme::fg_dim()).child("Loading indexes…").into_any_element(),
                    Some(Err(e)) => div().text_color(theme::git_deleted()).child(e.clone()).into_any_element(),
                    Some(Ok(list)) => {
                        let head = |t: &'static str, w: Option<f32>| match w {
                            Some(w) => div().w(px(w)).flex_none().px(px(8.)).child(t),
                            None => div().flex_1().min_w_0().px(px(8.)).child(t),
                        };
                        div()
                            .id("db-indexes")
                            .debug_selector(|| "db-indexes".into())
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .rounded(px(6.))
                            .border_1()
                            .border_color(theme::border_input())
                            .child(div().flex().items_center().h(px(30.)).bg(theme::bg_side()).border_b_1().border_color(theme::border()).text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::fg_bright()).child(head("Name and Definition", None)).child(head("Type", Some(110.))).child(head("Size", Some(90.))).child(head("Usage", Some(170.))).child(head("Properties", Some(170.))).child(div().w(px(40.))))
                            .children(list.iter().enumerate().map(|(i, ix)| {
                                let name = ix.name.clone();
                                let usage = match (&ix.ops, &ix.since) {
                                    (Some(n), Some(s)) => format!("{} since {s}", db::short_count(*n)),
                                    (Some(n), None) => db::short_count(*n),
                                    _ => "—".into(),
                                };
                                div()
                                    .id(("db-index", i))
                                    .debug_selector(move || format!("db-index-{i}"))
                                    .group("db-index")
                                    .flex()
                                    .items_center()
                                    .min_h(px(46.))
                                    .border_b_1()
                                    .border_color(theme::border())
                                    .text_size(px(12.5))
                                    .child(div().flex_1().min_w_0().px(px(8.)).flex().flex_col().gap(px(2.)).child(div().font_weight(FontWeight::SEMIBOLD).text_color(theme::fg_bright()).child(ix.name.clone())).child(div().font_family(theme::MONO_FONT).text_size(px(11.5)).text_color(theme::fg_dim()).overflow_hidden().text_ellipsis().whitespace_nowrap().child(ix.keys.clone())))
                                    .child(div().w(px(110.)).flex_none().px(px(8.)).child(ui::badge(ix.kind.to_uppercase(), theme::bg_input())))
                                    .child(div().w(px(90.)).flex_none().px(px(8.)).text_color(theme::fg()).child(ix.size.map(size_text).unwrap_or_else(|| "—".into())))
                                    .child(div().w(px(170.)).flex_none().px(px(8.)).text_color(theme::fg()).child(usage))
                                    .child(div().w(px(170.)).flex_none().px(px(8.)).flex().flex_wrap().gap(px(4.)).children(ix.props.iter().map(|p| ui::badge(p.to_uppercase(), theme::bg_input()))))
                                    .child(
                                        div().w(px(40.)).flex_none().flex().justify_center().when(ix.name != "_id_", |d| {
                                            d.child(
                                                div()
                                                    .id(("db-index-drop", i))
                                                    .debug_selector(move || format!("db-index-drop-{i}"))
                                                    .invisible()
                                                    .group_hover("db-index", |s| s.visible())
                                                    .p(px(4.))
                                                    .rounded(px(4.))
                                                    .cursor_pointer()
                                                    .hover(|s| s.bg(theme::hover()))
                                                    .tooltip(ui::tooltip("Drop Index"))
                                                    .on_click(cx.listener(move |v, _, _, cx| v.ask_drop_index(name.clone(), cx)))
                                                    .child(ui::small_icon("trash", theme::fg_dim())),
                                            )
                                        }),
                                    )
                            }))
                            .into_any_element()
                    }
                };
                let field = |e: &Entity<Editor>| div().flex_1().min_w_0().h(px(28.)).px(px(8.)).flex().items_center().rounded(px(4.)).bg(theme::bg_input()).border_1().border_color(theme::border_input()).font_family(theme::MONO_FONT).child(e.clone());
                page.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(div().flex_1().text_size(px(17.)).text_color(theme::fg_bright()).child(format!("Indexes: {name}")))
                        .child(button("db-index-refresh", "Refresh", Some("refresh"), false).on_click(cx.listener(|v, _, _, cx| v.load_indexes(cx))))
                        .child(button("db-index-new", "Create Index", Some("plus"), true).on_click(cx.listener(|v, _, _, cx| {
                            if let Some(c) = v.compass_mut() {
                                c.new_index = !c.new_index;
                                c.idx_status = None;
                            }
                            cx.notify();
                        }))),
                )
                .when(c.new_index, |el| {
                    el.child(
                        div()
                            .debug_selector(|| "db-index-form".into())
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .p(px(12.))
                            .rounded(px(6.))
                            .border_1()
                            .border_color(theme::border_input())
                            .bg(theme::bg_side())
                            .child(div().flex().items_center().gap(px(12.)).child(div().w(px(70.)).text_size(px(12.)).child("Fields")).child(field(&c.idx_keys)))
                            .child(div().flex().items_center().gap(px(12.)).child(div().w(px(70.)).text_size(px(12.)).child("Options")).child(field(&c.idx_options)))
                            .child(div().text_size(px(11.)).text_color(theme::fg_muted()).child("1 / -1 for ascending / descending, 'text', '2dsphere' or 'hashed'. Options: unique, sparse, hidden, expireAfterSeconds, partialFilterExpression, collation, name."))
                            .child(div().flex().gap(px(8.)).child(button("db-index-create", "Create Index", None, true).on_click(cx.listener(|v, _, _, cx| v.create_index(cx)))).child(button("db-index-cancel", "Cancel", None, false).on_click(cx.listener(|v, _, _, cx| {
                                if let Some(c) = v.compass_mut() {
                                    c.new_index = false;
                                }
                                cx.notify();
                            })))),
                    )
                })
                .when_some(c.idx_status.clone(), |el, s| el.child(status_line("db-index-status", s)))
                .child(body)
                .into_any_element()
            }
            CTab::Validation => {
                let choice = |id: &'static str, value: &'static str, on: bool, level: bool, cx: &mut Context<Self>| {
                    div()
                        .id(id)
                        .debug_selector(move || id.to_string())
                        .px(px(10.))
                        .h(px(24.))
                        .flex()
                        .items_center()
                        .rounded(px(4.))
                        .cursor_pointer()
                        .text_size(px(12.))
                        .when(on, |d| d.bg(theme::accent()).text_color(theme::on(theme::accent())))
                        .when(!on, |d| d.text_color(theme::fg()).hover(|s| s.bg(theme::hover())))
                        .on_click(cx.listener(move |v, _, _, cx| {
                            if let Some(c) = v.compass_mut() {
                                if level {
                                    c.level = value.into();
                                } else {
                                    c.action = value.into();
                                }
                            }
                            cx.notify();
                        }))
                        .child(value)
                };
                let group = || div().flex().gap(px(2.)).p(px(2.)).rounded(px(4.)).border_1().border_color(theme::border_input());
                page.child(div().text_size(px(17.)).text_color(theme::fg_bright()).child(format!("Validation: {name}")))
                    .child(div().text_size(px(12.)).text_color(theme::fg_dim()).child("Rules new and updated documents must follow, as a query ({ $jsonSchema: {…} } or operators). {} for none."))
                    .when_some(c.val_loaded.as_ref().and_then(|r| r.as_ref().err()).cloned(), |el, e| el.child(div().text_color(theme::git_deleted()).child(e)))
                    .child(div().h(px(260.)).flex_none().rounded(px(6.)).border_1().border_color(theme::border_input()).overflow_hidden().child(c.validator.clone()))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(12.))
                            .flex_wrap()
                            .child(div().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).child("Validation Action"))
                            .child(group().child(choice("db-val-error", "error", c.action == "error", false, cx)).child(choice("db-val-warn", "warn", c.action == "warn", false, cx)))
                            .child(div().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).child("Validation Level"))
                            .child(group().child(choice("db-val-off", "off", c.level == "off", true, cx)).child(choice("db-val-moderate", "moderate", c.level == "moderate", true, cx)).child(choice("db-val-strict", "strict", c.level == "strict", true, cx)))
                            .child(div().flex_1())
                            .child(button("db-val-cancel", "Cancel", None, false).on_click(cx.listener(|v, _, _, cx| v.load_validation(cx))))
                            .child(button("db-val-update", "Update", None, true).on_click(cx.listener(|v, _, _, cx| v.update_validation(cx)))),
                    )
                    .when_some(c.val_status.clone(), |el, s| el.child(status_line("db-val-status", s)))
                    .into_any_element()
            }
        }
    }
}

fn status_line(id: &'static str, s: Result<String, String>) -> gpui::Div {
    let (text, color) = match s {
        Ok(t) => (t, theme::git_added()),
        Err(e) => (e, theme::git_deleted()),
    };
    div().debug_selector(move || id.to_string()).text_size(px(12.)).text_color(color).child(text)
}

fn size_text(b: u64) -> String {
    match b {
        0..1024 => format!("{b} B"),
        1024..1_048_576 => format!("{:.1} KB", b as f64 / 1024.),
        1_048_576..1_073_741_824 => format!("{:.1} MB", b as f64 / 1_048_576.),
        _ => format!("{:.2} GB", b as f64 / 1_073_741_824.),
    }
}

/// One field of the schema: its name, how often it's there, its types as a bar, a few values.
fn schema_row(i: usize, f: &compass::Field, sampled: u64) -> gpui::AnyElement {
    let pct = |n: u64| if sampled == 0 { 0. } else { n as f32 * 100. / sampled as f32 };
    let palette = [theme::accent(), theme::git_added(), theme::git_modified(), theme::git_deleted(), theme::fg_dim()];
    let total: u64 = f.types.iter().map(|t| t.1).sum::<u64>().max(1);
    let bar = div().flex().h(px(10.)).w_full().rounded(px(3.)).overflow_hidden().bg(theme::bg_input()).children(f.types.iter().enumerate().map(|(k, (_, n))| div().h_full().w(gpui::relative(*n as f32 / total as f32)).bg(palette[k % palette.len()])));
    let legend = div().flex().flex_wrap().gap(px(10.)).text_size(px(11.5)).children(f.types.iter().enumerate().map(|(k, (t, n))| {
        div().flex().items_center().gap(px(4.)).child(div().size(px(8.)).rounded(px(2.)).bg(palette[k % palette.len()])).child(div().text_color(theme::fg()).child(format!("{t} {:.0}%", *n as f32 * 100. / total as f32)))
    }));
    let detail = match &f.range {
        Some((lo, hi)) => format!("{lo} … {hi}"),
        None => f.samples.join(" · "),
    };
    div()
        .id(("db-schema-field", i))
        .debug_selector(move || format!("db-schema-field-{i}"))
        .flex()
        .gap(px(16.))
        .py(px(10.))
        .pl(px(f.depth as f32 * 22.))
        .border_b_1()
        .border_color(theme::border())
        .child(
            div()
                .w(px(240.))
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().font_weight(FontWeight::SEMIBOLD).text_color(theme::fg_bright()).font_family(theme::MONO_FONT).child(SharedString::from(f.name.clone())))
                .child(div().text_size(px(11.5)).text_color(theme::fg_dim()).child(format!("in {:.0}% of documents", pct(f.count).min(100.)))),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(5.))
                .child(bar)
                .child(legend)
                .when(!detail.is_empty(), |d| d.child(div().font_family(theme::MONO_FONT).text_size(px(11.5)).text_color(theme::fg_dim()).overflow_hidden().text_ellipsis().whitespace_nowrap().child(detail))),
        )
        .into_any_element()
}
