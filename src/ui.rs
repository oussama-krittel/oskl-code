//! Small shared UI pieces.

use crate::assets::icon_path;
use crate::git::FileStatus;
use crate::theme;
use gpui::prelude::*;
use gpui::*;
use std::sync::atomic::{AtomicU32, Ordering};

/// Interface zoom (⌘⇧+ / ⌘⇧−), 1.0 = 100%: panels, tabs, bars and popups, not code text.
/// App-wide; in tests, per thread, so tests running side by side don't zoom each other.
#[cfg(not(test))]
static SCALE: AtomicU32 = AtomicU32::new(0x3f80_0000);
#[cfg(test)]
thread_local!(static SCALE: AtomicU32 = const { AtomicU32::new(0x3f80_0000) });

pub fn scale() -> f32 {
    #[cfg(not(test))]
    return f32::from_bits(SCALE.load(Ordering::Relaxed));
    #[cfg(test)]
    return SCALE.with(|s| f32::from_bits(s.load(Ordering::Relaxed)));
}

pub fn set_scale(s: f32) {
    #[cfg(not(test))]
    SCALE.store(s.to_bits(), Ordering::Relaxed);
    #[cfg(test)]
    SCALE.with(|x| x.store(s.to_bits(), Ordering::Relaxed));
}

/// `gpui::px` times the interface zoom. The interface's files use this one (it shadows gpui's);
/// editors, diffs and terminals keep gpui's, so code text follows only ⌘+ / ⌘−.
pub fn px(v: f32) -> Pixels {
    gpui::px(v * scale())
}

pub fn icon(name: &str, color: Hsla) -> Svg {
    svg().path(icon_path(name)).size(px(16.)).flex_none().text_color(color)
}

pub fn small_icon(name: &str, color: Hsla) -> Svg {
    svg().path(icon_path(name)).size(px(14.)).flex_none().text_color(color)
}

/// A square hover button with an icon.
pub fn icon_button(id: impl Into<ElementId>, name: &str) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(22.))
        .rounded(px(4.))
        .cursor_pointer()
        .hover(|s| s.bg(theme::ca(0x5a5d5e50)))
        .child(icon(name, theme::fg()))
}

/// A changed file in the flat list, in Source Control and the Git tab: the status letter in a
/// fixed column, then the name (in the status color) with its folder dimmed beside it. `name_sel`
/// is the name's test selector.
pub fn change_cells(status: FileStatus, path: &str, name_sel: Option<String>) -> Div {
    let color = status_color(status);
    div()
        .flex()
        .flex_1()
        .min_w_0()
        .items_center()
        .gap(px(6.))
        .overflow_hidden()
        .whitespace_nowrap()
        .child(div().flex_none().w(px(14.)).text_center().text_size(px(11.)).font_family(theme::MONO_FONT).font_weight(FontWeight::SEMIBOLD).text_color(color).child(status.letter()))
        .child(
            div()
                .when_some(name_sel, |d, s| d.debug_selector(move || s))
                .flex_none()
                .max_w(relative(0.7))
                .overflow_hidden()
                .text_ellipsis()
                .text_color(color)
                .when(status == FileStatus::Deleted, |d| d.line_through())
                .child(file_name(path).to_string()),
        )
        .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().text_size(px(11.)).text_color(theme::fg_muted()).child(file_dir(path).to_string()))
}

/// Buttons over the right end of a row: they show with the row's hover (or `show`) and sit on
/// top of the folder text, so nothing in the row moves when they appear. `group` is the row's
/// group name and `bg` the color under them.
pub fn hover_actions(group: &'static str, show: bool, bg: Hsla) -> Div {
    div()
        .absolute()
        .right(px(4.))
        .top(px(1.))
        .bottom(px(1.))
        .flex()
        .items_center()
        .gap(px(1.))
        .pl(px(6.))
        .rounded(px(4.))
        .bg(bg)
        .when(!show, |d| d.invisible().group_hover(group, |s| s.visible()))
}

