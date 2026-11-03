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
    let classpath = classpath(root, file);
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
        // Libraries that aren't downloaded: hide what comes from them, and only that.
        let known = Known::new(&sourcepath, &classpath);
        let lombok = module_dirs(file, root).iter().any(|d| build_text(d).contains("lombok"));
        drop_library_noise(&mut diags, &text, &known, lombok);
    }
    widen(&mut diags, &text);
    if !output.status.success() && diags.is_empty() && !stderr.contains("error") {
        let line = stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("exited with an error");
        return Err(format!("javac failed: {line}"));
    }
    Ok(("javac".into(), diags))
}

/// What javac can compile against: the project's compiled classes, `lib/*.jar`, and the
/// dependencies its pom.xml / build.gradle declare that are already downloaded.
fn classpath(root: &Path, file: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in module_dirs(file, root) {
        for p in ["target/classes", "target/test-classes", "build/classes/java/main", "build/classes/java/test", "out/production", "bin"] {
            let p = dir.join(p);
            if p.is_dir() && !out.contains(&p) {
                out.push(p);
            }
        }
        if let Ok(rd) = std::fs::read_dir(dir.join("lib")) {
            out.extend(rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "jar")));
        }
        for jar in declared_jars(&dir) {
            if !out.contains(&jar) {
                out.push(jar);
            }
        }
    }
    out
}

/// The file's module folder and the ones above it, up to the project (multi-module builds).
fn module_dirs(file: &Path, root: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = file.ancestors().skip(1).take_while(|d| d.starts_with(root)).filter(|d| d == &root || !build_text(d).is_empty()).map(Path::to_path_buf).collect();
    if dirs.is_empty() {
        dirs.push(root.to_path_buf());
    }
    dirs
}

/// pom.xml and build.gradle(.kts) of a folder, as text ("" if none).
fn build_text(dir: &Path) -> String {
    ["pom.xml", "build.gradle", "build.gradle.kts"].iter().filter_map(|f| std::fs::read_to_string(dir.join(f)).ok()).collect::<Vec<_>>().join("\n")
}

/// `group:artifact:version` of each declared dependency (version may be empty: managed elsewhere).
fn declared_deps(dir: &Path) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    if let Ok(pom) = std::fs::read_to_string(dir.join("pom.xml")) {
        let tag = |block: &str, t: &str| re(&format!(r"<{t}>\s*([^<]*?)\s*</{t}>")).captures(block).map(|c| c[1].to_string());
        let props: std::collections::HashMap<String, String> = pom
            .split_once("<properties>")
            .and_then(|(_, r)| r.split_once("</properties>"))
            .map(|(p, _)| re(r"<([\w.\-]+)>\s*([^<]*?)\s*</[\w.\-]+>").captures_iter(p).map(|c| (c[1].to_string(), c[2].to_string())).collect())
            .unwrap_or_default();
        let expand = |v: String| re(r"\$\{([^}]+)\}").replace_all(&v, |c: &regex::Captures| props.get(&c[1]).cloned().unwrap_or_default()).to_string();
        for block in re(r"(?s)<dependency>(.*?)</dependency>").captures_iter(&pom) {
            if let (Some(g), Some(a)) = (tag(&block[1], "groupId"), tag(&block[1], "artifactId")) {
                out.push((expand(g), expand(a), expand(tag(&block[1], "version").unwrap_or_default())));
            }
        }
    }
    for f in ["build.gradle", "build.gradle.kts"] {
        if let Ok(g) = std::fs::read_to_string(dir.join(f)) {
            for c in re(r#"["']([\w.\-]+):([\w.\-]+)(?::([\w.\-]+))?["']"#).captures_iter(&g) {
                out.push((c[1].to_string(), c[2].to_string(), c.get(3).map(|m| m.as_str().to_string()).unwrap_or_default()));
            }
        }
    }
    out
}

/// The downloaded jars of a folder's declared dependencies (`~/.m2`, Gradle's cache).
fn declared_jars(dir: &Path) -> Vec<PathBuf> {
    std::env::var_os("HOME").map(|h| declared_jars_in(dir, &PathBuf::from(h))).unwrap_or_default()
}

