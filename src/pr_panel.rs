//! The Pull requests sidebar: the repository's pull requests (read with `gh`), filtered by
//! state; a click reviews one in its own tab. While a review tab is in front, the sidebar is
//! that pull request's files as a tree (like GitHub's), and a file picked shows its diff.

use crate::commit_view::FileRow;
use crate::editor::{Editor, EditorEvent, EditorMode};
use crate::git::gh;
use crate::git_panel::pr::PrView;
use crate::request::Request;
use crate::theme;
use crate::ui::{self, px};
use gpui::prelude::*;
use gpui::*;
use std::path::PathBuf;
use std::rc::Rc;

const STATES: [(&str, &str); 4] = [("open", "Open"), ("merged", "Merged"), ("closed", "Closed"), ("all", "All")];
/// Rows of the file tree (it draws only what's on screen, so 30 000 files scroll freely).
const ROW_H: f32 = 22.;
const INDENT: f32 = 12.;

pub struct PrPanel {
    focus: FocusHandle,
    root: Option<PathBuf>,
    /// One of `STATES`' keys.
    pub state: &'static str,
    /// Each state's list as last read (switching back shows it at once while it's read again).
    lists: std::collections::HashMap<&'static str, Result<Vec<gh::Pr>, String>>,
    /// The state being read with gh.
    loading: Option<&'static str>,
    selected: Option<u64>,
    _load: Option<Task<()>>,
    /// The review tab in front: its files are the tree.
    pr: Option<Entity<PrView>>,
    _pr_sub: Option<Subscription>,
    /// The list asked for while a review is in front.
    pub show_list: bool,
    filter: Entity<Editor>,
    /// Files whose path holds the filter's text (by index), for (text, files shown, how many).
    matches: Option<(String, usize, usize, Rc<Vec<usize>>)>,
    scroll: UniformListScrollHandle,
    _subs: Vec<Subscription>,
}

impl EventEmitter<Request> for PrPanel {}

