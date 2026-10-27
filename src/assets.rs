//! Built-in SVG icons, served to GPUI from memory (no files on disk).

use anyhow::Result;
use gpui::{AssetSource, SharedString};
use std::borrow::Cow;

pub struct Assets;

const HEAD: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="black" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round">"#;

fn icon_body(name: &str) -> Option<&'static str> {
    Some(match name {
        "files" => r#"<path d="M5.5 1.5h5l3 3v8h-8z"/><path d="M10.5 1.5v3h3"/><path d="M3.5 4.5v10h7"/>"#,
        "git" => r#"<circle cx="4.5" cy="3.5" r="1.7"/><circle cx="4.5" cy="12.5" r="1.7"/><circle cx="11.5" cy="5.5" r="1.7"/><path d="M4.5 5.2v5.6"/><path d="M11.5 7.2c0 3-7 1.8-7 3.6"/>"#,
        "check" => r#"<path d="M3 8.5l3.2 3.2L13 4.8"/>"#,
        "refresh" => r#"<path d="M13 8a5 5 0 1 1-1.6-3.7"/><path d="M13 2.3v3h-3"/>"#,
        "plus" => r#"<path d="M8 3v10M3 8h10"/>"#,
        "minus" => r#"<path d="M3 8h10"/>"#,
        "discard" => r#"<path d="M5 2.5L2 5.5l3 3"/><path d="M2 5.5h7.5a3.5 3.5 0 0 1 0 7H6"/>"#,
        "open" => r#"<path d="M3.5 1.5h6l3 3v10h-9z"/><path d="M9.5 1.5v3h3"/><path d="M6 10l3-3M7 7h2v2"/>"#,
        "chevron-down" => r#"<path d="M4 6l4 4 4-4"/>"#,
        "chevron-left" => r#"<path d="M10 4L6 8l4 4"/>"#,
        "maximize" => r#"<path d="M2.5 6V2.5H6M10 2.5h3.5V6M13.5 10v3.5H10M6 13.5H2.5V10"/>"#,
        "chevron-right" => r#"<path d="M6 4l4 4-4 4"/>"#,
        "folder" => r#"<path d="M1.5 3.5h4.2l1.5 1.5h7.3v8h-13z"/>"#,
        "folder-open" => r#"<path d="M1.5 12.5v-9h4.2l1.5 1.5h6.3v2"/><path d="M1.5 12.5l2-5h11l-2 5z"/>"#,
        "file" => r#"<path d="M3.5 1.5h6l3 3v10h-9z"/><path d="M9.5 1.5v3h3"/>"#,
        "cloud" => r#"<path d="M4.5 12.5h7a2.8 2.8 0 0 0 .3-5.6 4 4 0 0 0-7.7-.9A3.3 3.3 0 0 0 4.5 12.5z"/>"#,
        "target" => r#"<circle cx="8" cy="8" r="5.5"/><circle cx="8" cy="8" r="2" fill="black"/>"#,
        "tag" => r#"<path d="M1.5 1.5h6l7 7-6 6-7-7z"/><circle cx="5" cy="5" r="1"/>"#,
        "sync" => r#"<path d="M5 13.5v-11M2.5 5L5 2.5 7.5 5"/><path d="M11 2.5v11M8.5 11l2.5 2.5 2.5-2.5"/>"#,
        "arrow-left" => r#"<path d="M13.5 8h-11M6.5 4l-4 4 4 4"/>"#,
        "arrow-right" => r#"<path d="M2.5 8h11M9.5 4l4 4-4 4"/>"#,
        "arrow-up" => r#"<path d="M8 13.5v-11M4 6.5l4-4 4 4"/>"#,
        "arrow-down" => r#"<path d="M8 2.5v11M4 9.5l4 4 4-4"/>"#,
        "fetch" => r#"<path d="M4.5 12.5h7a2.8 2.8 0 0 0 .3-5.6 4 4 0 0 0-7.7-.9A3.3 3.3 0 0 0 4.5 12.5z"/><path d="M8 7v4M6.5 9.5L8 11l1.5-1.5"/>"#,
        "ellipsis" => r#"<circle cx="3.5" cy="8" r="1" fill="black"/><circle cx="8" cy="8" r="1" fill="black"/><circle cx="12.5" cy="8" r="1" fill="black"/>"#,
        "close" => r#"<path d="M4 4l8 8M12 4l-8 8"/>"#,
        "dot" => r#"<circle cx="8" cy="8" r="3.5" fill="black"/>"#,
        "search" => r#"<circle cx="7" cy="7" r="4.5"/><path d="M10.5 10.5L14 14"/>"#,
        "blame" => r#"<circle cx="8" cy="8" r="6"/><path d="M8 4.5V8l2.5 1.5"/>"#,
        "wrap" => r#"<path d="M2.5 4h11"/><path d="M2.5 8h9a2 2 0 0 1 0 4H8"/><path d="M9.5 10.5L8 12l1.5 1.5"/><path d="M2.5 12h3"/>"#,
        "terminal" => r#"<path d="M2.5 4l4 4-4 4"/><path d="M8 12h5.5"/>"#,
        "split" => r#"<rect x="1.5" y="2.5" width="13" height="11" rx="1"/><path d="M8 2.5v11"/>"#,
        "inline" => r#"<rect x="1.5" y="2.5" width="13" height="11" rx="1"/><path d="M1.5 8h13"/>"#,
        "copy" => r#"<rect x="5.5" y="5.5" width="8" height="8" rx="1"/><path d="M3.5 10.5h-1v-8h8v1"/>"#,
        "commit" => r#"<circle cx="8" cy="8" r="2.8"/><path d="M1 8h4.2M10.8 8H15"/>"#,
        "keyboard" => r#"<rect x="1.5" y="4" width="13" height="8.5" rx="1.2"/><path d="M4 6.6h.01M6.5 6.6h.01M9 6.6h.01M11.5 6.6h.01M4 9h.01M11.5 9h.01M6 10.3h4"/>"#,
        "error" => r#"<circle cx="8" cy="8" r="6"/><path d="M5.8 5.8l4.4 4.4M10.2 5.8l-4.4 4.4"/>"#,
        "filter" => r#"<path d="M2 3h12l-4.5 5.5v4l-3 1.5v-5.5z"/>"#,
        "warning" => r#"<path d="M8 2l6.5 11.5h-13z"/><path d="M8 6.5v3.5M8 12v.01"/>"#,
        _ => return None,
    })
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let name = path
            .strip_prefix("icons/")
            .and_then(|p| p.strip_suffix(".svg"))
            .unwrap_or(path);
        let body = icon_body(name).map(String::from).or_else(|| crate::icons::svg(name));
        Ok(body.map(|body| Cow::Owned(format!("{HEAD}{body}</svg>").into_bytes())))
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}

pub fn icon_path(name: &str) -> SharedString {
    format!("icons/{name}.svg").into()
}
