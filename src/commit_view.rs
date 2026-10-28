//! Commit details: hash, author, message and the files it changed.

use crate::git::{self, CommitDetails, FileStatus, RefKind, Rev};
use crate::request::{Back, DiffSpec, Request};
use crate::{theme, ui, util};
use gpui::prelude::*;
use gpui::*;
use std::path::PathBuf;

actions!(commit_view, [Next, Prev, OpenSelected, CopySha]);

pub fn bind_keys() {
    let c = Some("CommitView");
    crate::keybindings::collect([
        crate::keybindings::kb("{down}", Next, c),
        crate::keybindings::kb("down", Next, c),
        crate::keybindings::kb("{up}", Prev, c),
        crate::keybindings::kb("up", Prev, c),
        crate::keybindings::kb("enter", OpenSelected, c),
        crate::keybindings::kb("o", OpenSelected, c),
        crate::keybindings::kb("y", CopySha, c),
        crate::keybindings::kb("cmd-c", CopySha, c),
    ]);
}

pub struct CommitView {
    focus: FocusHandle,
    root: PathBuf,
    pub sha: String,
    details: Option<CommitDetails>,
    error: Option<String>,
    selected: usize,
    scroll: ScrollHandle,
    /// Comparing two branches (base, other) instead of showing one commit.
    pub compare: Option<(String, String)>,
    /// When comparing: commits `other` has that `base` lacks, and the other way round.
    ahead_behind: Option<(usize, usize)>,
    /// Branches to pick from when comparing.
    branches: Vec<String>,
    /// The open branch picker: true for the base side.
    picker: Option<bool>,
    _task: Task<()>,
}

impl EventEmitter<Request> for CommitView {}

impl CommitView {
    pub fn new(root: PathBuf, sha: String, cx: &mut Context<Self>) -> Self {
        let (r, s) = (root.clone(), sha.clone());
        let load = cx.background_executor().spawn(async move { git::commit_details(&r, &s) });
        let task = cx.spawn(async move |this, cx| {
            let result = load.await;
            this.update(cx, |view, cx| {
                match result {
                    Ok(d) => view.details = Some(d),
                    Err(e) => view.error = Some(e.to_string()),
                }
                cx.notify();
            })
            .ok();
        });
        Self::with(root, sha, None, task, cx)
    }

    fn with(root: PathBuf, sha: String, compare: Option<(String, String)>, task: Task<()>, cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            root,
            sha,
            details: None,
            error: None,
            selected: 0,
            scroll: ScrollHandle::new(),
            compare,
            ahead_behind: None,
            branches: Vec::new(),
            picker: None,
            _task: task,
        }
    }

    /// What `other` changed since it split from `base` (like comparing branches on GitHub).
    pub fn new_compare(root: PathBuf, base: String, other: String, cx: &mut Context<Self>) -> Self {
        let mut view = Self::with(root, String::new(), None, Task::ready(()), cx);
        view.set_compare(base, other, cx);
        view
    }

    /// Compares another pair of branches in this page.
    pub fn set_compare(&mut self, base: String, other: String, cx: &mut Context<Self>) {
        self.sha = compare_key(&base, &other);
        self.compare = Some((base.clone(), other.clone()));
        self.picker = None;
        self.error = None;
        self.selected = 0;
        let r = self.root.clone();
        let load = cx.background_executor().spawn(async move { (git::compare(&r, &base, &other), git::branches(&r)) });
        self._task = cx.spawn(async move |this, cx| {
            let (result, branches) = load.await;
            this.update(cx, |view, cx| {
                view.branches = branches;
                match result {
                    Ok(c) => {
                        view.details = Some(c.details);
                        view.ahead_behind = Some((c.ahead, c.behind));
                    }
                    Err(e) => {
                        view.details = None;
                        view.error = Some(e.to_string());
                    }
                }
                cx.notify();
            })
            .ok();
        });
        cx.notify();
    }

    pub fn title(&self) -> String {
        if let Some((a, b)) = &self.compare {
            return format!("{a} ↔ {b}");
        }
        match &self.details {
            Some(d) => format!("{} {}", &d.sha[..7], d.subject()),
            None => self.sha[..self.sha.len().min(7)].to_string(),
        }
    }

    pub fn focus(&self, window: &mut Window) {
        window.focus(&self.focus);
    }

    fn open(&self, ix: usize, preview: bool, cx: &mut Context<Self>) {
        let Some(d) = &self.details else { return };
        let Some(file) = d.files.get(ix) else { return };
        let left = match (d.parents.first(), file.status) {
            (_, FileStatus::Added) | (None, _) => Rev::Empty,
            (Some(p), _) => Rev::Commit(p.clone()),
        };
        let right = if file.status == FileStatus::Deleted { Rev::Empty } else { Rev::Commit(d.sha.clone()) };
        cx.emit(Request::OpenDiff {
            spec: DiffSpec {
                path: file.path.clone(),
                old_path: file.orig_path.clone(),
                left,
                right,
                back: Some(match &self.compare {
                    Some((base, other)) => Back::Compare(base.clone(), other.clone()),
                    None => Back::Commit(d.sha.clone()),
                }),
            },
            preview,
        });
    }

    fn next(&mut self, _: &Next, _: &mut Window, cx: &mut Context<Self>) {
        let n = self.details.as_ref().map(|d| d.files.len()).unwrap_or(0);
        self.selected = (self.selected + 1).min(n.saturating_sub(1));
        cx.notify();
    }
    fn prev(&mut self, _: &Prev, _: &mut Window, cx: &mut Context<Self>) {
        self.selected = self.selected.saturating_sub(1);
        cx.notify();
    }
    fn open_selected(&mut self, _: &OpenSelected, _: &mut Window, cx: &mut Context<Self>) {
        self.open(self.selected, true, cx);
    }
    fn copy_sha(&mut self, _: &CopySha, _: &mut Window, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.sha.clone()));
        cx.emit(Request::Message(format!("Copied {}", self.sha)));
    }
}