fn declared_jars_in(dir: &Path, home: &Path) -> Vec<PathBuf> {
    // Without a version (a BOM or parent manages it), the newest one downloaded.
    let version_dir = |base: PathBuf, v: &str| -> Option<PathBuf> {
        if !v.is_empty() {
            return Some(base.join(v)).filter(|d| d.is_dir());
        }
        let mut vs: Vec<PathBuf> = std::fs::read_dir(&base).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        vs.sort();
        vs.pop()
    };
    let mut out = Vec::new();
    for (g, a, v) in declared_deps(dir) {
        let m2 = home.join(".m2/repository").join(g.replace('.', "/")).join(&a);
        if let Some(vd) = version_dir(m2, &v) {
            let ver = vd.file_name().unwrap_or_default().to_string_lossy().to_string();
            let jar = vd.join(format!("{a}-{ver}.jar"));
            if jar.is_file() {
                out.push(jar);
                continue;
            }
        }
        let gradle = home.join(".gradle/caches/modules-2/files-2.1").join(&g).join(&a);
        if let Some(vd) = version_dir(gradle, &v) {
            let ver = vd.file_name().unwrap_or_default().to_string_lossy().to_string();
            let want = format!("{a}-{ver}.jar");
            if let Some(jar) = std::fs::read_dir(&vd).into_iter().flatten().flatten().map(|h| h.path().join(&want)).find(|j| j.is_file()) {
                out.push(jar);
            }
        }
    }
    out
}

/// Which packages exist: the project's folders, the JDK (its src.zip) and the classpath.
struct Known {
    dirs: Vec<PathBuf>,
    jars: std::collections::HashSet<String>,
}

impl Known {
    fn new(sourcepath: &[PathBuf], classpath: &[PathBuf]) -> Self {
        let mut jars = std::collections::HashSet::new();
        for jar in classpath.iter().filter(|p| p.is_file()) {
            if let Some(dir) = cache_dir().map(|c| c.join(format!("jar-{}", short_hash(&jar.to_string_lossy()))))
                && let Some(list) = listing(jar, &dir)
            {
                jars.extend(list.lines().filter(|l| l.ends_with(".class")).filter_map(|l| l.rsplit_once('/')).map(|(p, _)| p.replace('/', ".")));
            }
        }
        let dirs = sourcepath.iter().chain(classpath.iter().filter(|p| p.is_dir())).cloned().collect();
        Self { dirs, jars }
    }

    fn package(&self, pkg: &str) -> bool {
        // java.* can only be the JDK's, so an error about one is real (a typo like java.utl).
        if pkg == "java" || pkg.starts_with("java.") || jdk_packages().is_some_and(|jdk| jdk.contains(pkg)) {
            return true;
        }
        let rel = pkg.replace('.', "/");
        self.jars.contains(pkg) || self.dirs.iter().any(|d| d.join(&rel).is_dir())
    }
}

/// The JDK's packages, from the entries of its src.zip (`java.base/java/util/List.java`).
fn jdk_packages() -> Option<&'static std::collections::HashSet<String>> {
    static PKGS: OnceLock<Option<std::collections::HashSet<String>>> = OnceLock::new();
    PKGS.get_or_init(|| {
        let zip = java_home()?.join("lib/src.zip");
        let dir = cache_dir()?.join(format!("jdk-{}", short_hash(&zip.to_string_lossy())));
        let list = listing(&zip, &dir)?;
        Some(list.lines().filter(|l| l.ends_with(".java")).filter_map(|l| l.split_once('/').map(|(_, r)| r)).filter_map(|l| l.rsplit_once('/')).map(|(p, _)| p.replace('/', ".")).collect())
    })
    .as_ref()
}

/// `org.lib.Outer.Inner` → `org.lib`: packages are the lower-case names before the first type.
fn package_part(path: &str) -> String {
    path.split('.').take_while(|s| s.chars().next().is_some_and(|c| c.is_lowercase())).collect::<Vec<_>>().join(".")
}

