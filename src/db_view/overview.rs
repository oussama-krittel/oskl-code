//! A MongoDB database's tab, like Compass's: its collections with their sizes, sortable;
//! one clicked opens; Create collection, Refresh, the shell, and drop (asked first).

use super::{Body, DbRequest, DbView, Open, button, input};
use crate::db::{self, Conn, compass, engine};
use crate::editor::Editor;
use crate::request::Request;
use crate::theme;
use crate::ui::{self, px};
use gpui::prelude::*;
use gpui::{App, Context, Entity, FontWeight, SharedString, Subscription, Task, div};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Col {
    Name,
    Storage,
    Data,
    Docs,
    Avg,
    Indexes,
    IndexSize,
}

pub(super) struct Overview {
    pub(super) conn: Conn,
    pub(super) db: String,
    pub(super) list: Option<Result<Vec<compass::CollStat>, String>>,
    sort: (Col, bool),
    creating: Option<Entity<Editor>>,
    _enter: Option<Subscription>,
    status: Option<Result<String, String>>,
    task: Option<Task<()>>,
}

impl Overview {
    pub(super) fn new(conn: Conn, db: String) -> Overview {
        Overview { conn, db, list: None, sort: (Col::Name, false), creating: None, _enter: None, status: None, task: None }
    }

    fn sorted(&self) -> Vec<compass::CollStat> {
        let Some(Ok(list)) = &self.list else { return Vec::new() };
        let mut list = list.clone();
        let (col, desc) = self.sort;
        let key = |c: &compass::CollStat| match col {
            Col::Storage => c.storage,
            Col::Data => c.data,
            Col::Docs => c.docs,
            Col::Avg => c.avg,
            Col::Indexes => c.indexes,
            Col::IndexSize => c.index_size,
            Col::Name => None,
        };
        if col == Col::Name {
            list.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        } else {
            list.sort_by_key(key);
        }
        if desc {
            list.reverse();
        }
        list
    }
}

pub(super) fn size_text(b: u64) -> String {
    match b {
        0..1000 => format!("{b} B"),
        1000..1_000_000 => format!("{:.2} kB", b as f64 / 1e3),
        1_000_000..1_000_000_000 => format!("{:.2} MB", b as f64 / 1e6),
        _ => format!("{:.2} GB", b as f64 / 1e9),
    }
}

impl DbView {
    fn overview_mut(&mut self) -> Option<&mut Overview> {
        match &mut self.body {
            Body::Overview(o) => Some(o),
            _ => None,
        }
    }

