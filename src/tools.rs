//! Formatters, linters and shell commands, all run as external CLIs.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

static SHELL_PATH: OnceLock<Option<String>> = OnceLock::new();

/// Apps launched from Finder get a minimal PATH. Ask the user's login shell
/// for the real one once, like VS Code does.
pub fn init_shell_path() {
    SHELL_PATH.get_or_init(|| {
        if std::env::var_os("TERM").is_some() {
            return None;
        }
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        let out = Command::new(shell).args(["-lic", "printf '%s' \"$PATH\""]).stdin(Stdio::null()).output().ok()?;
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!path.is_empty()).then_some(path)
    });
}

fn command(program: &Path) -> Command {
    let mut cmd = Command::new(program);
    if let Some(Some(path)) = SHELL_PATH.get() {
        cmd.env("PATH", path);
    }
    cmd.env("NO_COLOR", "1").env("FORCE_COLOR", "0");
    cmd
}

fn search_path() -> Vec<PathBuf> {
    let path = match SHELL_PATH.get() {
        Some(Some(p)) => p.clone(),
        _ => std::env::var("PATH").unwrap_or_default(),
    };
    std::env::split_paths(&path).collect()
}

/// Finds `name` in node_modules/.bin walking up from `dir` to `root`, then on PATH.
fn find_tool(name: &str, dir: &Path, root: &Path) -> Option<PathBuf> {
    let mut cur = Some(dir);
    while let Some(d) = cur {
        let candidate = d.join("node_modules/.bin").join(name);
        if candidate.exists() {
            return Some(candidate);
        }
        if d == root {
            break;
        }
        cur = d.parent();
    }
    search_path().into_iter().map(|p| p.join(name)).find(|p| p.is_file())
}

fn has_config(dir: &Path, root: &Path, names: &[&str]) -> bool {
    let mut cur = Some(dir);
    while let Some(d) = cur {
        if names.iter().any(|n| d.join(n).exists()) {
            return true;
        }
        if d == root {
            break;
        }
        cur = d.parent();
    }
    false
}

const BIOME_CONFIG: &[&str] = &["biome.json", "biome.jsonc"];
const PRETTIER_CONFIG: &[&str] = &[
    ".prettierrc", ".prettierrc.json", ".prettierrc.js", ".prettierrc.cjs", ".prettierrc.mjs",
    ".prettierrc.yaml", ".prettierrc.yml", ".prettierrc.toml", "prettier.config.js", "prettier.config.cjs", "prettier.config.mjs",
];
const ESLINT_CONFIG: &[&str] = &[
    "eslint.config.js", "eslint.config.mjs", "eslint.config.cjs", "eslint.config.ts",
    ".eslintrc", ".eslintrc.js", ".eslintrc.cjs", ".eslintrc.json", ".eslintrc.yml",
];

fn ext(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase()
}

fn is_web(ext: &str) -> bool {
    matches!(ext, "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "mts" | "cts" | "json" | "jsonc" | "css" | "scss" | "less" | "html" | "vue" | "svelte" | "md" | "mdx" | "yaml" | "yml" | "graphql")
}

pub struct ToolOutput {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
    pub tool: String,
}

fn run(mut cmd: Command, tool: String, stdin: Option<&str>, cwd: &Path) -> ToolOutput {
    cmd.current_dir(cwd).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() });
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return ToolOutput { ok: false, stdout: String::new(), stderr: e.to_string(), tool },
    };
    let mut child = child;
    let writer = stdin.map(|text| {
        let mut pipe = child.stdin.take().unwrap();
        let text = text.to_string();
        std::thread::spawn(move || {
            let _ = pipe.write_all(text.as_bytes());
        })
    });
    let out = child.wait_with_output();
    if let Some(w) = writer {
        let _ = w.join();
    }
    match out {
        Ok(o) => ToolOutput {
            ok: o.status.success(),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
            tool,
        },
        Err(e) => ToolOutput { ok: false, stdout: String::new(), stderr: e.to_string(), tool },
    }
}