/// Removes errors that only say a library isn't there (not downloaded, or Maven/Gradle not run):
/// its packages, the names imported from them, what a class gets from extending one, and what
/// Lombok would generate. Everything else stays: typos in your own names, a misspelled JDK
/// package, a wrong method on a known type.
fn drop_library_noise(diags: &mut Vec<Diag>, text: &str, known: &Known, lombok: bool) {
    let imports = imports(text);
    let missing = |path: &str| !known.package(&package_part(path));
    // Simple names that can only come from a missing library.
    let mut lib_types = std::collections::HashSet::new();
    let mut lib_members = std::collections::HashSet::new();
    let (mut open_types, mut open_members) = (false, false);
    for i in &imports {
        if !missing(&i.path) {
            continue;
        }
        let last = i.path.rsplit('.').next().unwrap_or("").to_string();
        match (i.is_static, i.wildcard) {
            (false, false) => {
                lib_types.insert(last);
            }
            (false, true) => open_types = true,
            (true, false) => {
                lib_members.insert(last);
            }
            (true, true) => open_members = true,
        }
    }
    let symbol = |d: &Diag| d.message.lines().find_map(|l| l.trim().strip_prefix("symbol:")).map(|s| s.trim().to_string()).unwrap_or_default();
    let location = |d: &Diag| d.message.lines().find_map(|l| l.trim().strip_prefix("location:")).map(|s| s.trim().to_string()).unwrap_or_default();
    let first = |d: &Diag| d.message.lines().next().unwrap_or("").to_string();
    let class_name = |s: &str| s.strip_prefix("class ").map(|n| n.split('<').next().unwrap_or(n).trim().to_string());
    let is_lib_type = |n: &str| lib_types.contains(n) || open_types;
    // Types this file extends or implements; if one is a library's, its members are unknown too.
    let supers: Vec<String> = re(r"(?s)\b(?:extends|implements)\s+([^{;]+)\{")
        .captures_iter(text)
        .flat_map(|c| c[1].split(|ch: char| !is_ident(ch) && ch != '.').filter(|w| !w.is_empty()).map(|w| w.rsplit('.').next().unwrap_or(w).to_string()).collect::<Vec<_>>())
        .collect();
    let extends_lib = diags.iter().any(|d| first(d) == "cannot find symbol" && class_name(&symbol(d)).is_some_and(|n| supers.contains(&n) && is_lib_type(&n)));
    let lines: Vec<&str> = text.lines().collect();
    diags.retain(|d| {
        let msg = first(d);
        if let Some(pkg) = msg.strip_prefix("package ").and_then(|r| r.strip_suffix(" does not exist")) {
            return known.package(pkg);
        }
        if msg.starts_with("static import only from classes") {
            let line = lines.get(d.row).copied().unwrap_or("");
            return !imports.iter().any(|i| i.is_static && line.contains(&i.path) && missing(&i.path));
        }
        if msg.starts_with("cannot access") {
            return false;
        }
        if msg.starts_with("method does not override") {
            return !extends_lib;
        }
        if lombok && msg.starts_with("constructor ") && msg.contains("cannot be applied") {
            return false;
        }
        if msg != "cannot find symbol" {
            return true;
        }
        let sym = symbol(d);
        if let Some(name) = class_name(&sym) {
            return !is_lib_type(&name);
        }
        let member = sym.strip_prefix("method ").or_else(|| sym.strip_prefix("variable ")).map(|m| m.split('(').next().unwrap_or(m).trim().to_string());
        let Some(member) = member else { return true };
        if lombok {
            return false;
        }
        // Unqualified, in this class: maybe statically imported or inherited from a library.
        let here = ["class ", "interface ", "enum ", "record "].iter().any(|k| location(d).starts_with(k));
        !(here && (lib_members.contains(&member) || open_members || extends_lib))
    });
}

