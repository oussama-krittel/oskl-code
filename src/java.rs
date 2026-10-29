//! Java without a language server: syntax errors as you type (tree-sitter), compile errors
//! on save (`javac`, a short-lived process), and go to definition through `package` /
//! `import` lines, into the JDK's own sources (`src.zip`) and downloaded `-sources.jar`s.
//! Archives are never loaded: one file at a time is unpacked into a cache folder.

use crate::definition::Target;
use crate::editor::{Diag, Severity};
use regex::Regex;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

fn re(s: &str) -> Regex {
    Regex::new(s).expect("valid regex")
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

// ---------------------------------------------------------------- problems

/// Syntax errors, straight from the parser: cheap enough to run on every keystroke.
pub fn check_syntax(text: &str) -> Vec<Diag> {
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&tree_sitter_java::LANGUAGE.into()).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(text, None) else { return Vec::new() };
    let lines: Vec<&str> = text.split('\n').collect();
    let col = |p: tree_sitter::Point| lines.get(p.row).map(|l| l.get(..p.column.min(l.len())).map(|s| s.chars().count()).unwrap_or(p.column)).unwrap_or(0);
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if out.len() >= 20 {
            break;
        }
        let (start, end) = (node.start_position(), node.end_position());
        if node.is_missing() {
            let what = match node.kind() {
                ";" => "`;`".to_string(),
                "}" => "`}` (a block isn't closed)".to_string(),
                ")" => "`)`".to_string(),
                "identifier" => "a name".to_string(),
                k => format!("`{k}`"),
            };
            out.push(Diag { row: start.row, col: col(start), end_row: start.row, end_col: col(start), severity: Severity::Error, message: format!("Java: missing {what}"), source: "java".into() });
            continue;
        }
        if node.is_error() {
            let snippet: String = text[node.byte_range()].lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").chars().take(30).collect();
            let end_col = if end.row == start.row { col(end) } else { lines.get(start.row).map(|l| l.chars().count()).unwrap_or(0) };
            let message = if snippet.is_empty() { "Java: syntax error".to_string() } else { format!("Java: unexpected `{snippet}`") };
            out.push(Diag { row: start.row, col: col(start), end_row: start.row, end_col, severity: Severity::Error, message, source: "java".into() });
            continue;
        }
        if node.has_error() {
            let mut w = node.walk();
            let kids: Vec<_> = node.children(&mut w).collect();
            stack.extend(kids.into_iter().rev());
        }
    }
    out.sort_by_key(|d| (d.row, d.col));
    out
}

/// The `package a.b;` of a file.
fn package_of(text: &str) -> Option<String> {
    re(r"(?m)^\s*package\s+([\w.]+)\s*;").captures(text).map(|c| c[1].to_string())
}

/// The folder a file's packages start from: `src/main/java` for `src/main/java/a/b/C.java`
/// in `package a.b`.
fn source_root(file: &Path, text: &str) -> Option<PathBuf> {
    let mut dir = file.parent()?.to_path_buf();
    if let Some(pkg) = package_of(text) {
        for part in pkg.split('.').rev() {
            if dir.file_name()? != part {
                return file.parent().map(Path::to_path_buf);
            }
            dir = dir.parent()?.to_path_buf();
        }
    }
    Some(dir)
}

/// Maven / Gradle: dependencies javac can't see, so "cannot find symbol" isn't trusted there.
fn build_file(file: &Path, root: &Path) -> bool {
    file.ancestors().skip(1).take_while(|d| d.starts_with(root) || *d == root).any(|d| {
        ["pom.xml", "build.gradle", "build.gradle.kts", "settings.gradle", "settings.gradle.kts"].iter().any(|f| d.join(f).is_file())
    })
}