/// Formats `text` (the unsaved buffer) for `path`. Returns the formatted text.
pub fn format(root: &Path, path: &Path, text: &str) -> Result<(String, String), String> {
    let dir = path.parent().unwrap_or(root);
    let e = ext(path);
    let p = path.to_string_lossy().to_string();
    let (cmd, name) = if is_web(&e) {
        let yaml = matches!(e.as_str(), "yml" | "yaml");
        // Biome doesn't format YAML; Prettier does.
        let biome = find_tool("biome", dir, root).filter(|_| !yaml);
        let prettier = find_tool("prettier", dir, root);
        let use_biome = biome.is_some() && (has_config(dir, root, BIOME_CONFIG) || !has_config(dir, root, PRETTIER_CONFIG) || prettier.is_none());
        if use_biome {
            let mut c = command(biome.as_ref().unwrap());
            c.args(["format", &format!("--stdin-file-path={p}")]);
            (c, "biome")
        } else if let Some(prettier) = prettier {
            let mut c = command(&prettier);
            c.args(["--stdin-filepath", &p]);
            (c, "prettier")
        } else if matches!(e.as_str(), "json") {
            // Built-in JSON formatter keeps key order; files with comments need a real formatter.
            let indent = if text.contains("\n    \"") && !text.contains("\n  \"") { "    " } else { "  " };
            return crate::datafiles::format_json(text, indent)
                .map(|out| (out, "built-in JSON formatter".to_string()))
                .ok_or_else(|| "Can't format: the JSON has errors (or comments — install Prettier or Biome for JSONC).".to_string());
        } else if yaml {
            return Err("Install Prettier to format YAML (npm i -D prettier).".into());
        } else {
            return Err("No formatter found. Install Biome (npm i -D @biomejs/biome) or Prettier.".into());
        }
    } else {
        let (name, args): (&str, Vec<String>) = match e.as_str() {
            "rs" => ("rustfmt", vec!["--emit".into(), "stdout".into(), "--edition".into(), "2024".into()]),
            "go" => ("gofmt", vec![]),
            "py" => ("ruff", vec!["format".into(), "--stdin-filename".into(), p.clone(), "-".into()]),
            "swift" => ("swift-format", vec![]),
            "sh" | "bash" | "zsh" => ("shfmt", vec!["-".into()]),
            "lua" => ("stylua", vec!["-".into()]),
            "c" | "h" | "cpp" | "hpp" | "cc" | "m" | "mm" => ("clang-format", vec![format!("--assume-filename={p}")]),
            _ => return Err(format!("No formatter configured for .{e} files")),
        };
        let Some(bin) = find_tool(name, dir, root) else {
            return Err(format!("{name} not found on PATH"));
        };
        let mut c = command(&bin);
        c.args(args);
        (c, name)
    };
    let out = run(cmd, name.to_string(), Some(text), root);
    if out.ok && !out.stdout.is_empty() {
        Ok((out.stdout, out.tool))
    } else {
        Err(format!("{} failed:\n{}{}", out.tool, out.stderr, out.stdout))
    }
}

/// Lints `target` (a file, or the whole project when None).
pub fn lint(root: &Path, target: Option<&Path>) -> ToolOutput {
    let dir = target.and_then(|t| t.parent()).unwrap_or(root);
    let target_arg = target.map(|t| t.to_string_lossy().to_string()).unwrap_or_else(|| ".".into());
    let e = target.map(ext).unwrap_or_default();
    if target.is_none() || is_web(&e) {
        let eslint = find_tool("eslint", dir, root).filter(|_| has_config(dir, root, ESLINT_CONFIG));
        if let Some(eslint) = eslint {
            let mut c = command(&eslint);
            c.args([&target_arg]);
            return run(c, "eslint".into(), None, root);
        }
        if let Some(biome) = find_tool("biome", dir, root) {
            let mut c = command(&biome);
            c.args(["lint", "--colors=off", &target_arg]);
            return run(c, "biome lint".into(), None, root);
        }
    }
    let (name, args): (&str, Vec<String>) = match e.as_str() {
        "rs" => ("cargo", vec!["clippy".into(), "--message-format=short".into()]),
        "py" => ("ruff", vec!["check".into(), target_arg.clone()]),
        "sh" | "bash" | "zsh" => ("shellcheck", vec!["-f".into(), "gcc".into(), target_arg.clone()]),
        "go" => ("go", vec!["vet".into(), "./...".into()]),
        _ => {
            return ToolOutput {
                ok: false,
                stdout: String::new(),
                stderr: "No linter found. Install ESLint or Biome for JS/TS.".into(),
                tool: "lint".into(),
            };
        }
    };
    match find_tool(name, dir, root) {
        Some(bin) => {
            let mut c = command(&bin);
            c.args(args);
            run(c, name.into(), None, root)
        }
        None => ToolOutput { ok: false, stdout: String::new(), stderr: format!("{name} not found on PATH"), tool: name.into() },
    }
}

pub fn shell(root: &Path, line: &str) -> ToolOutput {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let mut c = command(Path::new(&shell));
    c.args(["-c", line]);
    run(c, line.into(), None, root)
}

pub fn comment_prefix(path: Option<&Path>) -> &'static str {
    match path.map(ext).as_deref() {
        Some("py" | "sh" | "bash" | "zsh" | "rb" | "toml" | "yaml" | "yml" | "pl" | "r" | "conf" | "ini" | "dockerfile" | "mk") => "#",
        Some("lua" | "sql" | "hs") => "--",
        Some("lisp" | "clj" | "el") => ";",
        Some("vim") => "\"",
        _ => "//",
    }
}

pub fn find_named(name: &str, dir: &Path, root: &Path) -> Option<PathBuf> {
    find_tool(name, dir, root)
}

/// ESLint, only when the project actually configures it.
pub fn find_eslint(dir: &Path, root: &Path) -> Option<PathBuf> {
    find_tool("eslint", dir, root).filter(|_| has_config(dir, root, ESLINT_CONFIG))
}

/// Runs a tool and returns its stdout (diagnostic formats are printed there).
pub fn run_tool(bin: &Path, args: &[&str], cwd: &Path) -> String {
    let mut c = command(bin);
    c.args(args);
    run(c, String::new(), None, cwd).stdout
}