impl PrPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| {
            let mut ed = Editor::new(EditorMode::SingleLine, cx);
            ed.font_size = 12.;
            ed.placeholder = "Filter files".into();
            ed
        });
        let subs = vec![cx.subscribe(&filter, |p, _, ev: &EditorEvent, cx| {
            if matches!(ev, EditorEvent::Changed) {
                p.scroll.scroll_to_item(0, ScrollStrategy::Top);
                cx.notify();
            }
        })];
        Self {
            focus: cx.focus_handle(),
            root: None,
            state: "open",
            lists: Default::default(),
            loading: None,
            selected: None,
            _load: None,
            pr: None,
            _pr_sub: None,
            show_list: false,
            filter,
            matches: None,
            scroll: UniformListScrollHandle::new(),
            _subs: subs,
        }
    }

    pub fn focus_target(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }

    /// The repository to list; read the first time and when it changes.
    pub fn set_root(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if self.root.as_ref() != Some(&root) {
            self.root = Some(root);
            self.selected = None;
            self.lists.clear();
            self.reload(cx);
        }
    }

    /// The review tab in front (None: another tab): the sidebar shows its files.
    pub fn set_pr(&mut self, pr: Option<Entity<PrView>>, cx: &mut Context<Self>) {
        if self.pr.as_ref().map(|p| p.entity_id()) == pr.as_ref().map(|p| p.entity_id()) {
            return;
        }
        self._pr_sub = pr.as_ref().map(|p| cx.observe(p, |_, _, cx| cx.notify()));
        if let Some(p) = &pr {
            self.selected = Some(p.read(cx).number);
        }
        self.pr = pr;
        self.show_list = false;
        self.matches = None;
        self.filter.update(cx, |e, cx| e.set_text("", cx));
        cx.notify();
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        let state = self.state;
        self.loading = Some(state);
        let task = cx.background_executor().spawn(async move { gh::list(&root, state).map_err(|e| e.to_string()) });
        self._load = Some(cx.spawn(async move |p, cx| {
            let prs = task.await;
            p.update(cx, |p, cx| {
                p.lists.insert(state, prs);
                p.loading = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn render_row(&self, p: &gh::Pr, cx: &mut Context<Self>) -> impl IntoElement {
        let n = p.number;
        let state_color = match p.state.as_str() {
            "MERGED" => rgb(0xa371f7).into(),
            "CLOSED" => theme::git_deleted(),
            _ if p.is_draft => theme::fg_muted(),
            _ => theme::git_added(),
        };
        let (check, check_color) = match gh::checks(&p.status_check_rollup) {
            gh::Checks::Passing => ("✓", theme::git_added()),
            gh::Checks::Failing => ("✗", theme::git_deleted()),
            gh::Checks::Pending => ("●", theme::git_modified()),
            gh::Checks::None => ("", theme::fg_muted()),
        };
        let review = match (p.is_draft, p.review_decision.as_deref()) {
            (true, _) => Some(("draft", theme::fg_muted())),
            (_, Some("APPROVED")) => Some(("approved", theme::git_added())),
            (_, Some("CHANGES_REQUESTED")) => Some(("changes requested", theme::git_deleted())),
            _ => None,
        };
        let age = github_time(&p.updated_at).map(crate::util::short_time).unwrap_or_default();
        let selected = self.selected == Some(n);
        let tip = format!("#{n} {}\n{} → {} · by {}", p.title, p.head_ref_name, p.base_ref_name, p.author.login);
        div()
            .id(("pr-row", n as usize))
            .debug_selector(move || format!("pr-row-{n}"))
            .flex()
            .gap(px(7.))
            .mx(px(6.))
            .px(px(8.))
            .py(px(4.))
            .rounded(px(5.))
            .cursor_pointer()
            .when(selected, |d| d.bg(theme::selected()))
            .when(!selected, |d| d.hover(|s| s.bg(theme::hover())))
            .tooltip(ui::tooltip_text(tip))
            .on_click(cx.listener(move |p, _, _, cx| {
                p.selected = Some(n);
                if let Some(root) = p.root.clone() {
                    cx.emit(Request::OpenPr(root, n));
                }
                cx.notify();
            }))
            .child(div().flex_none().pt(px(2.)).child(ui::icon("pull-request", state_color).size(px(13.))))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(div().overflow_hidden().text_ellipsis().whitespace_nowrap().text_color(theme::fg_bright()).child(p.title.clone()))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .text_size(px(10.5))
                            .text_color(theme::fg_muted())
                            .whitespace_nowrap()
                            .child(div().flex_none().child(format!("#{n}")))
                            .child(div().min_w_0().overflow_hidden().text_ellipsis().child(format!("· {}", p.author.login)))
                            .when(!age.is_empty(), |d| d.child(div().flex_none().child(format!("· {age}"))))
                            .when_some(review, |d, (t, c)| d.child(div().flex_none().text_color(c).child(format!("· {t}"))))
                            .child(div().flex_1())
                            .when(!check.is_empty(), |d| d.child(div().flex_none().text_color(check_color).child(check))),
                    ),
            )
    }

    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let segment = |key: &'static str, label: &'static str, on: bool| {
            div()
                .id(key)
                .debug_selector(move || format!("pr-filter-{key}"))
                .flex_1()
                .h(px(20.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .cursor_pointer()
                .text_size(px(11.))
                .when(on, |d| d.bg(theme::bg_popup()).text_color(theme::fg_bright()).shadow_sm())
                .when(!on, |d| d.text_color(theme::fg_dim()).hover(|s| s.text_color(theme::fg_bright())))
                .child(label)
        };
        let name = STATES.iter().find(|(k, _)| *k == self.state).map_or("", |s| s.1).to_lowercase();
        let shown = self.lists.get(self.state);
        let loading = self.loading.is_some();
        let count = shown.and_then(|p| p.as_ref().ok()).map(|p| p.len());
        let body: AnyElement = match shown {
            _ if self.root.is_none() => ui::empty_state("pull-request", "No repository", "Open a folder with a GitHub repository.").into_any_element(),
            None => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .debug_selector(|| "pr-loading".into())
                .child(spinner("pr-loading-spin", 20.))
                .into_any_element(),
            Some(Err(e)) if e.contains("No such file") || e.contains("not found") => {
                ui::empty_state("pull-request", "GitHub CLI not found", "Install it (brew install gh), then sign in with gh auth login.").into_any_element()
            }
            Some(Err(e)) if e.contains("auth") => ui::empty_state("pull-request", "Sign in to GitHub", "Run gh auth login in a terminal, then refresh.").into_any_element(),
            Some(Err(e)) => ui::empty_state("warning", "Couldn't list the pull requests", e.lines().next().unwrap_or("").to_string()).into_any_element(),
            Some(Ok(list)) if list.is_empty() => ui::empty_state("pull-request", if self.state == "all" { "No pull requests".to_string() } else { format!("No {name} pull requests") }, "Pick another state above.").into_any_element(),
            Some(Ok(list)) => div()
                .id("pr-list")
                .flex()
                .flex_col()
                .gap(px(1.))
                .pb(px(8.))
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .children(list.clone().iter().map(|p| self.render_row(p, cx).into_any_element()).collect::<Vec<_>>())
                .into_any_element(),
        };
        let back = self.pr.as_ref().map(|p| p.read(cx).number);
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(6.))
                    .h(px(35.))
                    .pl(px(20.))
                    .pr(px(6.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::fg_bright())
                    .child("Pull requests")
                    .when_some(count.filter(|c| *c > 0), |d, c| d.child(div().text_color(theme::fg_muted()).font_weight(FontWeight::NORMAL).child(c.to_string())))
                    .child(div().flex_1())
                    .when_some(back, |d, n| {
                        d.child(ui::icon_button("pr-to-files", "files").debug_selector(|| "pr-to-files".into()).tooltip(ui::tooltip_text(format!("Files of #{n}"))).on_click(cx.listener(|p, _, _, cx| {
                            p.show_list = false;
                            cx.notify();
                        })))
                    })
                    .child(ui::icon_button("pr-new", "plus").debug_selector(|| "pr-new".into()).tooltip(ui::tooltip("Open a pull request from the current branch")).on_click(cx.listener(|_, _, _, cx| cx.emit(Request::NewPr))))
                    .when(loading, |d| d.child(div().size(px(22.)).flex().flex_none().items_center().justify_center().debug_selector(|| "pr-refreshing".into()).child(spinner("pr-refresh-spin", 14.))))
                    .when(!loading, |d| d.child(ui::icon_button("pr-refresh", "refresh").debug_selector(|| "pr-refresh".into()).tooltip(ui::tooltip("Read them again")).on_click(cx.listener(|p, _, _, cx| p.reload(cx))))),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .mx(px(12.))
                    .mb(px(6.))
                    .p(px(2.))
                    .gap(px(2.))
                    .rounded(px(6.))
                    .bg(theme::hover())
                    .children(STATES.map(|(key, label)| {
                        segment(key, label, self.state == key).on_click(cx.listener(move |p, _, _, cx| {
                            p.state = key;
                            p.reload(cx);
                        }))
                    })),
            )
            .child(body)
            .into_any_element()
    }

    /// The files whose path holds `q` (any case), by index; kept until the text or files change.
    fn matching(&mut self, q: &str, pr: &PrView) -> Option<Rc<Vec<usize>>> {
        let Some(Ok(f)) = pr.shown_files() else { return None };
        let key = (q.to_string(), f.list.as_ptr() as usize, f.list.len());
        if let Some((t, ptr, len, m)) = &self.matches
            && (t, ptr, len) == (&key.0, &key.1, &key.2)
        {
            return Some(m.clone());
        }
        let q = q.to_lowercase();
        let m: Rc<Vec<usize>> = Rc::new(f.list.iter().enumerate().filter(|(_, c)| c.path.to_lowercase().contains(&q)).map(|(i, _)| i).collect());
        self.matches = Some((key.0, key.1, key.2, m.clone()));
        Some(m)
    }

    fn render_files(&mut self, pr: Entity<PrView>, cx: &mut Context<Self>) -> AnyElement {
        let q = self.filter.read(cx).text();
        let matched = if q.trim().is_empty() { None } else { self.matching(q.trim(), pr.read(cx)) };
        let v = pr.read(cx);
        let (n, title) = (v.number, v.detail.as_ref().and_then(|d| d.as_ref().ok()).map(|(d, _)| d.title.clone()).unwrap_or_default());
        let scope = v.commit_label();
        let (count, body): (usize, Option<AnyElement>) = match v.shown_files() {
            None => (0, Some(div().px(px(20.)).py(px(8.)).text_color(theme::fg_muted()).child("Bringing its commits here…").into_any_element())),
            Some(Err(e)) => (0, Some(div().px(px(20.)).py(px(8.)).text_color(theme::git_deleted()).child(e.clone()).into_any_element())),
            Some(Ok(f)) => (matched.as_ref().map_or(f.rows.len(), |m| m.len()), None),
        };
        let files_n = match v.shown_files() {
            Some(Ok(f)) => f.list.len(),
            _ => 0,
        };
        let body = body.unwrap_or_else(|| {
            let pr2 = pr.clone();
            let matched = matched.clone();
            uniform_list("pr-tree", count, move |range: std::ops::Range<usize>, _, cx: &mut App| {
                let v = pr2.read(cx);
                let Some(Ok(f)) = v.shown_files() else { return Vec::new() };
                let current = v.file.clone();
                range
                    .map(|i| {
                        let row = match &matched {
                            Some(m) => FileRow::File { ix: m[i], depth: 0 },
                            None => f.rows[i].clone(),
                        };
                        match row {
                            FileRow::Folder { key, label, depth, open } => {
                                let (pr3, k) = (pr2.clone(), key.clone());
                                div()
                                    .id(SharedString::from(format!("pr-dir-{key}")))
                                    .debug_selector(move || format!("pr-dir-{key}"))
                                    .flex()
                                    .items_center()
                                    .gap(px(4.))
                                    .h(px(ROW_H))
                                    .pl(px(10. + depth as f32 * INDENT))
                                    .pr(px(8.))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(theme::hover()))
                                    .whitespace_nowrap()
                                    .on_click(move |_, _, cx| pr3.update(cx, |v, cx| v.toggle_folder(&k, cx)))
                                    .child(ui::small_icon(if open { "chevron-down" } else { "chevron-right" }, theme::fg_muted()))
                                    .child(ui::folder_icon(&label, open).size(px(14.)).flex_none())
                                    .child(div().min_w_0().overflow_hidden().text_ellipsis().text_color(theme::fg()).child(label))
                                    .into_any_element()
                            }
                            FileRow::File { ix, depth } => {
                                let c = &f.list[ix];
                                let on = current.as_deref() == Some(c.path.as_str());
                                let notes = v.comments_on(&c.path);
                                let color = ui::status_color(c.status);
                                let pr3 = pr2.clone();
                                let sel = format!("pr-file-{}", c.path);
                                let flat = matched.is_some();
                                div()
                                    .id(SharedString::from(sel.clone()))
                                    .debug_selector(move || sel)
                                    .flex()
                                    .items_center()
                                    .gap(px(5.))
                                    .h(px(ROW_H))
                                    .pl(px(10. + depth as f32 * INDENT + if flat { 0. } else { 16. }))
                                    .pr(px(8.))
                                    .cursor_pointer()
                                    .when(on, |d| d.bg(theme::selected()))
                                    .when(!on, |d| d.hover(|s| s.bg(theme::hover())))
                                    .whitespace_nowrap()
                                    .tooltip(ui::tooltip_text(c.path.clone()))
                                    .on_click(move |_, _, cx| pr3.update(cx, |v, cx| v.open_ix(ix, cx)))
                                    .child(ui::file_icon(ui::file_name(&c.path)).size(px(14.)).flex_none())
                                    .child(div().flex_none().max_w(relative(0.75)).overflow_hidden().text_ellipsis().text_color(color).when(c.status == crate::git::FileStatus::Deleted, |d| d.line_through()).child(ui::file_name(&c.path).to_string()))
                                    .when(flat, |d| d.child(div().min_w_0().overflow_hidden().text_ellipsis().text_size(px(10.5)).text_color(theme::fg_muted()).child(ui::file_dir(&c.path).to_string())))
                                    .child(div().flex_1())
                                    .when(notes > 0, |d| d.child(div().flex_none().text_size(px(10.)).text_color(theme::accent()).child(format!("💬{notes}"))))
                                    .child(div().flex_none().w(px(12.)).text_center().text_size(px(10.5)).font_family(theme::MONO_FONT).font_weight(FontWeight::SEMIBOLD).text_color(color).child(c.status.letter()))
                                    .into_any_element()
                            }
                        }
                    })
                    .collect()
            })
            .track_scroll(&self.scroll)
            .flex_1()
            .into_any_element()
        });
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(4.))
                    .h(px(35.))
                    .pl(px(8.))
                    .pr(px(6.))
                    .child(ui::icon_button("pr-back-to-list", "chevron-left").debug_selector(|| "pr-back-to-list".into()).tooltip(ui::tooltip("All pull requests")).on_click(cx.listener(|p, _, _, cx| {
                        p.show_list = true;
                        cx.notify();
                    })))
                    .child(div().flex_none().text_color(theme::fg_muted()).child(format!("#{n}")))
                    .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().font_weight(FontWeight::SEMIBOLD).text_color(theme::fg_bright()).child(title)),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(6.))
                    .px(px(12.))
                    .pb(px(6.))
                    .text_size(px(11.))
                    .text_color(theme::fg_muted())
                    .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(match &scope {
                        Some(c) => format!("{files_n} files in {c}"),
                        None => format!("{files_n} files changed"),
                    }))
                    .when(scope.is_some(), |d| {
                        let pr = pr.clone();
                        d.child(div().id("pr-all-commits").debug_selector(|| "pr-all-commits".into()).flex_none().cursor_pointer().text_color(theme::accent()).child("Show all").on_click(move |_, _, cx| pr.update(cx, |v, cx| v.pick_commit(None, cx))))
                    }),
            )
            .child(div().id("pr-files-filter").debug_selector(|| "pr-files-filter".into()).flex_none().mx(px(12.)).mb(px(6.)).h(px(24.)).px(px(6.)).flex().items_center().rounded(px(4.)).bg(theme::bg_input()).border_1().border_color(theme::border_input()).overflow_hidden().child(self.filter.clone()))
            .child(div().flex_none().h(px(1.)).bg(theme::border()))
            .child(div().flex().flex_col().flex_1().min_h_0().pt(px(4.)).child(body))
            .into_any_element()
    }
}