/// Compile errors and warnings for one saved file, from `javac` (checked, nothing kept).
pub fn javac_check(root: &Path, file: &Path) -> Result<(String, Vec<Diag>), String> {
    let text = std::fs::read_to_string(file).map_err(|e| e.to_string())?;
    // The parser already shows syntax errors; javac would only repeat them.
    if !check_syntax(&text).is_empty() {
        return Ok(("javac".into(), Vec::new()));
    }
    let Some(javac) = javac() else { return Err("javac not found: install a JDK to see Java compile errors".into()) };
    let src = source_root(file, &text).unwrap_or_else(|| root.to_path_buf());
    let mut sourcepath = vec![src.clone()];
    sourcepath.extend(source_roots(root).into_iter().filter(|r| *r != src));
    let mut classpath: Vec<PathBuf> = ["target/classes", "build/classes/java/main", "out/production", "bin"].iter().map(|p| root.join(p)).filter(|p| p.is_dir()).collect();
    if let Ok(rd) = std::fs::read_dir(root.join("lib")) {
        classpath.extend(rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "jar")));
    }
    let out_dir = std::env::temp_dir().join(format!("kite-javac-{}", std::process::id()));
    let join = |v: &[PathBuf]| v.iter().map(|p| p.to_string_lossy().to_string()).collect::<Vec<_>>().join(":");
    let mut c = Command::new(&javac);
    // A small, quick JVM: this runs on every save.
    c.args(["-J-XX:+UseSerialGC", "-J-XX:TieredStopAtLevel=1", "-J-Xmx384m", "-J-Duser.language=en"]);
    c.args(["-proc:none", "-implicit:none", "-encoding", "UTF-8", "-Xlint:all", "-Xlint:-serial", "-Xlint:-processing", "-Xmaxerrs", "200"]);
    c.arg("-d").arg(&out_dir).arg("-sourcepath").arg(join(&sourcepath));
    if !classpath.is_empty() {
        c.arg("-cp").arg(join(&classpath));
    }
    c.arg(file).current_dir(root);
    let output = c.output().map_err(|e| format!("javac failed: {e}"))?;
    let _ = std::fs::remove_dir_all(&out_dir);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut diags = parse_javac(&stderr, file, root);
    if build_file(file, root) {
        // Libraries come from Maven/Gradle, which javac alone doesn't know about.
        diags.retain(|d| !is_missing_dependency(&d.message));
    }
    if !output.status.success() && diags.is_empty() && !stderr.contains("error") {
        let line = stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("exited with an error");
        return Err(format!("javac failed: {line}"));
    }
    Ok(("javac".into(), diags))
}

fn is_missing_dependency(msg: &str) -> bool {
    let m = msg.to_lowercase();
    m.starts_with("cannot find symbol") || (m.starts_with("package ") && m.contains("does not exist")) || m.starts_with("cannot access") || m.contains("method does not override")
}

/// `Foo.java:12: error: ';' expected` + the source line + a caret line (+ `symbol:` details).
pub fn parse_javac(out: &str, file: &Path, cwd: &Path) -> Vec<Diag> {
    let head = re(r"^(.+\.java):(\d+): (error|warning): (.*)$");
    let want = file.canonicalize().unwrap_or(file.to_path_buf());
    // (whether it's this file's, the problem); javac also reports the files it read along the way.
    let mut found: Vec<(bool, Diag)> = Vec::new();
    let mut saw_source = false;
    for line in out.lines() {
        if let Some(c) = head.captures(line) {
            let p = Path::new(&c[1]);
            let p = if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) };
            let ours = p.canonicalize().unwrap_or(p) == want;
            let row = c[2].parse::<usize>().unwrap_or(1).saturating_sub(1);
            let severity = if &c[3] == "error" { Severity::Error } else { Severity::Warning };
            found.push((ours, Diag { row, col: 0, end_row: row, end_col: 0, severity, message: c[4].to_string(), source: "javac".into() }));
            saw_source = false;
            continue;
        }
        let Some((_, d)) = found.last_mut() else { continue };
        let t = line.trim();
        if !t.is_empty() && t.chars().all(|c| c == '^') {
            d.col = line.chars().take_while(|c| *c != '^').count();
            d.end_col = d.col;
        } else if !saw_source {
            saw_source = true;
        } else if let Some((label, detail)) = t.strip_prefix("symbol:").map(|r| ("symbol", r)).or_else(|| t.strip_prefix("location:").map(|r| ("location", r))) {
            d.message.push_str(&format!("\n  {label}: {}", detail.trim()));
        }
    }
    found.into_iter().filter(|(ours, _)| *ours).map(|(_, d)| d).collect()
}