/// What a pane shows when it has nothing to list: an icon, a title and a line of help.
pub fn empty_state(name: &str, title: impl Into<SharedString>, hint: impl Into<SharedString>) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(8.))
        .p(px(24.))
        .text_center()
        .text_size(px(12.))
        .text_color(theme::fg_dim())
        .child(icon(name, theme::fg_muted()).size(px(22.)))
        .child(div().text_size(px(14.)).text_color(theme::fg_bright()).child(title.into()))
        .child(div().max_w(px(380.)).min_w_0().w_full().child(hint.into()))
}

/// A dialog centered near the top of the window, over a dimmed backdrop as big as the window.
pub fn dialog_scrim(vp: Size<Pixels>, dialog: impl IntoElement) -> Stateful<Div> {
    div().id("dialog-scrim").occlude().w(vp.width).h(vp.height).flex().items_start().justify_center().pt(vp.height * 0.1).bg(theme::ca(0x00000066)).child(dialog)
}

pub fn status_color(s: FileStatus) -> Hsla {
    match s {
        FileStatus::Modified | FileStatus::TypeChanged => theme::git_modified(),
        FileStatus::Added | FileStatus::Untracked | FileStatus::Renamed | FileStatus::Copied => theme::git_added(),
        FileStatus::Deleted => theme::git_deleted(),
        FileStatus::Conflicted => theme::git_conflict(),
    }
}

/// Material-style icon for a file name.
pub fn file_icon(name: &str) -> Svg {
    let (key, color) = crate::icons::file_icon(name);
    icon(&key, theme::c(color))
}

/// Material-style folder icon, colored by folder name.
pub fn folder_icon(name: &str, open: bool) -> Svg {
    icon(if open { "fd-open" } else { "fd-closed" }, theme::c(crate::icons::folder_color(name)))
}

pub fn badge(text: impl Into<SharedString>, bg: Hsla) -> Div {
    div()
        .flex_none()
        .px(px(6.))
        .h(px(18.))
        .min_w(px(18.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(9.))
        .bg(bg)
        .text_color(theme::on(bg))
        .text_size(px(10.5))
        .child(text.into())
}

/// A foldable section's header. Open, only its arrow and title fold it (so a missed click on
/// the buttons or counts beside them doesn't); folded, clicking anywhere on it opens it.
pub fn section_header(id: impl Into<ElementId>, title: &str, open: bool, toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Stateful<Div> {
    let id: ElementId = id.into();
    let title_id = ElementId::NamedChild(std::sync::Arc::new(id.clone()), "title".into());
    let (sel, title_sel) = (format!("section-{}", title.to_lowercase()), format!("section-{}-title", title.to_lowercase()));
    let label = div()
        .id(title_id)
        .debug_selector(move || title_sel)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(2.))
        .h_full()
        .pr(px(6.))
        .cursor_pointer()
        .child(small_icon(if open { "chevron-down" } else { "chevron-right" }, theme::fg()))
        .child(title.to_string());
    let header = div()
        .id(id)
        .debug_selector(move || sel)
        .flex()
        .flex_none()
        .items_center()
        .h(px(22.))
        .pl(px(2.))
        .pr(px(6.))
        .gap(px(2.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::fg_dim())
        .text_size(px(12.));
    if open {
        header.child(label.on_click(toggle)).child(div().flex_1())
    } else {
        header.cursor_pointer().on_click(toggle).child(label).child(div().flex_1())
    }
}

pub fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

pub fn file_dir(path: &str) -> &str {
    path.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
}

pub struct TextTooltip(SharedString);

impl Render for TextTooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(8.))
            .py(px(4.))
            .bg(theme::bg_popup())
            .border_1()
            .border_color(theme::border_input())
            .rounded(px(4.))
            .text_size(px(11.))
            .text_color(theme::fg())
            .child(self.0.clone())
    }
}

pub fn tooltip(text: &'static str) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    move |_, cx| cx.new(|_| TextTooltip(text.into())).into()
}

