//! A commit message drafted by Claude Code (`claude -p`, the staged diff on its input).
//! `OSKL_CLAUDE` names another program to run instead (the tests' fake).

use super::run;
use crate::editor::Editor;
use crate::request::Request;
use crate::{theme, ui};
use gpui::prelude::*;
use gpui::*;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const PROMPT: &str = "Write a git commit message for the staged diff on stdin. First line: a subject of at most 72 characters, in the imperative mood, with no trailing period. If the change needs it, then a blank line and a short body that says why, not what. Answer with the message only: no quotes, no code fences, no preface.";
/// shortcut: a bigger diff is cut here (the message is drafted from its start); upgrade by sending `--stat` and the largest hunks.
const MAX_DIFF: usize = 60_000;
const LIMIT: Duration = Duration::from_secs(120);

/// The message for what is staged in `root`. Stops (and kills Claude Code) when `cancel` is set.
pub fn draft(root: &Path, cancel: &AtomicBool) -> Result<String, String> {
    let diff = run(root, &["diff", "--cached", "--no-color"]).map_err(|e| e.to_string())?;
    if diff.trim().is_empty() {
        return Err("Nothing is staged: stage the changes you want described first".into());
    }
    let bin = match std::env::var_os("OSKL_CLAUDE") {
        Some(b) => PathBuf::from(b),
        None => crate::agent::claude_bin()?,
    };
    let mut cut = diff.len().min(MAX_DIFF);
    while !diff.is_char_boundary(cut) {
        cut -= 1;
    }
    let diff = if cut < diff.len() { format!("{}\n[the diff is cut here]\n", &diff[..cut]) } else { diff };
    let mut child = crate::tools::command(&bin)
        .args(["-p", "--no-session-persistence", PROMPT])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Couldn't run Claude Code: {e}"))?;
    let mut input = child.stdin.take().ok_or("Couldn't run Claude Code")?;
    std::thread::spawn(move || {
        let _ = input.write_all(diff.as_bytes());
    });
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(mut p) = pipe {
                p.read_to_string(&mut s).ok();
            }
            s
        })
    };
    let out = read(child.stdout.take().map(|p| Box::new(p) as _));
    let err = read(child.stderr.take().map(|p| Box::new(p) as _));
    let started = Instant::now();
    let status = loop {
        if cancel.load(Ordering::Relaxed) {
            child.kill().ok();
            child.wait().ok();
            return Err("Stopped".into());
        }
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if started.elapsed() > LIMIT => {
                child.kill().ok();
                child.wait().ok();
                return Err("Claude Code took too long to answer".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(40)),
            Err(e) => return Err(format!("Couldn't run Claude Code: {e}")),
        }
    };
    let (out, err) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
    if !status.success() {
        let why = err.lines().chain(out.lines()).map(str::trim).find(|l| !l.is_empty()).unwrap_or("it stopped without saying why");
        return Err(format!("Claude Code couldn't draft a message: {why}"));
    }
    let message = unfence(&out);
    if message.is_empty() { Err("Claude Code answered with nothing".into()) } else { Ok(message) }
}

/// The answer without a code fence around it.
fn unfence(text: &str) -> String {
    let t = text.trim();
    let t = t.strip_prefix("```").map(|r| r.split_once('\n').map_or("", |(_, rest)| rest)).unwrap_or(t);
    t.trim().trim_end_matches("```").trim().to_string()
}

/// A draft being written in the background, which can be stopped.
#[derive(Default)]
pub struct Drafter {
    stop: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
    task: Option<Task<()>>,
}

impl Drafter {
    pub fn running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    pub fn cancel(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.running.store(false, Ordering::Relaxed);
        self.task = None;
    }

    /// The button was clicked: stops a draft being written, or starts one into `editor`.
    /// A message typed meanwhile is kept: the draft doesn't replace it.
    pub fn toggle<V: EventEmitter<Request> + 'static>(&mut self, root: PathBuf, editor: &Entity<Editor>, cx: &mut Context<V>) {
        if self.running() {
            self.cancel();
            cx.emit(Request::Message("Stopped drafting".into()));
            return cx.notify();
        }
        let (stop, running) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(true)));
        (self.stop, self.running) = (stop.clone(), running.clone());
        let job = cx.background_executor().spawn(async move { draft(&root, &stop) });
        let (editor, typed) = (editor.clone(), editor.read(cx).text());
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = job.await;
            running.store(false, Ordering::Relaxed);
            this.update(cx, |_, cx| {
                match result {
                    Err(e) => cx.emit(Request::Error(e)),
                    Ok(_) if editor.read(cx).text() != typed => cx.emit(Request::Message("Drafted, but you changed the message meanwhile, so it is as you left it".into())),
                    Ok(message) => {
                        editor.update(cx, |e, cx| {
                            e.set_text(&message, cx);
                            e.go_to(0, 0, cx);
                        });
                        cx.emit(Request::Message("Drafted from the staged changes. Edit it before you commit".into()));
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }
}

/// The ✦ Draft button (the caller adds what a click does); while drafting it says so and stops it.
pub fn button(id: &'static str, running: bool) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || id.to_string())
        .flex()
        .flex_none()
        .items_center()
        .h(ui::px(22.))
        .px(ui::px(8.))
        .rounded(ui::px(4.))
        .border_1()
        .border_color(theme::border_input())
        .text_size(ui::px(11.5))
        .whitespace_nowrap()
        .cursor_pointer()
        .text_color(if running { theme::accent() } else { theme::fg() })
        .hover(|s| s.bg(theme::hover()))
        .tooltip(ui::tooltip(if running { "Drafting with Claude Code: click to stop" } else { "Draft the message from the staged changes, with Claude Code" }))
        .child(if running { "Drafting… ✕" } else { "✦ Draft" })
}

/// The same, as a sparkle inside a message box (an ✕ while drafting).
pub fn icon(id: &'static str, running: bool) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || id.to_string())
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(ui::px(20.))
        .rounded(ui::px(4.))
        .cursor_pointer()
        .hover(|s| s.bg(theme::hover()))
        .tooltip(ui::tooltip(if running { "Drafting with Claude Code: click to stop" } else { "Draft the message from the staged changes, with Claude Code" }))
        .child(if running { ui::small_icon("close", theme::accent()) } else { ui::small_icon("sparkle", theme::fg_muted()) })
}

#[cfg(test)]
mod tests {
    #[test]
    fn code_fences_are_dropped() {
        assert_eq!(super::unfence("```\nFix it\n\nWhy.\n```\n"), "Fix it\n\nWhy.");
        assert_eq!(super::unfence("```text\nFix it\n```"), "Fix it");
        assert_eq!(super::unfence("  Fix it \n"), "Fix it");
    }
}