fn javac() -> Option<PathBuf> {
    if let Some(home) = java_home() {
        let p = home.join("bin/javac");
        if p.is_file() {
            return Some(p);
        }
    }
    crate::tools::find_named("javac", Path::new("."), Path::new("."))
}

/// The JDK in use: `$JAVA_HOME`, else what macOS's `java_home` says.
fn java_home() -> Option<&'static PathBuf> {
    static HOME: OnceLock<Option<PathBuf>> = OnceLock::new();
    HOME.get_or_init(|| {
        if let Some(h) = std::env::var_os("JAVA_HOME").map(PathBuf::from).filter(|h| h.join("bin").is_dir()) {
            return Some(h);
        }
        let out = Command::new("/usr/libexec/java_home").output().ok()?;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (out.status.success() && !s.is_empty()).then(|| PathBuf::from(s))
    })
    .as_ref()
}

/// The newest JDK that's 21 or later, if any.
fn modern_java_home() -> Option<PathBuf> {
    let out = Command::new("/usr/libexec/java_home").args(["-v", "21+"]).output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !s.is_empty()).then(|| PathBuf::from(s))
}

/// Formats with google-java-format, else clang-format (which knows Java).
pub fn format_command(path: &Path, dir: &Path, root: &Path) -> Result<(Command, &'static str), String> {
    if let Some(bin) = crate::tools::find_named("google-java-format", dir, root) {
        let mut c = crate::tools::command(&bin);
        c.arg("-");
        // It needs Java 21+, whatever JAVA_HOME the shell has.
        if let Some(home) = modern_java_home() {
            c.env("JAVA_HOME", home);
        }
        return Ok((c, "google-java-format"));
    }
    if let Some(bin) = crate::tools::find_named("clang-format", dir, root) {
        let mut c = crate::tools::command(&bin);
        c.arg(format!("--assume-filename={}", path.to_string_lossy()));
        return Ok((c, "clang-format"));
    }
    Err("No Java formatter found. Install one: brew install google-java-format".into())
}

// ---------------------------------------------------------------- go to definition

/// The project's Java source folders: `src/main/java`, `src/test/java`, `src` with packages…
fn source_roots(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        if out.len() > 50 {
            break;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            if !e.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || matches!(name.as_str(), "node_modules" | "target" | "build" | "out" | "bin") {
                continue;
            }
            let parent = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            if name == "java" && matches!(parent.as_str(), "main" | "test") || name == "src" && !p.join("main").is_dir() && !p.join("test").is_dir() {
                out.push(p.clone());
            }
            if depth < 5 {
                stack.push((p, depth + 1));
            }
        }
    }
    out.push(root.to_path_buf());
    out
}

struct Import {
    /// `a.b.C`, or `a.b` for `a.b.*`.
    path: String,
    wildcard: bool,
    is_static: bool,
    /// Where it is (chars), for clicks on it.
    range: Range<usize>,
}

fn imports(text: &str) -> Vec<Import> {
    let r = re(r"(?m)^\s*import\s+(static\s+)?([\w.]+?)(\.\*)?\s*;");
    r.captures_iter(text)
        .map(|c| {
            let m = c.get(2).unwrap();
            let a = text[..m.start()].chars().count();
            Import { path: c[2].to_string(), wildcard: c.get(3).is_some(), is_static: c.get(1).is_some(), range: a..a + m.as_str().chars().count() }
        })
        .collect()
}

const MODIFIERS: &str = r"(?:(?:public|protected|private|static|final|abstract|synchronized|native|default|strictfp|sealed|non-sealed|transient|volatile)\s+)*";

