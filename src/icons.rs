//! Material-style file and folder icons. GPUI paints SVGs in one color, so icons are
//! filled shapes with cut-outs (e.g. "TS" punched out of a blue square), tinted per type.

const PX: f32 = 1.12;

fn glyph(c: char) -> Option<[&'static str; 5]> {
    Some(match c {
        'A' => [".#.", "#.#", "###", "#.#", "#.#"],
        'B' => ["##.", "#.#", "##.", "#.#", "##."],
        'C' => ["###", "#..", "#..", "#..", "###"],
        'D' => ["##.", "#.#", "#.#", "#.#", "##."],
        'E' => ["###", "#..", "###", "#..", "###"],
        'G' => ["###", "#..", "#.#", "#.#", "###"],
        'H' => ["#.#", "#.#", "###", "#.#", "#.#"],
        'J' => ["..#", "..#", "..#", "#.#", "###"],
        'K' => ["#.#", "#.#", "##.", "#.#", "#.#"],
        'L' => ["#..", "#..", "#..", "#..", "###"],
        'M' => ["#.#", "###", "###", "#.#", "#.#"],
        'N' => ["##.", "#.#", "#.#", "#.#", "#.#"],
        'O' => ["###", "#.#", "#.#", "#.#", "###"],
        'P' => ["###", "#.#", "###", "#..", "#.."],
        'Q' => ["###", "#.#", "#.#", "###", "..#"],
        'R' => ["###", "#.#", "##.", "#.#", "#.#"],
        'S' => ["###", "#..", "###", "..#", "###"],
        'T' => ["###", ".#.", ".#.", ".#.", ".#."],
        'U' => ["#.#", "#.#", "#.#", "#.#", "###"],
        'V' => ["#.#", "#.#", "#.#", "#.#", ".#."],
        'W' => ["#.#", "#.#", "###", "###", "#.#"],
        'X' => ["#.#", "#.#", ".#.", "#.#", "#.#"],
        'Y' => ["#.#", "#.#", "###", ".#.", ".#."],
        '+' => ["...", ".#.", "###", ".#.", "..."],
        _ => return None,
    })
}