/// javac points at one character; underline the whole name there (or the import's path).
fn widen(diags: &mut [Diag], text: &str) {
    let lines: Vec<Vec<char>> = text.lines().map(|l| l.chars().collect()).collect();
    for d in diags.iter_mut().filter(|d| d.end_row == d.row && d.end_col == d.col) {
        let Some(line) = lines.get(d.row) else { continue };
        let s: String = line.iter().collect();
        if let Some(rest) = s.trim_start().strip_prefix("import ") {
            let start = line.len() - s.trim_start().len() + "import ".len() + rest.find(|c: char| !c.is_whitespace()).unwrap_or(0);
            let end = s.rfind(';').map(|e| s[..e].chars().count()).unwrap_or(line.len());
            if end > start {
                (d.col, d.end_col) = (start, end);
            }
            continue;
        }
        // `m.sizee()`: javac points at the dot before the name.
        if line.get(d.col) == Some(&'.') && line.get(d.col + 1).copied().is_some_and(is_ident) {
            d.col += 1;
        }
        if line.get(d.col).copied().is_some_and(is_ident) {
            let end = (d.col..line.len()).find(|&i| !is_ident(line[i])).unwrap_or(line.len());
            d.end_col = end;
        }
    }
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
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn hides_only_what_missing_libraries_cause() {
        if javac().is_none() {
            return;
        }
        let d = tmp("noise");
        let java = d.join("src/main/java/app");
        std::fs::create_dir_all(&java).unwrap();
        std::fs::write(d.join("pom.xml"), "<project><dependencies><dependency><groupId>org.lib</groupId><artifactId>lib</artifactId><version>1</version></dependency></dependencies></project>").unwrap();
        std::fs::write(java.join("Model.java"), "package app;\npublic class Model {\n    public int size() { return 0; }\n}\n").unwrap();
        let check = |name: &str, text: &str| {
            let file = java.join(name);
            std::fs::write(&file, text).unwrap();
            let (_, diags) = javac_check(&d, &file).unwrap();
            diags.into_iter().map(|d| (d.row + 1, d.message.lines().next().unwrap().to_string(), d.message.lines().nth(1).unwrap_or("").trim().to_string(), d.end_col - d.col)).collect::<Vec<_>>()
        };
        // Uses a library, extends nothing: everything that isn't the library's shows.
        let uses = check(
            "Uses.java",
            "package app;\n\nimport java.utl.Map;\nimport org.lib.Thing;\n\npublic class Uses {\n    void run(Model m) {\n        Thing t = new Thing();\n        t.go();\n        Strng s = \"x\";\n        int count = 0;\n        System.out.println(cont);\n        m.sizee();\n        undefined();\n    }\n}\n",
        );
        let lines: Vec<usize> = uses.iter().map(|u| u.0).collect();
        assert_eq!(lines, [3, 10, 12, 13, 14], "{uses:#?}");
        assert_eq!(uses[0].1, "package java.utl does not exist", "a misspelled JDK package is real");
        assert_eq!((uses[1].2.as_str(), uses[1].3), ("symbol: class Strng", 5), "the whole name is underlined");
        assert_eq!(uses[3].2, "symbol: method sizee()", "a wrong method on your own class");

        // Extends a library class: what it might inherit is unknown, so only sure errors show.
        let extends = check(
            "Child.java",
            "package app;\n\nimport org.lib.Base;\nimport static org.junit.Assert.assertEquals;\n\npublic class Child extends Base {\n    @Override\n    void run(Model m) {\n        helper();\n        assertEquals(1, 2);\n        m.sizee();\n    }\n}\n",
        );
        assert_eq!(extends.iter().map(|u| (u.0, u.2.clone())).collect::<Vec<_>>(), [(11, "symbol: method sizee()".to_string())], "{extends:#?}");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_library_that_is_there_is_checked_for_real() {
        if javac().is_none() {
            return;
        }
        let d = tmp("jar");
        // A tiny library jar in lib/: org.lib.Thing with go().
        let lib = d.join("libsrc/org/lib");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(lib.join("Thing.java"), "package org.lib;\npublic class Thing { public void go() {} }\n").unwrap();
        std::fs::create_dir_all(d.join("lib")).unwrap();
        let sh = |cmd: &str| assert!(Command::new("sh").arg("-c").arg(cmd).current_dir(&d).status().unwrap().success(), "{cmd}");
        sh(&format!("{} -d libout libsrc/org/lib/Thing.java && cd libout && /usr/bin/zip -qr ../lib/thing.jar org", javac().unwrap().display()));
        let java = d.join("src/main/java/app");
        std::fs::create_dir_all(&java).unwrap();
        std::fs::write(d.join("pom.xml"), "<project/>").unwrap();
        let file = java.join("App.java");
        std::fs::write(&file, "package app;\n\nimport org.lib.Thing;\nimport org.lib.Thingg;\n\npublic class App {\n    void run() {\n        new Thing().go();\n        new Thing().goo();\n    }\n}\n").unwrap();
        let (_, diags) = javac_check(&d, &file).unwrap();
        let got: Vec<(usize, String)> = diags.iter().map(|d| (d.row + 1, d.message.lines().nth(1).unwrap_or("").trim().to_string())).collect();
        assert_eq!(got, [(4, "symbol: class Thingg".to_string()), (9, "symbol: method goo()".to_string())], "its package exists, so a wrong name in it is a real error");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn finds_downloaded_dependencies() {
        let d = tmp("deps");
        let home = d.join("home");
        let m2 = home.join(".m2/repository/com/acme/core/2.1");
        std::fs::create_dir_all(&m2).unwrap();
        std::fs::write(m2.join("core-2.1.jar"), "").unwrap();
        let gradle = home.join(".gradle/caches/modules-2/files-2.1/org.x/util/1.0/abc123");
        std::fs::create_dir_all(&gradle).unwrap();
        std::fs::write(gradle.join("util-1.0.jar"), "").unwrap();
        let module = d.join("app");
        std::fs::create_dir_all(&module).unwrap();
        std::fs::write(module.join("pom.xml"), "<project><properties><core.version>2.1</core.version></properties><dependencies><dependency><groupId>com.acme</groupId><artifactId>core</artifactId><version>${core.version}</version></dependency><dependency><groupId>not</groupId><artifactId>here</artifactId></dependency></dependencies></project>").unwrap();
        std::fs::write(module.join("build.gradle"), "dependencies {\n  implementation 'org.x:util:1.0'\n}\n").unwrap();
        let jars = declared_jars_in(&module, &home);
        assert_eq!(jars, [m2.join("core-2.1.jar"), gradle.join("util-1.0.jar")]);
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