/// Where `name` is declared in Java source `text` (byte offset of the name): a type, a method
/// or constructor, else a field or variable.
pub fn declaration(text: &str, name: &str) -> Option<usize> {
    let n = regex::escape(name);
    let ty = re(&format!(r"\b(?:class|interface|enum|record|@interface)\s+({n})\b"));
    if let Some(m) = ty.captures(text).and_then(|c| c.get(1)) {
        return Some(m.start());
    }
    const NOT_TYPES: &[&str] = &["return", "new", "throw", "else", "case", "yield", "await", "assert", "goto"];
    let typed = |tail: &str| re(&format!(r"(?m)^[ \t]*(?:@\w+(?:\([^)]*\))?\s+)*{MODIFIERS}(?:<[^>]*>\s+)?([\w$][\w$.<>\[\]?, ]*?)\s+({n})\s*{tail}"));
    for tail in [r"\(", r"(?:=|;|,|\))"] {
        for c in typed(tail).captures_iter(text) {
            let t = c[1].trim();
            if NOT_TYPES.contains(&t) || t.ends_with('.') {
                continue;
            }
            return c.get(2).map(|m| m.start());
        }
        if tail == r"\(" {
            // Constructors: modifiers straight before the name.
            let ctor = re(&format!(r"(?m)^[ \t]*(?:@\w+(?:\([^)]*\))?\s+)*(?:public|protected|private)\s+({n})\s*\("));
            if let Some(m) = ctor.captures(text).and_then(|c| c.get(1)) {
                return Some(m.start());
            }
        }
    }
    // Enum constants (written in capitals; anything else on its own line is a call).
    if name.chars().any(char::is_lowercase) {
        return None;
    }
    re(&format!(r"(?m)^[ \t]*({n})\s*[,;(]")).captures(text).and_then(|c| c.get(1)).map(|m| m.start())
}

fn target_at(path: &Path, text: &str, byte: usize) -> Target {
    let row = text[..byte].matches('\n').count();
    let col = text[..byte].rsplit('\n').next().unwrap_or("").chars().count();
    Target { path: path.to_path_buf(), row, col }
}

/// The `a.b.` written right before char `at` of `line`.
fn qualifier(line: &[char], mut at: usize) -> Vec<String> {
    let mut segs = Vec::new();
    while at >= 2 && line[at - 1] == '.' {
        let end = at - 1;
        let mut a = end;
        while a > 0 && is_ident(line[a - 1]) {
            a -= 1;
        }
        if a == end {
            break;
        }
        segs.push(line[a..end].iter().collect::<String>());
        at = a;
    }
    segs.reverse();
    segs
}

struct Ctx<'a> {
    file: &'a Path,
    text: &'a str,
    root: Option<&'a Path>,
    package: Option<String>,
    imports: Vec<Import>,
}

impl Ctx<'_> {
    /// The source of class `fqcn` (`a.b.C`, or `a.b.Outer.Inner`): (file, its text, the
    /// declaration of the innermost class in it).
    fn locate(&self, fqcn: &str) -> Option<(PathBuf, String, usize)> {
        let parts: Vec<&str> = fqcn.split('.').collect();
        let mut roots: Vec<PathBuf> = source_root(self.file, self.text).into_iter().collect();
        if let Some(r) = self.root {
            roots.extend(source_roots(r));
        }
        // The file is named after the outermost class: try `a/b/C.java`, then `a/b.java`…
        for k in (1..=parts.len()).rev() {
            let rel = format!("{}.java", parts[..k].join("/"));
            let found = roots.iter().map(|r| r.join(&rel)).find(|p| p.is_file()).or_else(|| library_source(&rel));
            if let Some(p) = found {
                let text = std::fs::read_to_string(&p).ok()?;
                let at = declaration(&text, parts.last()?).unwrap_or(0);
                return Some((p, text, at));
            }
        }
        None
    }

    /// A class named `simple` as this file sees it: imported, in the same package, in a
    /// wildcard import's package, in java.lang, or declared in this file.
    fn class(&self, simple: &str) -> Option<(PathBuf, String, usize)> {
        if let Some(at) = re(&format!(r"\b(?:class|interface|enum|record|@interface)\s+{}\b", regex::escape(simple))).find(self.text) {
            let byte = at.start() + at.as_str().rfind(simple)?;
            return Some((self.file.to_path_buf(), self.text.to_string(), byte));
        }
        if let Some(i) = self.imports.iter().find(|i| !i.wildcard && !i.is_static && i.path.rsplit('.').next() == Some(simple)) {
            return self.locate(&i.path);
        }
        let same = self.package.as_ref().map(|p| format!("{p}.{simple}")).unwrap_or(simple.to_string());
        if let Some(found) = self.locate(&same) {
            return Some(found);
        }
        for i in self.imports.iter().filter(|i| i.wildcard && !i.is_static) {
            if let Some(found) = self.locate(&format!("{}.{simple}", i.path)) {
                return Some(found);
            }
        }
        self.locate(&format!("java.lang.{simple}"))
    }

    /// A dotted path in code or an import: `a.b.C`, `C.member`, `a.b.C.member`.
    fn path(&self, segs: &[String]) -> Option<Target> {
        // The first capitalised segment is a class (after any package); the rest are members.
        let first_class = segs.iter().position(|s| s.chars().next().is_some_and(char::is_uppercase))?;
        let (mut p, mut text, mut at) = if first_class == 0 { self.class(&segs[0])? } else { self.locate(&segs[..=first_class].join("."))? };
        for (k, seg) in segs.iter().enumerate().skip(first_class + 1) {
            match declaration(&text, seg) {
                Some(a) => at = a,
                None if seg.chars().next().is_some_and(char::is_uppercase) => {
                    // A nested class in another file of the same package, or a class as a field type: stop here.
                    let (np, nt, na) = self.locate(&segs[..=k].join("."))?;
                    (p, text, at) = (np, nt, na);
                }
                None => return Some(target_at(&p, &text, at)),
            }
        }
        Some(target_at(&p, &text, at))
    }
}