/// A rounded square with up to two letters punched out of its bottom-right corner.
fn badge(label: &str) -> String {
    let mut d = String::from("M4 1.5h8a2.5 2.5 0 0 1 2.5 2.5v8a2.5 2.5 0 0 1-2.5 2.5H4A2.5 2.5 0 0 1 1.5 12V4A2.5 2.5 0 0 1 4 1.5z");
    let letters: Vec<char> = label.chars().take(2).collect();
    let width = letters.len() as f32 * 3. * PX + (letters.len().saturating_sub(1)) as f32 * PX;
    let x0 = 13.4 - width;
    let y0 = 13.2 - 5. * PX;
    for (li, ch) in letters.iter().enumerate() {
        let Some(rows) = glyph(*ch) else { continue };
        let lx = x0 + li as f32 * 4. * PX;
        for (ry, row) in rows.iter().enumerate() {
            for (rx, cell) in row.chars().enumerate() {
                if cell == '#' {
                    let (x, y) = (lx + rx as f32 * PX, y0 + ry as f32 * PX);
                    d.push_str(&format!("M{x:.2} {y:.2}h{PX}v{PX}h-{PX}z"));
                }
            }
        }
    }
    format!(r#"<path fill="black" stroke="none" fill-rule="evenodd" d="{d}"/>"#)
}

const PAGE: &str = r#"<path fill="black" stroke="none" fill-rule="evenodd" d="M3.5 1h6.2l3.8 3.8V15h-10zM9.3 1.9v3.3h3.3"/>"#;

/// SVG body for an icon key ("ft-…" file types, "fd-…" folders).
pub fn svg(key: &str) -> Option<String> {
    let body = match key {
        "fd-closed" => r#"<path fill="black" stroke="none" d="M1.5 3.2c0-.6.4-1 1-1h3.7l1.6 1.7h5.7c.6 0 1 .4 1 1v7.9c0 .6-.4 1-1 1H2.5c-.6 0-1-.4-1-1z"/>"#.into(),
        "fd-open" => r#"<path fill="black" stroke="none" fill-rule="evenodd" d="M1.5 3.2c0-.6.4-1 1-1h3.7l1.6 1.7h5c.6 0 1 .4 1 1v1.4H4.4L2.3 13.4H2.5c-.6 0-1-.4-1-1zM4.9 7.3h10.3l-2.3 6.6H2.6z"/>"#.into(),
        "ft-react" => r#"<g fill="none" stroke="black" stroke-width="1.1"><ellipse cx="8" cy="8" rx="6.6" ry="2.5"/><ellipse cx="8" cy="8" rx="6.6" ry="2.5" transform="rotate(60 8 8)"/><ellipse cx="8" cy="8" rx="6.6" ry="2.5" transform="rotate(120 8 8)"/></g><circle cx="8" cy="8" r="1.4" fill="black" stroke="none"/>"#.into(),
        "ft-json" => r#"<path fill="none" stroke="black" stroke-width="1.5" stroke-linecap="round" d="M5.8 2.5C4.4 2.5 4 3.2 4 4.4v1.8C4 7.1 3.4 7.6 2.5 8c.9.4 1.5.9 1.5 1.8v1.8c0 1.2.4 1.9 1.8 1.9M10.2 2.5c1.4 0 1.8.7 1.8 1.9v1.8c0 .9.6 1.4 1.5 1.8-.9.4-1.5.9-1.5 1.8v1.8c0 1.2-.4 1.9-1.8 1.9"/>"#.into(),
        "ft-css" => r#"<path fill="none" stroke="black" stroke-width="1.6" stroke-linecap="round" d="M6.3 2.5l-1.5 11M11.3 2.5l-1.5 11M3 6h10.5M2.5 10h10.5"/>"#.into(),
        "ft-html" => r#"<path fill="none" stroke="black" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" d="M5.5 4L1.8 8l3.7 4M10.5 4l3.7 4-3.7 4"/>"#.into(),
        "ft-md" => r#"<path fill="black" stroke="none" fill-rule="evenodd" d="M2.5 3.5h11a1.5 1.5 0 0 1 1.5 1.5v6a1.5 1.5 0 0 1-1.5 1.5h-11A1.5 1.5 0 0 1 1 11V5a1.5 1.5 0 0 1 1.5-1.5zM3.3 10.6V5.4h1.4L6 7.1l1.3-1.7h1.4v5.2H7.3V7.4L6 9 4.7 7.4v3.2zM11 5.4h1.3v2.8h1.3L11.6 10.7 9.6 8.2H11z"/>"#.into(),
        "ft-rust" => r#"<g fill="none" stroke="black" stroke-width="1.7" stroke-linecap="round"><path d="M8 1.6v1.8M8 12.6v1.8M1.6 8h1.8M12.6 8h1.8M3.5 3.5l1.3 1.3M11.2 11.2l1.3 1.3M3.5 12.5l1.3-1.3M11.2 4.8l1.3-1.3"/><circle cx="8" cy="8" r="3.9"/></g><circle cx="8" cy="8" r="1.5" fill="black" stroke="none"/>"#.into(),
        "ft-shell" => r#"<path fill="black" stroke="none" fill-rule="evenodd" d="M2.5 2h11A1.5 1.5 0 0 1 15 3.5v9a1.5 1.5 0 0 1-1.5 1.5h-11A1.5 1.5 0 0 1 1 12.5v-9A1.5 1.5 0 0 1 2.5 2zM3.6 5.4l2.8 2.6-2.8 2.6.8.8 3.7-3.4-3.7-3.4zM8.6 10.4h4v1.1h-4z"/>"#.into(),
        "ft-image" => r#"<path fill="black" stroke="none" fill-rule="evenodd" d="M2.5 2.5h11a1 1 0 0 1 1 1v9a1 1 0 0 1-1 1h-11a1 1 0 0 1-1-1v-9a1 1 0 0 1 1-1zM2.8 11.8h10.4L9.9 7.3 7.4 10.4 5.8 8.6zM10.6 4a1.3 1.3 0 1 0 0 2.6 1.3 1.3 0 0 0 0-2.6z"/>"#.into(),
        "ft-git" => r#"<path fill="black" stroke="none" fill-rule="evenodd" d="M8 1l7 7-7 7-7-7zM6.2 4.6l1 1v3.1a1.2 1.2 0 1 0 1.2 0V6.9l1.3 1.3a1.2 1.2 0 1 0 .8-.8L8.4 5.7 7.1 4.4z"/>"#.into(),
        "ft-lock" => r#"<path fill="black" stroke="none" fill-rule="evenodd" d="M4.5 7V5a3.5 3.5 0 0 1 7 0v2h1a1 1 0 0 1 1 1v6a1 1 0 0 1-1 1h-9a1 1 0 0 1-1-1V8a1 1 0 0 1 1-1zM6 7h4V5a2 2 0 0 0-4 0zM7.3 9.5v2.5h1.4V9.5z"/>"#.into(),
        "ft-npm" => r#"<path fill="black" stroke="none" fill-rule="evenodd" d="M1.5 1.5h13v13h-13zM3.8 3.8v8.4h4.2V6h2.1v6.2h2.1V3.8z"/>"#.into(),
        "ft-gear" => r#"<path fill="black" stroke="none" fill-rule="evenodd" d="M6.9 1h2.2l.4 1.9 1.3.6 1.7-1 1.5 1.5-1 1.7.6 1.3 1.9.4v2.2l-1.9.4-.6 1.3 1 1.7-1.5 1.5-1.7-1-1.3.6-.4 1.9H6.9l-.4-1.9-1.3-.6-1.7 1-1.5-1.5 1-1.7-.6-1.3L1 9.1V6.9l1.9-.4.6-1.3-1-1.7 1.5-1.5 1.7 1 1.3-.6zM8 5.6a2.4 2.4 0 1 0 0 4.8 2.4 2.4 0 0 0 0-4.8z"/>"#.into(),
        "ft-info" => r#"<path fill="black" stroke="none" fill-rule="evenodd" d="M8 1a7 7 0 1 1 0 14A7 7 0 0 1 8 1zM7.2 7v5h1.6V7zM8 3.8a1 1 0 1 0 0 2 1 1 0 0 0 0-2z"/>"#.into(),
        "ft-lines" => r#"<path fill="black" stroke="none" fill-rule="evenodd" d="M3.5 1h6.2l3.8 3.8V15h-10zM5.3 7.2v1h5.4v-1zM5.3 9.4v1h5.4v-1zM5.3 11.6v1h3.6v-1z"/>"#.into(),
        "ft-db" => r#"<path fill="black" stroke="none" d="M8 1.5c3.3 0 5.5 1 5.5 2.2v8.6c0 1.2-2.2 2.2-5.5 2.2s-5.5-1-5.5-2.2V3.7C2.5 2.5 4.7 1.5 8 1.5z"/>"#.into(),
        "ft-file" => PAGE.into(),
        k if k.starts_with("ft-badge-") => badge(&k["ft-badge-".len()..]),
        _ => return None,
    };
    Some(body)
}

/// (icon key, color) for a file name.
pub fn file_icon(name: &str) -> (String, u32) {
    let lower = name.to_lowercase();
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    let b = |label: &str, color: u32| (format!("ft-badge-{label}"), color);
    let k = |key: &str, color: u32| (key.to_string(), color);
    match lower.as_str() {
        "package.json" | "package-lock.json" => return k("ft-npm", 0xe53935),
        "cargo.toml" | "cargo.lock" => return k("ft-rust", 0xff7043),
        "dockerfile" | "docker-compose.yml" | "compose.yaml" => return b("DK", 0x0288d1),
        "readme.md" | "readme" => return k("ft-info", 0x42a5f5),
        "license" | "license.md" | "licence" => return k("ft-lines", 0xef5350),
        "makefile" => return k("ft-gear", 0xef5350),
        ".gitignore" | ".gitattributes" | ".gitmodules" | ".gitkeep" => return k("ft-git", 0xe64a19),
        "yarn.lock" | "pnpm-lock.yaml" | "bun.lockb" => return k("ft-lock", 0xffd54f),
        "tsconfig.json" | "jsconfig.json" => return b("TS", 0x0288d1),
        _ if lower.starts_with(".env") => return k("ft-gear", 0xfbc02d),
        _ if lower.starts_with(".eslintrc") || lower.starts_with("eslint.config") => return b("ES", 0x7e57c2),
        _ if lower.starts_with(".prettierrc") || lower.starts_with("prettier.config") => return b("PR", 0x56b3b4),
        _ if lower.starts_with("biome.json") => return b("BI", 0x60a5fa),
        _ if lower.starts_with("vite.config") => return b("VI", 0xab47bc),
        _ if lower.starts_with("app.config") || lower.starts_with("babel.config") || lower.starts_with("metro.config") => return k("ft-gear", 0x90a4ae),
        _ => {}
    }
    if lower.ends_with(".d.ts") {
        return b("TS", 0x0277bd);
    }
    if lower.contains(".test.") || lower.contains(".spec.") {
        return b("TE", 0x00bfa5);
    }
    match ext {
        "ts" | "mts" | "cts" => b("TS", 0x0288d1),
        "tsx" => k("ft-react", 0x00bcd4),
        "jsx" => k("ft-react", 0x00bcd4),
        "js" | "mjs" | "cjs" => b("JS", 0xffca28),
        "json" | "jsonc" | "json5" => k("ft-json", 0xfbc02d),
        "css" => k("ft-css", 0x42a5f5),
        "scss" | "sass" => k("ft-css", 0xec407a),
        "less" => k("ft-css", 0x0277bd),
        "html" | "htm" => k("ft-html", 0xe44d26),
        "xml" | "plist" => k("ft-html", 0xff9800),
        "vue" => b("VU", 0x41b883),
        "svelte" => b("SV", 0xff3e00),
        "md" | "mdx" => k("ft-md", 0x42a5f5),
        "rs" => k("ft-rust", 0xff7043),
        "py" | "pyi" => b("PY", 0x3d7ab8),
        "go" => b("GO", 0x00add8),
        "rb" => b("RB", 0xe53935),
        "java" => b("JA", 0xf44336),
        "kt" | "kts" => b("KT", 0x7e57c2),
        "swift" => b("SW", 0xfe5e2f),
        "c" | "h" => b("C", 0x0277bd),
        "cpp" | "cc" | "hpp" | "cxx" => b("C+", 0x0277bd),
        "cs" => b("C+", 0x7cb342),
        "php" => b("PH", 0x7a86b8),
        "sh" | "bash" | "zsh" | "fish" => k("ft-shell", 0x4caf50),
        "yml" | "yaml" => b("YM", 0xff5252),
        "toml" => k("ft-gear", 0x9e9e9e),
        "ini" | "conf" | "cfg" => k("ft-gear", 0x90a4ae),
        "lock" => k("ft-lock", 0xffd54f),
        "svg" => k("ft-image", 0xffb300),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "ico" | "bmp" | "avif" => k("ft-image", 0x26a69a),
        "sql" | "db" | "sqlite" | "prisma" => k("ft-db", 0xffca28),
        "graphql" | "gql" => b("GQ", 0xe535ab),
        "txt" | "log" => k("ft-lines", 0x90a4ae),
        "csv" | "tsv" => k("ft-lines", 0x8bc34a),
        "pdf" => b("PD", 0xef5350),
        "zip" | "gz" | "tar" | "tgz" | "rar" | "7z" => k("ft-lock", 0xafb42b),
        _ => k("ft-file", 0x90a4ae),
    }
}

pub fn folder_color(name: &str) -> u32 {
    match name.to_lowercase().as_str() {
        "src" | "source" | "lib" => 0x4caf50,
        "app" | "apps" | "pages" | "routes" | "screens" => 0x42a5f5,
        "components" | "ui" | "widgets" => 0xffc107,
        "node_modules" => 0x8bc34a,
        ".git" | ".github" | ".gitlab" => 0xe64a19,
        "test" | "tests" | "__tests__" | "spec" | "e2e" => 0x00bfa5,
        "public" | "static" | "assets" | "images" | "img" | "icons" | "fonts" => 0xffa000,
        "dist" | "build" | "out" | "target" | ".next" | ".expo" => 0xe57373,
        "docs" | "doc" => 0x2196f3,
        "config" | "configs" | ".vscode" | ".idea" | ".claude" | "scripts" => 0x7986cb,
        "hooks" | "utils" | "helpers" | "services" | "api" | "server" => 0x26a69a,
        "styles" | "css" | "theme" => 0xec407a,
        "types" | "typings" | "@types" => 0x0288d1,
        "store" | "redux" | "state" | "context" => 0x7e57c2,
        _ => 0x90a4ae,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn badges_render_and_types_map() {
        assert!(super::svg("ft-badge-TS").unwrap().contains("evenodd"));
        assert_eq!(super::file_icon("app.tsx").0, "ft-react");
        assert_eq!(super::file_icon("package.json").0, "ft-npm");
        assert_eq!(super::file_icon("x.ts").0, "ft-badge-TS");
        assert_eq!(super::folder_color("src"), 0x4caf50);
    }
}

#[cfg(test)]
mod parse {
    use crate::assets::Assets;
    use gpui::AssetSource;

    /// Every icon must parse with the same SVG library GPUI renders with.
    #[test]
    fn all_icons_parse() {
        let names = [
            "fd-closed", "fd-open", "ft-react", "ft-json", "ft-css", "ft-html", "ft-md", "ft-rust", "ft-shell", "ft-image",
            "ft-git", "ft-lock", "ft-npm", "ft-gear", "ft-info", "ft-lines", "ft-db", "ft-file", "ft-badge-TS", "ft-badge-C+",
            "files", "git", "check", "filter", "error", "keyboard", "maximize", "chevron-left", "terminal",
        ];
        for n in names {
            let bytes = Assets.load(&format!("icons/{n}.svg")).unwrap().unwrap_or_else(|| panic!("{n} missing"));
            usvg::Tree::from_data(&bytes, &usvg::Options::default()).unwrap_or_else(|e| panic!("{n}: {e}"));
        }
    }
}