    pub(super) fn load_overview(&mut self, cx: &mut Context<Self>) {
        let Some(o) = self.overview_mut() else { return };
        let (conn, db) = (o.conn.clone(), o.db.clone());
        let work = engine::run(engine::mongo(conn, move |cl| async move { compass::collections(&cl, &db).await }));
        o.task = Some(cx.spawn(async move |this, cx| {
            let r = work.await;
            this.update(cx, |v, cx| {
                if let Some(o) = v.overview_mut() {
                    o.list = Some(r);
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn create_collection(&mut self, cx: &mut Context<Self>) {
        let Some(o) = self.overview_mut() else { return };
        let Some(name) = o.creating.as_ref().map(|e| e.read(cx).text()) else { return };
        let (conn, db, shown) = (o.conn.clone(), o.db.clone(), name.trim().to_string());
        let work = engine::run(engine::mongo(conn, move |cl| async move { compass::create_collection(&cl, &db, &name).await }));
        o.task = Some(cx.spawn(async move |this, cx| {
            let r = work.await;
            this.update(cx, |v, cx| {
                let ok = r.is_ok();
                if let Some(o) = v.overview_mut() {
                    o.status = Some(r.map(|_| format!("Created {shown}")));
                    if ok {
                        o.creating = None;
                    }
                }
                if ok {
                    v.load_overview(cx);
                    // The panel lists it too.
                    cx.emit(Request::Db(DbRequest::Changed { expand: None }));
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn ask_drop_collection(&mut self, name: String, cx: &mut Context<Self>) {
        let Some(o) = self.overview_mut() else { return };
        let (conn, db) = (o.conn.clone(), o.db.clone());
        let weak = cx.entity().downgrade();
        cx.emit(Request::Ask(crate::dialog::Ask {
            title: format!("Drop the collection “{name}”?"),
            detail: format!("All its documents and indexes are deleted from {db}. This can't be undone."),
            buttons: vec!["Drop Collection".into()],
            danger: true,
            cancel_default: true,
            cancel_label: None,
            on_answer: std::rc::Rc::new(move |_, _, cx: &mut App| {
                let (conn, db, name) = (conn.clone(), db.clone(), name.clone());
                let Some(v) = weak.upgrade() else { return };
                v.update(cx, |v, cx| {
                    let shown = name.clone();
                    let work = engine::run(engine::mongo(conn, move |cl| async move { compass::drop_collection(&cl, &db, &name).await }));
                    if let Some(o) = v.overview_mut() {
                        o.task = Some(cx.spawn(async move |this, cx| {
                            let r = work.await;
                            this.update(cx, |v, cx| {
                                if let Some(o) = v.overview_mut() {
                                    o.status = Some(r.map(|_| format!("Dropped {shown}")));
                                }
                                v.load_overview(cx);
                                cx.emit(Request::Db(DbRequest::Changed { expand: None }));
                            })
                            .ok();
                        }));
                    }
                });
            }),
        }));
    }

    #[cfg(test)]
    pub fn overview_names(&self) -> Option<Result<Vec<String>, String>> {
        match &self.body {
            Body::Overview(o) => o.list.clone().map(|r| r.map(|_| o.sorted().into_iter().map(|c| c.name).collect())),
            _ => None,
        }
    }

    #[cfg(test)]
    pub fn type_collection_name(&mut self, name: &str, cx: &mut Context<Self>) {
        if let Some(e) = self.overview_mut().and_then(|o| o.creating.clone()) {
            e.update(cx, |e, cx| e.set_text(name, cx));
        }
    }

    pub(super) fn render_overview(&self, o: &Overview, cx: &mut Context<Self>) -> gpui::AnyElement {
        let head = |id: &'static str, text: &'static str, col: Col, w: Option<f32>, cx: &mut Context<Self>| {
            let arrow = match o.sort {
                (c, false) if c == col => " ↑",
                (c, true) if c == col => " ↓",
                _ => " ↕",
            };
            div()
                .id(id)
                .debug_selector(move || id.to_string())
                .when_some(w, |d, w| d.w(px(w)).flex_none())
                .when(w.is_none(), |d| d.flex_1().min_w_0())
                .px(px(10.))
                .h_full()
                .flex()
                .items_center()
                .gap(px(4.))
                // A long name stays in its column (cut short rather than over the next one).
                .overflow_hidden()
                .whitespace_nowrap()
                .cursor_pointer()
                .hover(|s| s.bg(theme::hover()))
                .on_click(cx.listener(move |v, _, _, cx| {
                    if let Some(o) = v.overview_mut() {
                        // Sizes and counts biggest first; names A to Z.
                        o.sort = if o.sort.0 == col { (col, !o.sort.1) } else { (col, col != Col::Name) };
                    }
                    cx.notify();
                }))
                .child(div().min_w_0().overflow_hidden().text_ellipsis().child(text))
                .child(div().flex_none().text_color(theme::fg_muted()).child(arrow))
        };
        let cell = |text: String, w: f32| div().w(px(w)).flex_none().px(px(10.)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(text);
        let opt = |v: Option<u64>, f: fn(u64) -> String| v.map(f).unwrap_or_else(|| "–".into());
        let body: gpui::AnyElement = match &o.list {
            None => div().p(px(14.)).text_color(theme::fg_dim()).child("Loading collections…").into_any_element(),
            Some(Err(e)) => div().p(px(14.)).text_color(theme::git_deleted()).child(e.clone()).into_any_element(),
            Some(Ok(l)) if l.is_empty() => div().p(px(14.)).text_color(theme::fg_dim()).child("No collections yet").into_any_element(),
            Some(Ok(_)) => div()
                .id("db-overview-rows")
                .debug_selector(|| "db-overview-rows".into())
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .children(o.sorted().into_iter().enumerate().map(|(i, c)| {
                    let (conn, db, name) = (o.conn.id.clone(), o.db.clone(), c.name.clone());
                    let drop_name = c.name.clone();
                    div()
                        .id(("db-coll-row", i))
                        .debug_selector(move || format!("db-coll-{}", i))
                        .group("db-coll-row")
                        .flex()
                        .items_center()
                        .min_h(px(40.))
                        .border_b_1()
                        .border_color(theme::border())
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .on_click(cx.listener(move |_, _, _, cx| cx.emit(Request::Db(DbRequest::Open(Open::Data { conn: conn.clone(), path: vec![db.clone(), name.clone()] })))))
                        .child(div().flex_1().min_w_0().px(px(10.)).text_color(theme::fg_bright()).child(SharedString::from(c.name.clone())))
                        .child(div().w(px(150.)).flex_none().px(px(10.)).flex().flex_wrap().gap(px(4.)).when(c.props.is_empty(), |d| d.child("-")).children(c.props.iter().map(|p| ui::badge(p.to_uppercase(), theme::bg_input()))))
                        .child(cell(opt(c.storage, size_text), 130.))
                        .child(cell(opt(c.data, size_text), 115.))
                        .child(cell(opt(c.docs, db::short_count), 115.))
                        .child(cell(opt(c.avg, size_text), 130.))
                        .child(cell(opt(c.indexes, |n| n.to_string()), 95.))
                        .child(cell(opt(c.index_size, size_text), 115.))
                        .child(
                            div().w(px(40.)).flex_none().flex().justify_center().child(
                                div()
                                    .id(("db-coll-drop", i))
                                    .debug_selector(move || format!("db-coll-drop-{i}"))
                                    .invisible()
                                    .group_hover("db-coll-row", |s| s.visible())
                                    .p(px(4.))
                                    .rounded(px(4.))
                                    .hover(|s| s.bg(theme::bg_input()))
                                    .tooltip(ui::tooltip("Drop Collection"))
                                    .on_click(cx.listener(move |v, _, _, cx| {
                                        cx.stop_propagation();
                                        v.ask_drop_collection(drop_name.clone(), cx);
                                    }))
                                    .child(ui::small_icon("trash", theme::fg_dim())),
                            ),
                        )
                }))
                .into_any_element(),
        };
        let (conn_id, db) = (o.conn.id.clone(), o.db.clone());
        div()
            .flex()
            .flex_col()
            .size_full()
            .px(px(20.))
            .pt(px(14.))
            .pb(px(10.))
            .gap(px(10.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().flex_1().min_w_0().flex().items_center().gap(px(6.)).text_size(px(14.)).child(div().text_color(theme::git_added()).child(o.conn.title())).child(div().text_color(theme::fg_muted()).child("›")).child(div().text_color(theme::fg_bright()).child(o.db.clone())))
                    .child(button("db-open-shell", "Open MongoDB shell", Some("terminal"), false).on_click(cx.listener(move |_, _, _, cx| cx.emit(Request::Db(DbRequest::Open(Open::Query { conn: conn_id.clone(), path: vec![db.clone()] }))))))
                    .child(button("db-coll-new", "Create collection", Some("plus"), true).on_click(cx.listener(|v, _, window, cx| {
                        if let Some(o) = v.overview_mut() {
                            o.status = None;
                            o.creating = match o.creating.take() {
                                Some(_) => None,
                                None => {
                                    let e = input("Collection name", "", cx);
                                    window.focus(&e.read(cx).focus_target(cx), cx);
                                    o._enter = Some(cx.subscribe(&e, |v: &mut DbView, _, ev: &crate::editor::EditorEvent, cx| {
                                        if matches!(ev, crate::editor::EditorEvent::Submit) {
                                            v.create_collection(cx);
                                        }
                                    }));
                                    Some(e)
                                }
                            };
                        }
                        cx.notify();
                    })))
                    .child(button("db-overview-refresh", "Refresh", Some("refresh"), false).on_click(cx.listener(|v, _, _, cx| v.load_overview(cx)))),
            )
            .when_some(o.creating.clone(), |el, e| {
                el.child(
                    div()
                        .debug_selector(|| "db-coll-form".into())
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .p(px(10.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(theme::border_input())
                        .bg(theme::bg_side())
                        .child(div().text_size(px(12.)).child("Name"))
                        .child(div().flex_1().h(px(28.)).px(px(8.)).flex().items_center().rounded(px(4.)).bg(theme::bg_input()).border_1().border_color(theme::border_input()).child(e))
                        .child(button("db-coll-create", "Create", None, true).on_click(cx.listener(|v, _, _, cx| v.create_collection(cx))))
                        .child(button("db-coll-cancel", "Cancel", None, false).on_click(cx.listener(|v, _, _, cx| {
                            if let Some(o) = v.overview_mut() {
                                o.creating = None;
                            }
                            cx.notify();
                        }))),
                )
            })
            .when_some(o.status.clone(), |el, s| {
                let (text, color) = match s {
                    Ok(t) => (t, theme::git_added()),
                    Err(e) => (e, theme::git_deleted()),
                };
                el.child(div().debug_selector(|| "db-overview-status".into()).text_size(px(12.)).text_color(color).child(text))
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(theme::border_input())
                    .text_size(px(12.5))
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .h(px(34.))
                            .bg(theme::bg_side())
                            .border_b_1()
                            .border_color(theme::border())
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::fg_bright())
                            .child(head("db-sort-name", "Collection name", Col::Name, None, cx))
                            .child(div().w(px(150.)).flex_none().px(px(10.)).child("Properties"))
                            .child(head("db-sort-storage", "Storage size", Col::Storage, Some(130.), cx))
                            .child(head("db-sort-data", "Data size", Col::Data, Some(115.), cx))
                            .child(head("db-sort-docs", "Documents", Col::Docs, Some(115.), cx))
                            .child(head("db-sort-avg", "Avg. doc size", Col::Avg, Some(130.), cx))
                            .child(head("db-sort-indexes", "Indexes", Col::Indexes, Some(95.), cx))
                            .child(head("db-sort-index-size", "Index size", Col::IndexSize, Some(115.), cx))
                            .child(div().w(px(40.)).flex_none()),
                    )
                    .child(body),
            )
            .into_any_element()
    }
}
