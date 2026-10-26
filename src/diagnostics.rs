//! Problems from linters (ESLint / Biome) and the TypeScript compiler, parsed into `Diag`s.
//! Each tool runs as a short-lived process, so nothing heavy stays in memory.

use crate::editor::{Diag, Severity};
use crate::tools;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub type FileDiags = HashMap<PathBuf, Vec<Diag>>;

fn is_web(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).unwrap_or(""),
        "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "mts" | "cts" | "vue" | "svelte"
    )
}

pub fn is_typescript(path: &Path) -> bool {
    matches!(path.extension().and_then(|e| e.to_str()).unwrap_or(""), "ts" | "tsx" | "mts" | "cts")
}

/// Lints one file. Returns its diagnostics (possibly empty) or None when no linter applies.
pub fn lint_file(root: &Path, file: &Path) -> Option<(String, Vec<Diag>)> {
    if !is_web(file) {
        return None;
    }
    let dir = file.parent().unwrap_or(root);
    if let Some(eslint) = tools::find_eslint(dir, root) {
        let out = tools::run_tool(&eslint, &["-f", "json", &file.to_string_lossy()], root);
        let diags = parse_eslint(&out).remove(file).unwrap_or_default();
        return Some(("eslint".into(), diags));
    }
    if let Some(biome) = tools::find_named("biome", dir, root) {
        let out = tools::run_tool(&biome, &["lint", "--reporter=github", "--colors=off", &file.to_string_lossy()], root);
        let diags = parse_github(&out, root).remove(file).unwrap_or_default();
        return Some(("biome".into(), diags));
    }
    None
}

/// Type-checks the TypeScript project that contains `file`.
pub fn typecheck(root: &Path, file: &Path) -> Option<FileDiags> {
    let mut dir = file.parent();
    let project = loop {
        let d = dir?;
        if d.join("tsconfig.json").exists() {
            break d.to_path_buf();
        }
        if d == root {
            return None;
        }
        dir = d.parent();
    };
    let tsc = tools::find_named("tsc", &project, root)?;
    // Incremental info lives in the temp dir so repeated checks are fast.
    let info = std::env::temp_dir().join(format!("kite-{:x}.tsbuildinfo", hash(&project.to_string_lossy())));
    let out = tools::run_tool(
        &tsc,
        &["--noEmit", "--pretty", "false", "--incremental", "--tsBuildInfoFile", &info.to_string_lossy(), "-p", &project.to_string_lossy()],
        &project,
    );
    Some(parse_tsc(&out, &project))
}

