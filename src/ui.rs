//! Small shared UI pieces.

use crate::assets::icon_path;
use crate::git::FileStatus;
use crate::theme;
use gpui::prelude::*;
use gpui::*;

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
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    theme::c(match ext.as_str() {
        "ts" | "mts" | "cts" => 0x519aba,
        "tsx" | "jsx" => 0x4fc1e9,
        "js" | "mjs" | "cjs" => 0xcbcb41,
        "json" | "jsonc" => 0xcbcb41,
        "md" | "mdx" => 0x519aba,
        "css" | "scss" | "less" => 0x519aba,
        "html" | "vue" | "svelte" => 0xe37933,
        "rs" => 0xdea584,
        "py" => 0x3c78aa,
        "go" => 0x519aba,
        "swift" => 0xe37933,
        "yml" | "yaml" | "toml" => 0xa074c4,
        "sh" | "zsh" | "bash" => 0x8dc149,
        "lock" => 0x8c8c8c,
        "svg" | "png" | "jpg" | "gif" | "webp" => 0xa074c4,
        _ => 0x8c8c8c,
    })
}

pub fn file_icon(name: &str) -> Svg {
    icon("file", file_color(name))
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
        .text_color(theme::fg_bright())
        .text_size(px(10.5))
        .child(text.into())
}

/// Collapsible section header (e.g. "CHANGES", "GRAPH").
pub fn section_header(id: impl Into<ElementId>, title: &str, open: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .h(px(22.))
        .pl(px(2.))
        .pr(px(6.))
        .gap(px(2.))
        .cursor_pointer()
        .font_weight(FontWeight::BOLD)
        .text_size(px(10.5))
        .child(small_icon(if open { "chevron-down" } else { "chevron-right" }, theme::fg()))
        .child(div().flex_1().child(title.to_uppercase()))
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