impl CommitView {
    /// `base ← compared` pickers, a swap button, and how far apart the branches are.
    fn render_compare_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (base, other) = self.compare.clone().unwrap_or_default();
        let counts = self.ahead_behind.map(|(ahead, behind)| {
            let commits = |n: usize| if n == 1 { "1 commit".to_string() } else { format!("{n} commits") };
            let text = match (ahead, behind) {
                (0, 0) => format!("{other} and {base} point at the same history"),
                (a, 0) => format!("{other} is {} ahead of {base}", commits(a)),
                (0, b) => format!("{other} is {} behind {base}", commits(b)),
                (a, b) => format!("{other} is {} ahead and {} behind {base}", commits(a), commits(b)),
            };
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().text_color(theme::git_added()).child(format!("↑{ahead}")))
                .child(div().text_color(theme::git_deleted()).child(format!("↓{behind}")))
                .child(div().text_color(theme::fg_dim()).child(text))
        });
        let (b1, b2) = (base.clone(), other.clone());
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(px(8.))
                    .child(div().text_color(theme::fg_muted()).child("base"))
                    .child(self.branch_chip(true, &base, cx))
                    .child(div().text_color(theme::fg_muted()).child("←"))
                    .child(div().text_color(theme::fg_muted()).child("compare"))
                    .child(self.branch_chip(false, &other, cx))
                    .child(
                        div()
                            .id("compare-swap")
                            .debug_selector(|| "compare-swap".into())
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .px(px(8.))
                            .h(px(24.))
                            .rounded(px(4.))
                            .text_color(theme::accent())
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::hover()))
                            .tooltip(ui::tooltip("Swap base and compare"))
                            .on_click(cx.listener(move |v, _, _, cx| v.set_compare(b2.clone(), b1.clone(), cx)))
                            .child("⇄ Swap"),
                    ),
            )
            .children(counts)
    }

    fn branch_chip(&self, base_side: bool, name: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let open = self.picker == Some(base_side);
        let id = if base_side { "compare-base" } else { "compare-other" };
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .id(id)
                    .debug_selector(move || id.into())
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(8.))
                    .h(px(24.))
                    .rounded(px(4.))
                    .bg(theme::bg_input())
                    .border_1()
                    .border_color(if open { theme::focus_border() } else { theme::border_input() })
                    .font_family(theme::MONO_FONT)
                    .text_color(theme::fg_bright())
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::hover()))
                    .on_click(cx.listener(move |v, _, _, cx| {
                        v.picker = if open { None } else { Some(base_side) };
                        cx.notify();
                    }))
                    .child(ui::small_icon("git", theme::fg_dim()))
                    .child(name.to_string())
                    .child(ui::small_icon("chevron-down", theme::fg_dim())),
            )
            .when(open, |d| d.child(deferred(anchored().snap_to_window().child(self.render_picker(base_side, cx))).with_priority(2)))
    }

    fn render_picker(&self, base_side: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let (base, other) = self.compare.clone().unwrap_or_default();
        let current = if base_side { &base } else { &other };
        div()
            .id("compare-picker")
            .occlude()
            .mt(px(4.))
            .min_w(px(220.))
            .max_h(px(360.))
            .py(px(4.))
            .bg(theme::bg_popup())
            .border_1()
            .border_color(theme::border_input())
            .rounded(px(6.))
            .shadow_lg()
            .overflow_y_scroll()
            .on_mouse_down_out(cx.listener(|v, _, _, cx| {
                v.picker = None;
                cx.notify();
            }))
            .children(self.branches.iter().enumerate().map(|(ix, b)| {
                let active = b == current;
                let pick = b.clone();
                let (base, other) = (base.clone(), other.clone());
                div()
                    .id(("compare-branch", ix))
                    .debug_selector({
                        let b = b.clone();
                        move || format!("compare-branch-{b}")
                    })
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .h(px(24.))
                    .px(px(8.))
                    .mx(px(4.))
                    .rounded(px(3.))
                    .font_family(theme::MONO_FONT)
                    .cursor_pointer()
                    .when(active, |d| d.bg(theme::selected()))
                    .hover(|s| s.bg(theme::selected_focused()))
                    .on_click(cx.listener(move |v, _, _, cx| {
                        if base_side {
                            v.set_compare(pick.clone(), other.clone(), cx);
                        } else {
                            v.set_compare(base.clone(), pick.clone(), cx);
                        }
                    }))
                    .child(div().w(px(10.)).text_color(theme::frame_focused()).child(if active { "●" } else { "" }))
                    .child(b.clone())
            }))
    }

    #[cfg(test)]
    pub fn ahead_behind(&self) -> Option<(usize, usize)> {
        self.ahead_behind
    }
}

