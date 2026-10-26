//! Commit details: hash, author, message and the files it changed.

use crate::git::{self, CommitDetails, FileStatus, RefKind, Rev};
use crate::request::{DiffSpec, Request};
use crate::{theme, ui, util};
use gpui::prelude::*;
use gpui::*;
use std::path::PathBuf;

actions!(commit_view, [Next, Prev, OpenSelected, CopySha]);

pub fn bind_keys(cx: &mut App) {
    let c = Some("CommitView");
    cx.bind_keys([
        KeyBinding::new("j", Next, c),
        KeyBinding::new("down", Next, c),
        KeyBinding::new("k", Prev, c),
        KeyBinding::new("up", Prev, c),
        KeyBinding::new("enter", OpenSelected, c),
        KeyBinding::new("o", OpenSelected, c),
        KeyBinding::new("y", CopySha, c),
        KeyBinding::new("cmd-c", CopySha, c),
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
        Self { focus: cx.focus_handle(), root, sha, details: None, error: None, selected: 0, scroll: ScrollHandle::new(), _task: task }
    }

    pub fn title(&self) -> String {
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
            spec: DiffSpec { path: file.path.clone(), old_path: file.orig_path.clone(), left, right },
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

        let Some(d) = &self.details else {
            let msg = self.error.clone().unwrap_or_else(|| "Loading commit…".into());
            return root.child(div().p(px(24.)).text_color(theme::fg_muted()).child(msg));
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

        let files = d.files.iter().enumerate().map(|(ix, f)| {
            let name = ui::file_name(&f.path).to_string();
            let dir = ui::file_dir(&f.path).to_string();
            let selected = ix == self.selected;
            div()
                .id(("commit-file", ix))
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
                .child(div().text_size(px(20.)).text_color(theme::fg_bright()).child(d.subject().to_string()))
                .child(refs)
                .child(
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
                )
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
                        .child(div().text_color(theme::fg_muted()).text_size(px(11.)).child("j/k to move · enter for diff · y copies hash")),
                )
                .child(div().flex().flex_col().children(files)),
        )
    }
}