pub fn tooltip_text(text: String) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text: SharedString = text.into();
    move |_, cx| cx.new(|_| TextTooltip(text.clone())).into()
}

/// A pane item (file, diff, terminal…) being dragged by its title-bar chip.
#[derive(Clone)]
pub struct DraggedItem {
    pub tab_id: usize,
    pub label: String,
}

/// A file being dragged from the explorer.
#[derive(Clone)]
pub struct DraggedFile {
    pub path: std::path::PathBuf,
    pub label: String,
}

/// What follows the cursor while dragging.
pub struct DragChip(pub String);

impl Render for DragChip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(8.))
            .py(px(3.))
            .rounded(px(4.))
            .bg(theme::bg_popup())
            .border_1()
            .border_color(theme::frame_focused())
            .text_size(px(11.5))
            .font_family(theme::MONO_FONT)
            .text_color(theme::fg_bright())
            .child(self.0.clone())
    }
}

/// Branch and tag labels for one commit: a local branch whose remote twin (`origin/main`)
/// points at the same commit shows once, with a cloud mark. Each item is the ref to act on,
/// whether it carries the mark, and its place in `refs` (for element ids).
pub fn ref_labels(refs: &[crate::git::Ref]) -> Vec<(&crate::git::Ref, bool, usize)> {
    use crate::git::RefKind;
    let twin = |r: &crate::git::Ref| refs.iter().position(|l| l.kind == RefKind::Local && r.name.split_once('/').is_some_and(|(_, n)| n == l.name));
    // shortcut: the twin is found by name (origin/<branch>), not by the branch's configured upstream.
    refs.iter()
        .enumerate()
        .filter(|(_, r)| r.kind != RefKind::Remote || twin(r).is_none())
        .map(|(i, r)| {
            let cloud = r.kind == RefKind::Local && refs.iter().any(|o| o.kind == RefKind::Remote && twin(o) == Some(i));
            (r, cloud, i)
        })
        .collect()
}

/// A branch or tag chip: outlined in its color, filled with the accent only for the checked-out branch.
pub fn ref_chip(r: &crate::git::Ref, cloud: bool, h: f32) -> Div {
    use crate::git::RefKind;
    let (color, icon) = match r.kind {
        RefKind::Local => (theme::ref_local(), if r.is_head { "target" } else { "git" }),
        RefKind::Remote => (theme::ref_remote(), "cloud"),
        RefKind::Tag => (theme::ref_tag(), "tag"),
    };
    let text = if r.is_head { theme::on(theme::accent()) } else { color };
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(3.))
        .pl(px(5.))
        .pr(px(7.))
        .h(px(h))
        .rounded(px(h / 2.))
        .border_1()
        .font_family(theme::MONO_FONT)
        .text_size(px(10.5))
        .text_color(text)
        .when(r.is_head, |d| d.bg(theme::accent()).border_color(theme::accent()).font_weight(FontWeight::SEMIBOLD))
        .when(!r.is_head, |d| d.border_color(color.opacity(0.5)))
        .child(small_icon(icon, text).size(px(h - 7.)))
        .child(r.name.clone())
        .when(cloud, |d| d.child(small_icon("cloud", text).size(px(10.))))
}

#[cfg(test)]
mod ref_label_tests {
    use super::ref_labels;
    use crate::git::{Ref, RefKind};

    #[test]
    fn a_branch_and_its_remote_twin_show_once_with_a_cloud() {
        let r = |name: &str, kind| Ref { name: name.into(), kind, is_head: false };
        let refs = [r("main", RefKind::Local), r("origin/main", RefKind::Remote), r("origin/dev", RefKind::Remote), r("v1", RefKind::Tag)];
        let shown: Vec<_> = ref_labels(&refs).into_iter().map(|(r, cloud, _)| (r.name.as_str(), cloud)).collect();
        assert_eq!(shown, [("main", true), ("origin/dev", false), ("v1", false)]);
    }
}