/// Where the name at `range` (chars in `text`, the contents of `file`) is defined, as far as
/// Java's packages and imports say.
pub fn find(text: &str, range: Range<usize>, name: &str, file: &Path, root: Option<&Path>) -> Option<Target> {
    let ctx = Ctx { file, text, root, package: package_of(text), imports: imports(text) };
    let chars: Vec<char> = text.chars().collect();
    let line_start = chars[..range.start].iter().rposition(|c| *c == '\n').map_or(0, |i| i + 1);
    let line: &[char] = &chars[line_start..];
    let at = range.start - line_start;
    let mut segs = qualifier(line, at);
    segs.push(name.to_string());

    // In an import: the path up to what was clicked.
    if let Some(imp) = ctx.imports.iter().find(|i| i.range.start <= range.start && range.end <= i.range.end) {
        // The import's segments up to the click (the qualifier stops at `import `).
        let _ = imp;
        if !segs.iter().any(|s| s.chars().next().is_some_and(char::is_uppercase)) {
            return None; // a package
        }
        return ctx.path(&segs);
    }
    if segs.len() > 1 {
        // `obj.member`: only when the start is a class or package we can name.
        let head = &segs[0];
        if head.chars().next().is_some_and(char::is_uppercase) || segs.iter().any(|s| s.chars().next().is_some_and(char::is_uppercase)) && ctx.locate(&segs[..segs.len() - 1].join(".")).is_some() {
            return ctx.path(&segs);
        }
        return None;
    }
    if name.chars().next().is_some_and(char::is_uppercase) {
        if let Some((p, t, a)) = ctx.class(name) {
            return Some(target_at(&p, &t, a));
        }
    }
    if let Some(a) = declaration(text, name) {
        return Some(target_at(file, text, a));
    }
    // `import static a.b.C.name;` / `import static a.b.C.*;`
    for i in ctx.imports.iter().filter(|i| i.is_static) {
        let class = if i.wildcard { i.path.clone() } else if i.path.ends_with(&format!(".{name}")) { i.path[..i.path.len() - name.len() - 1].to_string() } else { continue };
        if let Some((p, t, _)) = ctx.locate(&class)
            && let Some(a) = declaration(&t, name)
        {
            return Some(target_at(&p, &t, a));
        }
    }
    None
}

// ---------------------------------------------------------------- library sources

/// `java/util/List.java` from the JDK's src.zip, or a downloaded `-sources.jar`, unpacked
/// into the cache (one file, once).
fn library_source(rel: &str) -> Option<PathBuf> {
    jdk_source(rel).or_else(|| jar_source(rel))
}

fn cache_dir() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    Some(home.join("Library/Caches/oskl-code"))
}

fn short_hash(s: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    format!("{:x}", h.finish())
}