impl Render for PrPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.pr.clone().filter(|_| !self.show_list) {
            Some(pr) => self.render_files(pr, cx),
            None => self.render_list(cx),
        };
        div().key_context("PrPanel").track_focus(&self.focus).size_full().text_size(px(12.)).child(body)
    }
}

/// GitHub's "2026-11-14T10:00:00Z" as seconds since 1970.
pub fn github_time(iso: &str) -> Option<i64> {
    let num = |r: std::ops::Range<usize>| iso.get(r)?.parse::<i64>().ok();
    let (y, m, d, hh, mm, ss) = (num(0..4)?, num(5..7)?, num(8..10)?, num(11..13)?, num(14..16)?, num(17..19)?);
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146_097 + doe - 719_468) * 86_400 + hh * 3600 + mm * 60 + ss)
}

#[cfg(test)]
mod tests {
    use super::github_time as updated_at;

    #[test]
    fn reads_github_times() {
        assert_eq!(updated_at("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(updated_at("2026-11-14T10:00:00Z"), Some(1_794_650_400));
        assert_eq!(updated_at("soon"), None);
    }
}

/// A turning arrow while something is read.
fn spinner(id: &'static str, size: f32) -> impl IntoElement {
    ui::icon("refresh", theme::fg_muted())
        .size(px(size))
        .with_animation(id, Animation::new(std::time::Duration::from_millis(900)).repeat(), |icon, t| icon.with_transformation(Transformation::rotate(percentage(t))))
}
