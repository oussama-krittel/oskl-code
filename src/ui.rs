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

pub fn status_color(s: FileStatus) -> Hsla {
    match s {
        FileStatus::Modified | FileStatus::TypeChanged => theme::git_modified(),
        FileStatus::Added | FileStatus::Untracked | FileStatus::Renamed | FileStatus::Copied => theme::git_added(),
        FileStatus::Deleted => theme::git_deleted(),
        FileStatus::Conflicted => theme::git_conflict(),
    }
}

pub fn file_color(name: &str) -> Hsla {
    theme::c(crate::icons::file_icon(name).1)
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

/// Collapsible section header (e.g. "CHANGES", "GRAPH").
/// A foldable section's header. Open, only its arrow and title fold it (so a missed click on
/// the buttons or counts beside them doesn't); folded, clicking anywhere on it opens it.
pub fn section_header(id: impl Into<ElementId>, title: &str, open: bool, toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Stateful<Div> {
    let id: ElementId = id.into();
    let title_id = ElementId::NamedChild(Box::new(id.clone()), "title".into());
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
        .child(title.to_uppercase());
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
        .font_weight(FontWeight::BOLD)
        .text_size(px(10.5));
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