/// Unpacks `entry` of `archive` into `dir` (if not there yet) and returns it.
fn unpack(archive: &Path, entry: &str, dir: &Path) -> Option<PathBuf> {
    let out = dir.join(entry);
    if out.is_file() {
        return Some(out);
    }
    std::fs::create_dir_all(dir).ok()?;
    let ok = Command::new("/usr/bin/unzip").arg("-o").arg("-q").arg(archive).arg(entry).arg("-d").arg(dir).status().ok()?.success();
    (ok && out.is_file()).then_some(out)
}

/// The entries of an archive, listed once into the cache.
fn listing(archive: &Path, dir: &Path) -> Option<String> {
    let file = dir.join("entries.txt");
    if let Ok(s) = std::fs::read_to_string(&file) {
        return Some(s);
    }
    let out = Command::new("/usr/bin/unzip").arg("-Z1").arg(archive).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).to_string();
    std::fs::create_dir_all(dir).ok()?;
    let _ = std::fs::write(&file, &s);
    Some(s)
}

fn jdk_source(rel: &str) -> Option<PathBuf> {
    let zip = java_home()?.join("lib/src.zip");
    if !zip.is_file() {
        return None;
    }
    let dir = cache_dir()?.join(format!("jdk-{}", short_hash(&zip.to_string_lossy())));
    let list = listing(&zip, &dir)?;
    // Entries are per module: `java.base/java/util/List.java`.
    let suffix = format!("/{rel}");
    let entry = list.lines().find(|l| l.ends_with(&suffix) || *l == rel)?;
    unpack(&zip, entry, &dir)
}

/// Downloaded library sources: Maven's `~/.m2` and Gradle's cache, narrowed by the package
/// (`org/apache/…` lives under `~/.m2/repository/org/apache/…`).
fn jar_source(rel: &str) -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let parts: Vec<&str> = rel.split('/').collect();
    let mut jars = Vec::new();
    let m2 = home.join(".m2/repository");
    if m2.is_dir() {
        // The deepest folder of ~/.m2 that the package path reaches.
        let mut dir = m2.clone();
        for p in &parts[..parts.len().saturating_sub(1)] {
            if dir.join(p).is_dir() {
                dir = dir.join(p);
            } else {
                break;
            }
        }
        if dir != m2 {
            sources_jars(&dir, 6, &mut jars);
        }
    }
    let gradle = home.join(".gradle/caches/modules-2/files-2.1");
    if let Ok(rd) = std::fs::read_dir(&gradle) {
        let pkg = parts[..parts.len().saturating_sub(1)].join(".");
        for e in rd.flatten() {
            let group = e.file_name().to_string_lossy().to_string();
            if pkg == group || pkg.starts_with(&format!("{group}.")) {
                sources_jars(&e.path(), 5, &mut jars);
            }
        }
    }
    for jar in jars.into_iter().take(40) {
        let dir = cache_dir()?.join(format!("jar-{}", short_hash(&jar.to_string_lossy())));
        if let Some(list) = listing(&jar, &dir)
            && list.lines().any(|l| l == rel)
        {
            return unpack(&jar, rel, &dir);
        }
    }
    None
}