/// The tab key of a comparison (the same pair opens the same tab).
pub fn compare_key(base: &str, other: &str) -> String {
    format!("{base}...{other}")
}

fn meta_row(label: &str, value: impl IntoElement) -> Div {
    div()
        .flex()
        .gap(px(12.))
        .child(div().w(px(80.)).flex_none().text_color(theme::fg_muted()).child(label.to_string()))
        .child(value)
}

impl Focusable for CommitView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for CommitView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        let root = div()
            .key_context("CommitView")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::next))
            .on_action(cx.listener(Self::prev))
            .on_action(cx.listener(Self::open_selected))
            .on_action(cx.listener(Self::copy_sha))
            .on_mouse_down(MouseButton::Left, cx.listener(|v, _, w, _| w.focus(&v.focus)))
            .size_full()
            .bg(theme::bg_editor());

        let Some(d) = self.details.as_ref().filter(|_| self.error.is_none()) else {
            let msg = self.error.clone().unwrap_or_else(|| "Loading…".into());
            return root.child(
                div()
                    .p(px(24.))
                    .flex()
                    .flex_col()
                    .gap(px(14.))
                    .when(self.compare.is_some(), |el| el.child(self.render_compare_bar(cx).into_any_element()))
                    .child(div().text_color(theme::fg_muted()).child(msg)),
            );
        };

        let sha = d.sha.clone();
        let refs = div().flex().gap(px(6.)).children(d.refs.iter().map(|r| {
            let (color, icon) = match r.kind {
                RefKind::Local => (theme::ref_local(), "target"),
                RefKind::Remote => (theme::ref_remote(), "cloud"),
                RefKind::Tag => (theme::ref_tag(), "tag"),
            };
            div()
                .flex()
                .items_center()
                .gap(px(4.))
                .px(px(6.))
                .h(px(18.))
                .rounded(px(9.))
                .bg(color)
                .text_color(theme::c(0xffffff))
                .text_size(px(10.5))
                .child(ui::small_icon(icon, theme::c(0xffffff)))
                .child(r.name.clone())
        }));

        let parents = div().flex().gap(px(8.)).children(d.parents.iter().map(|p| {
            let p2 = p.clone();
            div()
                .id(SharedString::from(format!("parent-{p}")))
                .font_family(theme::MONO_FONT)
                .text_color(theme::accent())
                .cursor_pointer()
                .hover(|s| s.underline())
                .on_click(cx.listener(move |_, _, _, cx| cx.emit(Request::OpenCommit(p2.clone()))))
                .child(p[..7].to_string())
        }));

        let compare_bar = self.compare.is_some().then(|| self.render_compare_bar(cx).into_any_element());
        let files = d.files.iter().enumerate().map(|(ix, f)| {
            let name = ui::file_name(&f.path).to_string();
            let dir = ui::file_dir(&f.path).to_string();
            let selected = ix == self.selected;
            div()
                .id(("commit-file", ix))
                .debug_selector({
                    let name = name.clone();
                    move || format!("commit-file-{name}")
                })
                .flex()
                .items_center()
                .gap(px(6.))
                .h(px(24.))
                .px(px(8.))
                .rounded(px(3.))
                .cursor_pointer()
                .when(selected, |d| d.bg(if focused { theme::selected_focused() } else { theme::selected() }))
                .hover(|s| s.bg(theme::hover()))
                .on_click(cx.listener(move |view, ev: &ClickEvent, _, cx| {
                    view.selected = ix;
                    view.open(ix, ev.click_count() < 2, cx);
                    cx.notify();
                }))
                .child(ui::file_icon(&name))
                .child(div().text_color(ui::status_color(f.status)).child(name))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_color(theme::fg_muted())
                        .text_size(px(11.))
                        .child(match &f.orig_path {
                            Some(old) => format!("{old} → {dir}"),
                            None => dir,
                        }),
                )
                .child(div().w(px(16.)).text_color(ui::status_color(f.status)).child(f.status.letter()))
        });

        root.child(
            div()
                .id("commit-scroll")
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .p(px(24.))
                .flex()
                .flex_col()
                .gap(px(14.))
                .when(self.compare.is_none(), |el| el.child(div().text_size(px(20.)).text_color(theme::fg_bright()).child(d.subject().to_string())).child(refs))
                .when_some(self.compare.clone(), |el, (a, b)| {
                    let split = d.parents.first().map(|p| p[..p.len().min(7)].to_string()).unwrap_or_default();
                    el.children(compare_bar).child(
                        div()
                            .text_color(theme::fg_muted())
                            .text_size(px(11.5))
                            .child(format!("Files below: what {b} changed since it split from {a} (at {split}).")),
                    )
                })
                .when(self.compare.is_none(), |el| el.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(meta_row(
                            "Commit",
                            div()
                                .id("sha")
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .font_family(theme::MONO_FONT)
                                .cursor_pointer()
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(sha.clone()));
                                    cx.emit(Request::Message("Commit hash copied".into()));
                                }))
                                .child(d.sha.clone())
                                .child(ui::small_icon("copy", theme::fg_muted())),
                        ))
                        .child(meta_row("Author", div().child(format!("{} <{}>", d.author, d.email))))
                        .child(meta_row("Date", div().child(format!("{}  ({})", d.date, util::relative_time(d.time)))))
                        .when(d.committer != d.author, |el| el.child(meta_row("Committer", div().child(d.committer.clone()))))
                        .when(!d.parents.is_empty(), |el| el.child(meta_row("Parents", parents))),
                ))
                .when(!d.body().is_empty(), |el| {
                    el.child(
                        div()
                            .p(px(12.))
                            .rounded(px(4.))
                            .bg(theme::bg_side())
                            .font_family(theme::MONO_FONT)
                            .text_size(px(11.5))
                            .children(d.body().lines().map(|l| div().min_h(px(18.)).child(l.to_string()))),
                    )
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .pt(px(6.))
                        .border_t_1()
                        .border_color(theme::border())
                        .child(div().font_weight(FontWeight::BOLD).child(format!("{} files changed", d.files.len())))
                        .child(div().text_color(theme::fg_muted()).text_size(px(11.)).child({
                            let d: Vec<char> = crate::keybindings::direction_letters().chars().collect();
                            let (down, up) = (d.get(1).copied().unwrap_or('k'), d.get(2).copied().unwrap_or('i'));
                            format!("{down}/{up} or ↓/↑ to move · enter for diff · y copies hash")
                        })),
                )
                .child(div().flex().flex_col().children(files)),
        )
    }
}