fn hash(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// `src/a.ts(12,5): error TS2322: Type 'x' is not assignable…` (+ indented continuation lines)
pub fn parse_tsc(out: &str, cwd: &Path) -> FileDiags {
    let mut map: FileDiags = HashMap::new();
    let mut last: Option<(PathBuf, usize)> = None;
    for line in out.lines() {
        if line.starts_with(' ') {
            if let Some((path, ix)) = &last
                && let Some(d) = map.get_mut(path).and_then(|v| v.get_mut(*ix))
            {
                d.message.push('\n');
                d.message.push_str(line.trim());
            }
            continue;
        }
        let Some((loc, rest)) = line.split_once("): ") else { continue };
        let Some((file, pos)) = loc.rsplit_once('(') else { continue };
        let mut nums = pos.split(',').map(|n| n.trim().parse::<usize>().unwrap_or(1));
        let (row, col) = (nums.next().unwrap_or(1).saturating_sub(1), nums.next().unwrap_or(1).saturating_sub(1));
        let (kind, msg) = rest.split_once(": ").unwrap_or((rest, ""));
        let severity = if kind.starts_with("error") { Severity::Error } else if kind.starts_with("warning") { Severity::Warning } else { Severity::Info };
        let code = kind.split_whitespace().nth(1).unwrap_or("");
        let path = absolutize(cwd, file);
        let list = map.entry(path.clone()).or_default();
        list.push(Diag { row, col, end_row: row, end_col: col, severity, message: format!("{msg} [{code}]"), source: "ts".into() });
        last = Some((path, list.len() - 1));
    }
    map
}

pub fn parse_eslint(out: &str) -> FileDiags {
    let mut map: FileDiags = HashMap::new();
    let Ok(serde_json::Value::Array(files)) = serde_json::from_str::<serde_json::Value>(out.trim()) else { return map };
    for f in files {
        let Some(path) = f.get("filePath").and_then(|p| p.as_str()) else { continue };
        let list = map.entry(PathBuf::from(path)).or_default();
        for m in f.get("messages").and_then(|m| m.as_array()).into_iter().flatten() {
            let num = |k: &str| m.get(k).and_then(|v| v.as_u64()).map(|v| v as usize);
            let row = num("line").unwrap_or(1).saturating_sub(1);
            let col = num("column").unwrap_or(1).saturating_sub(1);
            let end_row = num("endLine").map(|v| v.saturating_sub(1)).unwrap_or(row);
            let end_col = num("endColumn").map(|v| v.saturating_sub(1)).unwrap_or(col);
            let severity = if num("severity") == Some(2) { Severity::Error } else { Severity::Warning };
            let rule = m.get("ruleId").and_then(|r| r.as_str()).unwrap_or("");
            let msg = m.get("message").and_then(|r| r.as_str()).unwrap_or("");
            list.push(Diag { row, col, end_row, end_col, severity, message: if rule.is_empty() { msg.to_string() } else { format!("{msg} [{rule}]") }, source: "eslint".into() });
        }
    }
    map
}

/// GitHub annotation format: `::error title=…,file=…,line=1,endLine=1,col=2,endColumn=5::message`
pub fn parse_github(out: &str, cwd: &Path) -> FileDiags {
    let mut map: FileDiags = HashMap::new();
    for line in out.lines() {
        let Some(rest) = line.strip_prefix("::") else { continue };
        let Some((head, msg)) = rest.split_once("::") else { continue };
        let (kind, props) = head.split_once(' ').unwrap_or((head, ""));
        let mut get = HashMap::new();
        for kv in props.split(',') {
            if let Some((k, v)) = kv.split_once('=') {
                get.insert(k.trim(), v.trim());
            }
        }
        let Some(file) = get.get("file") else { continue };
        let num = |k: &str| get.get(k).and_then(|v| v.parse::<usize>().ok());
        let row = num("line").unwrap_or(1).saturating_sub(1);
        let col = num("col").unwrap_or(1).saturating_sub(1);
        let severity = match kind {
            "error" => Severity::Error,
            "warning" => Severity::Warning,
            _ => Severity::Info,
        };
        let title = get.get("title").copied().unwrap_or("");
        map.entry(absolutize(cwd, file)).or_default().push(Diag {
            row,
            col,
            end_row: num("endLine").map(|v| v.saturating_sub(1)).unwrap_or(row),
            end_col: num("endColumn").map(|v| v.saturating_sub(1)).unwrap_or(col),
            severity,
            message: if title.is_empty() { msg.to_string() } else { format!("{msg} [{title}]") },
            source: "biome".into(),
        });
    }
    map
}

fn absolutize(cwd: &Path, file: &str) -> PathBuf {
    let p = Path::new(file.trim());
    let p = if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) };
    p.canonicalize().unwrap_or(p)
}

#[cfg(test)]
mod tests {
    use super::{parse_eslint, parse_github, parse_tsc};
    use crate::editor::Severity;
    use std::path::Path;

    #[test]
    fn parses_tsc_output() {
        let out = "src/a.ts(3,7): error TS2322: Type 'string' is not assignable to type 'number'.\n  Extra detail.\nsrc/b.ts(1,1): error TS1005: ';' expected.\n";
        let map = parse_tsc(out, Path::new("/proj"));
        let a = &map[&Path::new("/proj/src/a.ts").to_path_buf()];
        assert_eq!((a[0].row, a[0].col), (2, 6));
        assert_eq!(a[0].severity, Severity::Error);
        assert!(a[0].message.contains("TS2322") && a[0].message.contains("Extra detail"));
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn parses_eslint_json() {
        let out = r#"[{"filePath":"/p/a.ts","messages":[{"ruleId":"no-unused-vars","severity":2,"message":"'x' is unused","line":2,"column":7,"endLine":2,"endColumn":8}]}]"#;
        let map = parse_eslint(out);
        let d = &map[&Path::new("/p/a.ts").to_path_buf()][0];
        assert_eq!((d.row, d.col, d.end_col), (1, 6, 7));
        assert!(d.message.contains("no-unused-vars"));
    }

    #[test]
    fn parses_github_annotations() {
        let out = "::error title=lint/style/useConst,file=/p/a.ts,line=4,endLine=4,col=1,endColumn=4::This let can be const\n";
        let map = parse_github(out, Path::new("/p"));
        let d = &map[&Path::new("/p/a.ts").to_path_buf()][0];
        assert_eq!((d.row, d.col, d.end_col), (3, 0, 3));
    }
}

#[cfg(test)]
mod live {
    /// Runs real tsc/biome against a project given in KITE_TS_PROJECT (skipped otherwise).
    #[test]
    fn real_tools() {
        let Ok(dir) = std::env::var("KITE_TS_PROJECT") else { return };
        let root = std::path::PathBuf::from(dir).canonicalize().unwrap();
        let file = root.join("src/app.ts");
        let ts = super::typecheck(&root, &file).expect("tsc ran");
        let diags = &ts[&file];
        println!("tsc: {diags:#?}");
        assert!(diags.len() >= 2);
        let (tool, lint) = super::lint_file(&root, &file).expect("a linter ran");
        println!("{tool}: {lint:#?}");
        assert!(!lint.is_empty());
    }
}