fn sources_jars(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if depth > 0 {
                sources_jars(&p, depth - 1, out);
            }
        } else if p.to_string_lossy().ends_with("-sources.jar") {
            out.push(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("kite-java-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn syntax_errors_as_you_type() {
        assert!(check_syntax("class A {\n  void f() { int x = 1; }\n}\n").is_empty());
        let d = check_syntax("class A {\n  void f() {\n    int x = 1\n  }\n}\n");
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!((d[0].row, d[0].message.as_str()), (2, "Java: missing `;`"));
    }

    #[test]
    fn reads_javac_output() {
        let out = "src/A.java:3: error: cannot find symbol\n    foo();\n    ^\n  symbol:   method foo()\n  location: class A\nsrc/B.java:1: warning: [deprecation] x\n  y\n  ^\n1 error\n1 warning\n";
        let d = tmp("javac");
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(d.join("src/A.java"), "").unwrap();
        std::fs::write(d.join("src/B.java"), "").unwrap();
        let diags = parse_javac(out, &d.join("src/A.java"), &d);
        assert_eq!(diags.len(), 1, "only this file's");
        assert_eq!((diags[0].row, diags[0].col, diags[0].severity), (2, 4, Severity::Error));
        assert_eq!(diags[0].message, "cannot find symbol\n  symbol: method foo()\n  location: class A");
        assert!(is_missing_dependency(&diags[0].message));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn finds_declarations() {
        let text = "package a;\n\n@Entity\npublic class Shop<T> extends Base {\n    private final List<String> names = new ArrayList<>();\n    public Shop(int x) {}\n    public static <K> Map<K, T> group(List<K> items) throws IOException {\n        return group(items);\n    }\n    enum Kind { BIG, SMALL }\n}\n";
        let at = |n: &str| declaration(text, n).map(|b| target_at(Path::new("x"), text, b)).map(|t| (t.row, t.col));
        assert_eq!(at("Shop"), Some((3, 13)));
        assert_eq!(at("names"), Some((4, 31)));
        assert_eq!(at("group"), Some((6, 32)), "the declaration, not the call in return");
        assert_eq!(at("Kind"), Some((9, 9)));
    }

    #[test]
    fn follows_packages_and_imports() {
        let d = tmp("pkg");
        let java = d.join("src/main/java");
        std::fs::create_dir_all(java.join("com/shop/model")).unwrap();
        std::fs::create_dir_all(java.join("com/shop/util")).unwrap();
        std::fs::write(d.join("pom.xml"), "<project/>").unwrap();
        std::fs::write(java.join("com/shop/model/Item.java"), "package com.shop.model;\n\n/** An item. */\npublic class Item {\n    public static Item of(String name) { return null; }\n    public static class Tag {}\n}\n").unwrap();
        std::fs::write(java.join("com/shop/util/Money.java"), "package com.shop.util;\npublic final class Money {\n    public static final int CENTS = 100;\n    public static String format(int c) { return \"\"; }\n}\n").unwrap();
        std::fs::write(java.join("com/shop/Helper.java"), "package com.shop;\nclass Helper {\n    static void help() {}\n}\n").unwrap();
        let file = java.join("com/shop/App.java");
        let text = "package com.shop;\n\nimport com.shop.model.Item;\nimport com.shop.util.*;\nimport static com.shop.util.Money.format;\n\npublic class App {\n    void run() {\n        Item i = Item.of(\"a\");\n        Item.Tag t;\n        Helper.help();\n        format(Money.CENTS);\n        int local = 1;\n        run();\n    }\n}\n";
        std::fs::write(&file, text).unwrap();
        let go = |needle: &str, nth: usize| {
            let byte = text.match_indices(needle).nth(nth).unwrap().0;
            let a = text[..byte].chars().count();
            find(text, a..a + needle.chars().count(), needle, &file, Some(&d)).map(|t| (t.path.file_name().unwrap().to_string_lossy().to_string(), t.row, t.col))
        };
        assert_eq!(go("Item", 0), Some(("Item.java".into(), 3, 13)), "the import's class");
        assert_eq!(go("model", 0), None, "a package isn't a file");
        assert_eq!(go("Item", 1), Some(("Item.java".into(), 3, 13)), "a type in code");
        assert_eq!(go("of", 0), Some(("Item.java".into(), 4, 23)), "Class.member");
        assert_eq!(go("Tag", 0), Some(("Item.java".into(), 5, 24)), "a nested class");
        assert_eq!(go("Helper", 0), Some(("Helper.java".into(), 1, 6)), "same package, no import");
        assert_eq!(go("help", 0), Some(("Helper.java".into(), 2, 16)));
        assert_eq!(go("Money", 1), Some(("Money.java".into(), 1, 19)), "through a wildcard import");
        assert_eq!(go("CENTS", 0), Some(("Money.java".into(), 2, 28)));
        assert_eq!(go("format", 1), Some(("Money.java".into(), 3, 25)), "a static import");
        assert_eq!(go("local", 0), Some(("App.java".into(), 12, 12)));
        assert_eq!(go("run", 1), Some(("App.java".into(), 7, 9)), "a method of this class");
        std::fs::remove_dir_all(&d).ok();
    }
}
