//! Built-in SVG icons, served to GPUI from memory (no files on disk).

use anyhow::Result;
use gpui::{AssetSource, SharedString};
use std::borrow::Cow;

pub struct Assets;

const HEAD: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="black" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round">"#;

fn icon_body(name: &str) -> Option<&'static str> {
    Some(match name {
        // Slonik, PostgreSQL's elephant, face on: the ears either side of the head, the eyes,
        // the trunk down the middle. One color: the ears' edges and the eyes are cut out.
        "db-postgres" => r#"<mask id="pg"><rect width="16" height="16" fill="white" stroke="none"/><g stroke="black" stroke-width="0.85" fill="none" stroke-linecap="round"><path d="M5.3 3.1C4.1 5 4 8 5.5 10.7"/><path d="M10.7 3.1C11.9 5 12 8 10.5 10.7"/></g><circle cx="6.4" cy="6.3" r="0.7" fill="black" stroke="none"/><circle cx="9.6" cy="6.3" r="0.7" fill="black" stroke="none"/></mask><g mask="url(#pg)" fill="black" stroke="none"><path d="M6.6 2.4C3.9 1.5 1.1 2.3 0.9 5 0.7 7.7 1.9 10.5 3.4 11.8 4.3 12.5 5.2 11.9 5.4 10.8L5.8 4Z"/><path d="M9.4 2.4C12.1 1.5 14.9 2.3 15.1 5 15.3 7.7 14.1 10.5 12.6 11.8 11.7 12.5 10.8 11.9 10.6 10.8L10.2 4Z"/><path d="M8 1.7C11 1.7 12.5 3.9 12.3 6.8 12.1 9.1 11 10.6 9.7 11.3L9.5 14.3C9.4 15.3 6.6 15.3 6.5 14.3L6.3 11.3C5 10.6 3.9 9.1 3.7 6.8 3.5 3.9 5 1.7 8 1.7Z"/></g>"#,
        "db-sqlserver" => r#"<path fill="black" stroke="none" fill-rule="evenodd" d="M8 1.5c3 0 5.5.9 5.5 2.2v8.6c0 1.3-2.5 2.2-5.5 2.2s-5.5-.9-5.5-2.2V3.7C2.5 2.4 5 1.5 8 1.5zM2.5 6c.9 1 3 1.6 5.5 1.6s4.6-.6 5.5-1.6v1.1c-.9 1-3 1.6-5.5 1.6S3.4 8.1 2.5 7.1zM2.5 9.4c.9 1 3 1.6 5.5 1.6s4.6-.6 5.5-1.6v1.1c-.9 1-3 1.6-5.5 1.6s-4.6-.6-5.5-1.6z"/>"#,
        "db-mongodb" => r#"<path fill="black" stroke="none" fill-rule="evenodd" d="M8 .6c.4 1.3 1.3 2.1 2.3 3.2 1.4 1.5 2.1 3.3 1.9 5.3-.2 2.4-1.7 4.1-3.3 4.9-.3.2-.5.5-.5.9V16h-.8v-1.1c0-.4-.2-.7-.5-.9C5.5 13.2 4 11.5 3.8 9.1 3.6 7.1 4.3 5.3 5.7 3.8 6.7 2.7 7.6 1.9 8 .6zM7.75 4.2h.5v9.2h-.5z"/>"#,
        "db-redis" => r#"<path fill="black" stroke="none" fill-rule="evenodd" d="M8 1.6l6 2.5L8 6.6 2 4.1zM2 6.6l6 2.5 6-2.5v1.7L8 10.8 2 8.3zM2 10.4l6 2.5 6-2.5v1.7L8 14.6 2 12.1z"/>"#,
        "db-rabbitmq" => r#"<path fill="black" stroke="none" fill-rule="evenodd" d="M2.5 1.5h2.7v5.2h1.5V1.5h2.7v5.2h3.1c.8 0 1.5.7 1.5 1.5v5.3c0 .8-.7 1.5-1.5 1.5H4c-.8 0-1.5-.7-1.5-1.5zM10.3 9.2h2.1v2.1h-2.1z"/>"#,
        // The API section: "API" in a rounded badge.
        "api" => r#"<rect x="0.9" y="2.6" width="14.2" height="10.8" rx="2.4"/><path d="M3.2 10.6l1.7-5.2 1.7 5.2"/><path d="M3.8 9h2.2"/><path d="M8.2 10.6V5.4h1.3a1.4 1.4 0 0 1 0 2.8H8.2"/><path d="M12.6 5.4v5.2"/>"#,
        "play" => r#"<path d="M4.5 2.8v10.4L13 8z"/>"#,
        "clock" => r#"<circle cx="8" cy="8" r="6"/><path d="M8 4.5V8l2.5 1.5"/>"#,
        "globe" => r#"<circle cx="8" cy="8" r="6"/><path d="M2 8h12"/><path d="M8 2c1.7 1.7 2.5 3.7 2.5 6S9.7 12.3 8 14c-1.7-1.7-2.5-3.7-2.5-6S6.3 3.7 8 2z"/>"#,
        "cookie" => r#"<path d="M13.8 8.6A5.9 5.9 0 1 1 7.4 2.2a2 2 0 0 0 2.4 2.4 2 2 0 0 0 2.4 2.4 2 2 0 0 0 1.6 1.6z"/><circle cx="5.6" cy="7" r=".5"/><circle cx="8" cy="10.5" r=".5"/><circle cx="5.5" cy="10.8" r=".4"/>"#,
        "lock" => r#"<rect x="3" y="7" width="10" height="7" rx="1.2"/><path d="M5.2 7V5a2.8 2.8 0 0 1 5.6 0v2"/>"#,
        "stop" => r#"<rect x="3.5" y="3.5" width="9" height="9" rx="1.2"/>"#,
        "braces" => r#"<path d="M5.5 2.5c-1.5 0-2 .7-2 2v1.6c0 .9-.5 1.4-1.5 1.9 1 .5 1.5 1 1.5 1.9v1.6c0 1.3.5 2 2 2"/><path d="M10.5 2.5c1.5 0 2 .7 2 2v1.6c0 .9.5 1.4 1.5 1.9-1 .5-1.5 1-1.5 1.9v1.6c0 1.3-.5 2-2 2"/>"#,
        "trash" => r#"<path d="M2.5 4h11"/><path d="M6 4V2.5h4V4"/><path d="M3.8 4l.7 9.5h7l.7-9.5"/><path d="M6.5 6.5v4.5M9.5 6.5v4.5"/>"#,
        "database" => r#"<ellipse cx="8" cy="3.8" rx="5" ry="2"/><path d="M3 3.8v8.4c0 1.1 2.2 2 5 2s5-.9 5-2V3.8"/><path d="M3 8c0 1.1 2.2 2 5 2s5-.9 5-2"/>"#,
        "table" => r#"<rect x="2" y="2.5" width="12" height="11" rx="1"/><path d="M2 6h12M2 9.5h12M6.5 6v7.5"/>"#,
        "collection" => r#"<path d="M4.5 2.5h5l2 2v9h-7z"/><path d="M2.5 4.5v10h7"/><path d="M6.5 7h3M6.5 9.5h3"/>"#,
        "key" => r#"<circle cx="5" cy="10.5" r="2.7"/><path d="M7 8.5l6-6M11 4.5l1.5 1.5M9.5 6l1.2 1.2"/>"#,
        "queue" => r#"<rect x="1.5" y="5" width="3.2" height="6" rx=".6"/><rect x="6.4" y="5" width="3.2" height="6" rx=".6"/><path d="M11.5 8h3M13 6.5L14.5 8 13 9.5"/>"#,
        "exchange" => r#"<path d="M2.5 5h9M9.5 3l2 2-2 2"/><path d="M13.5 11h-9M6.5 9l-2 2 2 2"/>"#,
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
        "new-file" => r#"<path d="M9.5 1.5h-6v13h5"/><path d="M9.5 1.5l3 3v4"/><path d="M9.5 1.5v3h3"/><path d="M12 10.5v4M10 12.5h4"/>"#,
        "new-folder" => r#"<path d="M8.5 13.5h-7v-10h4.2l1.5 1.5h7.3v3.5"/><path d="M12 9.5v4.5M9.75 11.75h4.5"/>"#,
        "collapse-all" => r#"<rect x="2.5" y="2.5" width="11" height="11" rx="1.5"/><path d="M5.5 8h5"/>"#,
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
        "repo" => r#"<path d="M12.5 11.5h-8a1.5 1.5 0 0 0 0 3h8v-13h-8a1.5 1.5 0 0 0-1.5 1.5v10"/><path d="M6 4.5h4"/>"#,
        "eye" => r#"<path d="M1.5 8s2.5-4.5 6.5-4.5S14.5 8 14.5 8s-2.5 4.5-6.5 4.5S1.5 8 1.5 8z"/><circle cx="8" cy="8" r="2"/>"#,
        "go-to-file" => r#"<path d="M7.5 14.5h-4v-13h6l3 3v2.5"/><path d="M9.5 1.5v3h3"/><circle cx="10.5" cy="10.5" r="2.5"/><path d="M12.3 12.3l2.2 2.2"/>"#,
        "blame" => r#"<circle cx="8" cy="8" r="6"/><path d="M8 4.5V8l2.5 1.5"/>"#,
        "wrap" => r#"<path d="M2.5 4h11"/><path d="M2.5 8h9a2 2 0 0 1 0 4H8"/><path d="M9.5 10.5L8 12l1.5 1.5"/><path d="M2.5 12h3"/>"#,
        "terminal" => r#"<path d="M2.5 4l4 4-4 4"/><path d="M8 12h5.5"/>"#,
        "split" => r#"<rect x="1.5" y="2.5" width="13" height="11" rx="1"/><path d="M8 2.5v11"/>"#,
        "inline" => r#"<rect x="1.5" y="2.5" width="13" height="11" rx="1"/><path d="M1.5 8h13"/>"#,
        "copy" => r#"<rect x="5.5" y="5.5" width="8" height="8" rx="1"/><path d="M3.5 10.5h-1v-8h8v1"/>"#,
        "commit" => r#"<circle cx="8" cy="8" r="2.8"/><path d="M1 8h4.2M10.8 8H15"/>"#,
        "settings" => r#"<circle cx="8" cy="8" r="2.2"/><path d="M8 1.5v2M8 12.5v2M1.5 8h2M12.5 8h2M3.4 3.4l1.4 1.4M11.2 11.2l1.4 1.4M3.4 12.6l1.4-1.4M11.2 4.8l1.4-1.4"/><circle cx="8" cy="8" r="4.6"/>"#,
        "keyboard" => r#"<rect x="1.5" y="4" width="13" height="8.5" rx="1.2"/><path d="M4 6.6h.01M6.5 6.6h.01M9 6.6h.01M11.5 6.6h.01M4 9h.01M11.5 9h.01M6 10.3h4"/>"#,
        "error" => r#"<circle cx="8" cy="8" r="6"/><path d="M5.8 5.8l4.4 4.4M10.2 5.8l-4.4 4.4"/>"#,
        "list-tree" => r#"<path d="M2.5 3h4M5 6.5h4M5 10h4M5 13.5h4M3.5 3v10.5M3.5 6.5H5M3.5 10H5M3.5 13.5H5"/><path d="M10.5 6.5h3M10.5 10h3M10.5 13.5h3"/>"#,
        "list-flat" => r#"<path d="M2.5 4h11M2.5 8h11M2.5 12h11"/>"#,
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

#[cfg(test)]
mod tests {
    use gpui::AssetSource;

    #[test]
    fn every_database_type_has_its_icon() {
        for k in crate::db::Kind::ALL {
            let svg = super::Assets.load(&super::icon_path(&format!("db-{}", k.id()))).unwrap().unwrap_or_else(|| panic!("{k:?} has no icon"));
            let tree = usvg::Tree::from_data(&svg, &usvg::Options::default()).unwrap_or_else(|e| panic!("{k:?}: {e}"));
            assert!(tree.root().has_children(), "{k:?} draws something");
        }
    }
}
