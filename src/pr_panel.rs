//! The Pull requests sidebar: the repository's pull requests (read with `gh`), filtered by
//! state; a click reviews one in its own tab (its files' tree is in the tab, beside the diff).

use crate::git::gh;
use crate::git_panel::pr::PrView;
use crate::request::Request;
use crate::theme;
use crate::ui::{self, px};
use gpui::prelude::*;
use gpui::*;
use std::path::PathBuf;

const STATES: [(&str, &str); 4] = [("open", "Open"), ("merged", "Merged"), ("closed", "Closed"), ("all", "All")];

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
    _subs: Vec<Subscription>,
}

impl EventEmitter<Request> for PrPanel {}

impl PrPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            root: None,
            state: "open",
            lists: Default::default(),
            loading: None,
            selected: None,
            _load: None,
            _subs: Vec::new(),
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

    /// The review tab in front (None: another tab): its row is marked in the list.
    pub fn set_pr(&mut self, pr: Option<Entity<PrView>>, cx: &mut Context<Self>) {
        if let Some(p) = &pr {
            let n = p.read(cx).number;
            if self.selected != Some(n) {
                self.selected = Some(n);
                cx.notify();
            }
        }
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
}

impl Render for PrPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = self.render_list(cx);
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
pub(crate) fn spinner(id: impl Into<ElementId>, size: f32) -> impl IntoElement {
    ui::icon("refresh", theme::fg_muted())
        .size(px(size))
        .with_animation(id, Animation::new(std::time::Duration::from_millis(900)).repeat(), |icon, t| icon.with_transformation(Transformation::rotate(percentage(t))))
}
