//! Project-wide search & replace, modelled on VS Code's Search view.

use crate::editor::{Editor, EditorEvent, EditorMode};
use crate::repo::Repo;
use crate::request::Request;
use crate::{theme, ui};
use gpui::prelude::*;
use gpui::*;
use regex::{Regex, RegexBuilder};
use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

actions!(search, [ToggleCase, ToggleWord, ToggleRegex, ResultDown, ResultUp, OpenResult, DismissResult, ReplaceResult, CollapseFile, ExpandFile, FocusQuery]);

pub fn bind_keys() {
    let p = Some("SearchPanel");
    crate::keybindings::collect([
        crate::keybindings::kb("alt-cmd-c", ToggleCase, p),
        crate::keybindings::kb("alt-cmd-w", ToggleWord, p),
        crate::keybindings::kb("alt-cmd-r", ToggleRegex, p),
    ]);
    let r = Some("SearchResults");
    crate::keybindings::collect([
        crate::keybindings::kb("j", ResultDown, r),
        crate::keybindings::kb("down", ResultDown, r),
        crate::keybindings::kb("k", ResultUp, r),
        crate::keybindings::kb("up", ResultUp, r),
        crate::keybindings::kb("enter", OpenResult, r),
        crate::keybindings::kb("o", OpenResult, r),
        crate::keybindings::kb("x", DismissResult, r),
        crate::keybindings::kb("delete", DismissResult, r),
        crate::keybindings::kb("backspace", DismissResult, r),
        crate::keybindings::kb("r", ReplaceResult, r),
        crate::keybindings::kb("h", CollapseFile, r),
        crate::keybindings::kb("left", CollapseFile, r),
        crate::keybindings::kb("l", ExpandFile, r),
        crate::keybindings::kb("right", ExpandFile, r),
        crate::keybindings::kb("i", FocusQuery, r),
    ]);
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Options {
    pub case: bool,
    pub word: bool,
    pub regex: bool,
}

#[derive(Clone)]
struct Match {
    line: usize,
    /// Char columns of the match in the full line.
    cols: Range<usize>,
    /// Byte offset of the match in the file.
    offset: usize,
    preview: String,
    preview_hl: Range<usize>,
    dismissed: bool,
}

struct FileResult {
    path: String,
    stamp: Option<(u64, SystemTime)>,
    matches: Vec<Match>,
    collapsed: bool,
    dismissed: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Row {
    File(usize),
    Match(usize, usize),
}

/// A planned replacement, applied by the workspace (open files are edited in their editor).
pub struct ReplacePlan {
    pub files: Vec<(PathBuf, String, usize)>,
    pub skipped: Vec<String>,
}

pub struct SearchPanel {
    focus: FocusHandle,
    results_focus: FocusHandle,
    repo: Entity<Repo>,
    pub query: Entity<Editor>,
    replace: Entity<Editor>,
    include: Entity<Editor>,
    exclude: Entity<Editor>,
    opts: Options,
    show_replace: bool,
    show_filters: bool,
    results: Vec<FileResult>,
    rows: Vec<Row>,
    selected: Option<Row>,
    searching: bool,
    truncated: bool,
    error: Option<String>,
    last_search: Option<(String, Options, String, String)>,
    scroll: UniformListScrollHandle,
    task: Option<Task<()>>,
    pending_scope: Option<Option<Row>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<Request> for SearchPanel {}

pub enum SearchEvent {
    Replace(ReplacePlan),
}

impl EventEmitter<SearchEvent> for SearchPanel {}

fn input(placeholder: &str, cx: &mut App) -> Entity<Editor> {
    cx.new(|cx| {
        let mut ed = Editor::new(EditorMode::SingleLine, cx);
        ed.placeholder = placeholder.to_string().into();
        ed
    })
}

impl SearchPanel {
    pub fn new(repo: Entity<Repo>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = input("Search", cx);
        let replace = input("Replace", cx);
        let include = input("e.g. *.ts, src/**/include", cx);
        let exclude = input("e.g. *.test.ts, dist", cx);
        let mut subs = Vec::new();
        for (ix, ed) in [&query, &replace, &include, &exclude].into_iter().enumerate() {
            subs.push(cx.subscribe_in(ed, window, move |panel, _, ev: &EditorEvent, window, cx| match ev {
                EditorEvent::Changed if ix == 1 => cx.notify(),
                EditorEvent::Changed => panel.schedule_search(cx),
                EditorEvent::Submit if ix == 1 => panel.replace_all(cx),
                EditorEvent::Submit => panel.run_search(cx),
                EditorEvent::Down => {
                    window.focus(&panel.results_focus);
                    if panel.selected.is_none() {
                        panel.selected = panel.rows.first().copied();
                    }
                    cx.notify();
                }
                EditorEvent::Cancel => window.focus(&panel.results_focus),
                _ => {}
            }));
        }
        Self {
            focus: cx.focus_handle(),
            results_focus: cx.focus_handle(),
            repo,
            query,
            replace,
            include,
            exclude,
            opts: Options::default(),
            show_replace: true,
            show_filters: false,
            results: Vec::new(),
            rows: Vec::new(),
            selected: None,
            searching: false,
            truncated: false,
            error: None,
            last_search: None,
            scroll: UniformListScrollHandle::new(),
            task: None,
            pending_scope: None,
            _subs: subs,
        }
    }

    pub fn focus_query(&self, window: &mut Window, cx: &mut App) {
        let query = self.query.clone();
        query.update(cx, |q, cx| {
            q.buf.select_all();
            cx.notify();
        });
        query.read(cx).focus(window);
    }

    pub fn set_query(&mut self, text: &str, cx: &mut Context<Self>) {
        self.query.update(cx, |q, cx| q.set_text(text, cx));
    }

    fn matcher(query: &str, opts: Options) -> Result<Regex, String> {
        let mut pat = if opts.regex { query.to_string() } else { regex::escape(query) };
        if opts.word {
            pat = format!(r"\b(?:{pat})\b");
        }
        RegexBuilder::new(&pat).case_insensitive(!opts.case).build().map_err(|e| e.to_string())
    }

    fn schedule_search(&mut self, cx: &mut Context<Self>) {
        self.task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(250)).await;
            this.update(cx, |p, cx| p.run_search(cx)).ok();
        }));
    }

    pub fn run_search(&mut self, cx: &mut Context<Self>) {
        let query = self.query.read(cx).text();
        let include = self.include.read(cx).text();
        let exclude = self.exclude.read(cx).text();
        let key = (query.clone(), self.opts, include.clone(), exclude.clone());
        if query.is_empty() {
            self.results.clear();
            self.rebuild_rows();
            self.error = None;
            self.last_search = None;
            return cx.notify();
        }
        let re = match Self::matcher(&query, self.opts) {
            Ok(re) => re,
            Err(e) => {
                self.error = Some(e);
                return cx.notify();
            }
        };
        let (inc, exc) = (globs(&include), globs(&exclude));
        let repo = self.repo.read(cx);
        let root = repo.root.clone();
        let files = repo.files.clone();
        self.error = None;
        self.searching = true;
        self.last_search = Some(key);
        cx.notify();
        let work = cx.background_executor().spawn(async move {
            let candidates: Vec<&String> = files
                .iter()
                .filter(|f| (inc.is_empty() || inc.iter().any(|g| g.is_match(f))) && !exc.iter().any(|g| g.is_match(f)))
                .collect();
            search_files(&root, &candidates, &re)
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            let (results, truncated) = work.await;
            this.update(cx, |p, cx| {
                p.searching = false;
                p.truncated = truncated;
                p.results = results;
                p.selected = None;
                p.rebuild_rows();
                cx.notify();
            })
            .ok();
        }));
    }

    fn rebuild_rows(&mut self) {
        self.rows.clear();
        for (fi, f) in self.results.iter().enumerate() {
            if f.dismissed {
                continue;
            }
            self.rows.push(Row::File(fi));
            if !f.collapsed {
                for (mi, m) in f.matches.iter().enumerate() {
                    if !m.dismissed {
                        self.rows.push(Row::Match(fi, mi));
                    }
                }
            }
        }
        if let Some(sel) = self.selected
            && !self.rows.contains(&sel)
        {
            self.selected = None;
        }
    }

    fn counts(&self) -> (usize, usize) {
        let files = self.results.iter().filter(|f| !f.dismissed && f.matches.iter().any(|m| !m.dismissed)).count();
        let matches = self.results.iter().filter(|f| !f.dismissed).map(|f| f.matches.iter().filter(|m| !m.dismissed).count()).sum();
        (matches, files)
    }

    fn dismiss(&mut self, row: Row, cx: &mut Context<Self>) {
        let pos = self.rows.iter().position(|r| *r == row);
        match row {
            Row::File(fi) => self.results[fi].dismissed = true,
            Row::Match(fi, mi) => {
                self.results[fi].matches[mi].dismissed = true;
                if self.results[fi].matches.iter().all(|m| m.dismissed) {
                    self.results[fi].dismissed = true;
                }
            }
        }
        self.rebuild_rows();
        if let Some(p) = pos {
            self.selected = self.rows.get(p.min(self.rows.len().saturating_sub(1))).copied();
        }
        cx.notify();
    }

    fn open(&self, row: Row, cx: &mut Context<Self>) {
        let root = self.repo.read(cx).root.clone();
        match row {
            Row::File(fi) => {
                let f = &self.results[fi];
                let first = f.matches.iter().find(|m| !m.dismissed);
                cx.emit(Request::OpenAt { path: root.join(&f.path), line: first.map(|m| m.line).unwrap_or(0), cols: first.map(|m| m.cols.clone()).unwrap_or(0..0) });
            }
            Row::Match(fi, mi) => {
                let (f, m) = (&self.results[fi], &self.results[fi].matches[mi]);
                cx.emit(Request::OpenAt { path: root.join(&f.path), line: m.line, cols: m.cols.clone() });
            }
        }
    }

    /// Builds new contents for the given matches (only non-dismissed ones).
    fn plan(&self, only: Option<Row>, cx: &App) -> Result<ReplacePlan, String> {
        let Some((query, opts, _, _)) = self.last_search.clone() else { return Err("Run a search first".into()) };
        let re = Self::matcher(&query, opts)?;
        let replacement = self.replace.read(cx).text();
        let root = self.repo.read(cx).root.clone();
        let mut plan = ReplacePlan { files: Vec::new(), skipped: Vec::new() };
        for (fi, f) in self.results.iter().enumerate() {
            if f.dismissed {
                continue;
            }
            let keep: HashSet<usize> = f
                .matches
                .iter()
                .enumerate()
                .filter(|(mi, m)| !m.dismissed && match only {
                    None => true,
                    Some(Row::File(x)) => x == fi,
                    Some(Row::Match(x, y)) => x == fi && y == *mi,
                })
                .map(|(_, m)| m.offset)
                .collect();
            if keep.is_empty() {
                continue;
            }
            let path = root.join(&f.path);
            if stamp(&path) != f.stamp {
                plan.skipped.push(f.path.clone());
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                plan.skipped.push(f.path.clone());
                continue;
            };
            let (out, count) = replace_kept(&content, &re, &keep, &replacement, opts.regex);
            if count > 0 {
                plan.files.push((path, out, count));
            }
        }
        Ok(plan)
    }

    fn do_replace(&mut self, only: Option<Row>, cx: &mut Context<Self>) {
        match self.plan(only, cx) {
            Ok(plan) if plan.files.is_empty() => cx.emit(Request::Error("Nothing to replace".into())),
            Ok(plan) => {
                self.pending_scope = Some(only);
                cx.emit(SearchEvent::Replace(plan))
            }
            Err(e) => cx.emit(Request::Error(e)),
        }
    }

    pub fn replace_all(&mut self, cx: &mut Context<Self>) {
        self.do_replace(None, cx);
    }

    /// Called by the workspace after a replacement was applied: replaced matches go away,
    /// dismissed ones stay hidden (and untouched).
    pub fn after_replace(&mut self, replaced: &[PathBuf], cx: &mut Context<Self>) {
        let root = self.repo.read(cx).root.clone();
        let scope = self.pending_scope.take().flatten();
        for (fi, f) in self.results.iter_mut().enumerate() {
            if !replaced.contains(&root.join(&f.path)) {
                continue;
            }
            let mut mi = 0;
            f.matches.retain(|m| {
                let in_scope = match scope {
                    None => true,
                    Some(Row::File(x)) => x == fi,
                    Some(Row::Match(x, y)) => x == fi && y == mi,
                };
                mi += 1;
                m.dismissed || !in_scope
            });
            // Offsets are stale now; another replace in this file needs a fresh search.
            f.stamp = None;
            if f.matches.iter().all(|m| m.dismissed) {
                f.dismissed = true;
            }
        }
        self.rebuild_rows();
        cx.notify();
    }

    fn move_sel(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let cur = self.selected.and_then(|s| self.rows.iter().position(|r| *r == s));
        let next = match cur {
            Some(c) => (c as isize + delta).clamp(0, self.rows.len() as isize - 1) as usize,
            None => 0,
        };
        self.selected = Some(self.rows[next]);
        self.scroll.scroll_to_item(next, ScrollStrategy::Top);
        cx.notify();
    }

    fn set_collapsed(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        let fi = match self.selected {
            Some(Row::File(fi)) | Some(Row::Match(fi, _)) => fi,
            None => return,
        };
        self.results[fi].collapsed = collapsed;
        self.selected = Some(Row::File(fi));
        self.rebuild_rows();
        cx.notify();
    }

    fn toggle(&mut self, which: u8, cx: &mut Context<Self>) {
        match which {
            0 => self.opts.case = !self.opts.case,
            1 => self.opts.word = !self.opts.word,
            _ => self.opts.regex = !self.opts.regex,
        }
        self.run_search(cx);
    }

    // ------------------------------------------------------------ rendering

    fn toggle_button(&self, id: &'static str, label: &'static str, on: bool, which: u8, tip: &'static str, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id(id)
            .flex()
            .items_center()
            .justify_center()
            .size(px(20.))
            .rounded(px(3.))
            .text_size(px(10.5))
            .font_family(theme::MONO_FONT)
            .cursor_pointer()
            .when(on, |d| d.bg(theme::ca(0x2489db82)).border_1().border_color(theme::focus_border()).text_color(theme::fg_bright()))
            .when(!on, |d| d.text_color(theme::fg_dim()).hover(|s| s.bg(theme::ca(0x5a5d5e50))))
            .on_click(cx.listener(move |p, _, _, cx| p.toggle(which, cx)))
            .tooltip(ui::tooltip(tip))
            .child(label)
    }

    fn input_box(&self, ed: &Entity<Editor>, window: &Window, cx: &App) -> Div {
        let focused = ed.read(cx).is_focused(window);
        div()
            .flex()
            .flex_1()
            .items_center()
            .min_w_0()
            .h(px(26.))
            .pl(px(4.))
            .pr(px(2.))
            .bg(theme::bg_input())
            .border_1()
            .border_color(if focused { theme::focus_border() } else { theme::border_input() })
            .child(div().flex_1().min_w_0().child(ed.clone()))
    }

    fn render_row(&self, ix: usize, focused: bool, replacement: &str, cx: &mut Context<Self>) -> AnyElement {
        let row = self.rows[ix];
        let selected = self.selected == Some(row);
        let base = div()
            .id(("search-row", ix))
            .group("srow")
            .flex()
            .items_center()
            .h(theme::row_h())
            .pr(px(6.))
            .gap(px(4.))
            .cursor_pointer()
            .when(selected, |d| {
                d.bg(if focused { theme::selected_focused() } else { theme::selected() })
                    .when(focused, |d| d.border_1().border_color(theme::focus_border()))
            })
            .when(!selected, |d| d.hover(|s| s.bg(theme::hover())))
            .on_click(cx.listener(move |p, _, window, cx| {
                window.focus(&p.results_focus);
                p.selected = Some(row);
                if let Row::File(fi) = row {
                    p.results[fi].collapsed = !p.results[fi].collapsed;
                    p.rebuild_rows();
                } else {
                    p.open(row, cx);
                }
                cx.notify();
            }));
        let actions = div()
            .flex()
            .flex_none()
            .gap(px(1.))
            .invisible()
            .group_hover("srow", |s| s.visible())
            .when(self.show_replace, |d| {
                d.child(ui::icon_button(("srow-replace", ix), "check").on_click(cx.listener(move |p, _, _, cx| {
                    cx.stop_propagation();
                    p.do_replace(Some(row), cx);
                })))
            })
            .child(ui::icon_button(("srow-dismiss", ix), "close").on_click(cx.listener(move |p, _, _, cx| {
                cx.stop_propagation();
                p.dismiss(row, cx);
            })));
        match row {
            Row::File(fi) => {
                let f = &self.results[fi];
                let count = f.matches.iter().filter(|m| !m.dismissed).count();
                base.pl(px(6.))
                    .child(ui::small_icon(if f.collapsed { "chevron-right" } else { "chevron-down" }, theme::fg()))
                    .child(ui::file_icon(ui::file_name(&f.path)))
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(6.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(div().text_color(theme::fg_bright()).child(ui::file_name(&f.path).to_string()))
                            .child(div().text_size(px(11.)).text_color(theme::fg_muted()).child(ui::file_dir(&f.path).to_string())),
                    )
                    .child(actions)
                    .child(ui::badge(count.to_string(), theme::badge()))
                    .into_any_element()
            }
            Row::Match(fi, mi) => {
                let m = &self.results[fi].matches[mi];
                let hl = m.preview_hl.clone();
                let (before, matched, after) = (&m.preview[..hl.start], &m.preview[hl.clone()], &m.preview[hl.end..]);
                let show_replace = self.show_replace && !replacement.is_empty();
                let mut text = String::new();
                let mut highlights = Vec::new();
                text.push_str(before);
                let mstart = text.len();
                text.push_str(matched);
                if show_replace {
                    highlights.push((mstart..text.len(), HighlightStyle { background_color: Some(theme::diff_del_char()), strikethrough: Some(StrikethroughStyle { thickness: px(1.), color: Some(theme::fg_dim()) }), ..Default::default() }));
                    let rs = text.len();
                    text.push_str(replacement);
                    highlights.push((rs..text.len(), HighlightStyle { background_color: Some(theme::diff_ins_char()), ..Default::default() }));
                } else {
                    highlights.push((mstart..text.len(), HighlightStyle { background_color: Some(theme::find_match()), color: Some(theme::fg_bright()), ..Default::default() }));
                }
                text.push_str(after);
                base.pl(px(34.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_size(px(11.5))
                            .child(StyledText::new(text).with_highlights(highlights)),
                    )
                    .child(actions)
                    .child(div().flex_none().text_size(px(10.5)).text_color(theme::fg_muted()).child(format!("{}", m.line + 1)))
                    .into_any_element()
            }
        }
    }
}

/// Replaces only the matches whose byte offset is in `keep` (dismissed ones are left alone).
fn replace_kept(content: &str, re: &Regex, keep: &HashSet<usize>, replacement: &str, use_captures: bool) -> (String, usize) {
    let mut out = String::with_capacity(content.len());
    let (mut last, mut count, mut line_start) = (0, 0, 0);
    for line in content.split_inclusive('\n') {
        let body = line.strip_suffix('\n').unwrap_or(line);
        let body = body.strip_suffix('\r').unwrap_or(body);
        for caps in re.captures_iter(body) {
            let m = caps.get(0).unwrap();
            let offset = line_start + m.start();
            if !keep.contains(&offset) || m.start() == m.end() {
                continue;
            }
            out.push_str(&content[last..offset]);
            if use_captures {
                caps.expand(replacement, &mut out);
            } else {
                out.push_str(replacement);
            }
            last = line_start + m.end();
            count += 1;
        }
        line_start += line.len();
    }
    out.push_str(&content[last..]);
    (out, count)
}

fn stamp(path: &Path) -> Option<(u64, SystemTime)> {
    let md = std::fs::metadata(path).ok()?;
    Some((md.len(), md.modified().ok()?))
}

const MAX_MATCHES: usize = 20_000;

fn search_files(root: &Path, files: &[&String], re: &Regex) -> (Vec<FileResult>, bool) {
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(8);
    let chunk = files.len().div_ceil(threads).max(1);
    let mut results: Vec<FileResult> = std::thread::scope(|s| {
        let handles: Vec<_> = files
            .chunks(chunk)
            .map(|part| s.spawn(move || part.iter().filter_map(|f| search_file(root, f, re)).collect::<Vec<_>>()))
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    });
    results.sort_by(|a, b| a.path.cmp(&b.path));
    let mut total = 0;
    let mut truncated = false;
    results.retain(|r| {
        if total >= MAX_MATCHES {
            truncated = true;
            return false;
        }
        total += r.matches.len();
        true
    });
    (results, truncated)
}

fn search_file(root: &Path, rel: &str, re: &Regex) -> Option<FileResult> {
    let path = root.join(rel);
    let md = std::fs::metadata(&path).ok()?;
    if !md.is_file() || md.len() > 5_000_000 {
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    if bytes.iter().take(8000).any(|b| *b == 0) {
        return None;
    }
    let content = String::from_utf8(bytes).ok()?;
    let mut matches = Vec::new();
    let mut line_start = 0;
    for (line_no, line) in content.split_inclusive('\n').enumerate() {
        let body = line.strip_suffix('\n').unwrap_or(line);
        let body = body.strip_suffix('\r').unwrap_or(body);
        for m in re.find_iter(body) {
            if m.start() == m.end() {
                continue;
            }
            let c0 = body[..m.start()].chars().count();
            let c1 = c0 + body[m.start()..m.end()].chars().count();
            // Preview: trim leading whitespace and keep some context around long lines.
            let mut pstart = body.len() - body.trim_start().len();
            if m.start() < pstart || m.start() - pstart > 40 {
                pstart = m.start().saturating_sub(30);
            }
            while !body.is_char_boundary(pstart) {
                pstart -= 1;
            }
            let mut pend = (m.end() + 120).min(body.len());
            while !body.is_char_boundary(pend) {
                pend += 1;
            }
            let prefix = if pstart > 0 && body[..pstart].trim().len() > 0 { "…" } else { "" };
            let preview = format!("{prefix}{}", &body[pstart..pend]);
            let off = prefix.len();
            matches.push(Match {
                line: line_no,
                cols: c0..c1,
                offset: line_start + m.start(),
                preview_hl: off + m.start() - pstart..off + m.end() - pstart,
                preview,
                dismissed: false,
            });
            if matches.len() >= 5000 {
                break;
            }
        }
        line_start += line.len();
    }
    let stamp = md.modified().ok().map(|t| (md.len(), t));
    (!matches.is_empty()).then(|| FileResult { path: rel.to_string(), stamp, matches, collapsed: false, dismissed: false })
}

/// Turns "*.ts, src/**/x, dist" into matchers. A pattern without "/" matches at any depth,
/// and a folder name also matches everything inside it.
fn globs(spec: &str) -> Vec<Regex> {
    spec.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|g| {
            let g = g.trim_start_matches("./").trim_end_matches('/');
            let mut re = String::new();
            let mut chars = g.chars().peekable();
            while let Some(c) = chars.next() {
                match c {
                    '*' if chars.peek() == Some(&'*') => {
                        chars.next();
                        if chars.peek() == Some(&'/') {
                            chars.next();
                            re.push_str("(?:.*/)?");
                        } else {
                            re.push_str(".*");
                        }
                    }
                    '*' => re.push_str("[^/]*"),
                    '?' => re.push_str("[^/]"),
                    '{' => re.push_str("(?:"),
                    '}' => re.push(')'),
                    ',' => re.push('|'),
                    c => re.push_str(&regex::escape(&c.to_string())),
                }
            }
            let anchor = if g.contains('/') { "^" } else { "(?:^|/)" };
            Regex::new(&format!("{anchor}{re}(?:/|$)")).ok()
        })
        .collect()
}

impl Focusable for SearchPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for SearchPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let results_focused = self.results_focus.is_focused(window);
        let (matches, files) = self.counts();
        let replacement = self.replace.read(cx).text();
        let summary = if let Some(e) = &self.error {
            e.clone()
        } else if self.searching {
            "Searching…".into()
        } else if self.last_search.is_some() && matches == 0 {
            "No results found.".into()
        } else if matches > 0 {
            format!("{matches} result{} in {files} file{}{}", if matches == 1 { "" } else { "s" }, if files == 1 { "" } else { "s" }, if self.truncated { " (truncated)" } else { "" })
        } else {
            String::new()
        };
        let rows_count = self.rows.len();

        div()
            .key_context("SearchPanel")
            .track_focus(&self.focus)
            .on_action(cx.listener(|p, _: &ToggleCase, _, cx| p.toggle(0, cx)))
            .on_action(cx.listener(|p, _: &ToggleWord, _, cx| p.toggle(1, cx)))
            .on_action(cx.listener(|p, _: &ToggleRegex, _, cx| p.toggle(2, cx)))
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .h(px(35.))
                    .px(px(20.))
                    .text_size(px(10.5))
                    .child(div().flex_1().child("SEARCH"))
                    .child(ui::icon_button("search-refresh", "refresh").on_click(cx.listener(|p, _, _, cx| p.run_search(cx))))
                    .child(ui::icon_button("search-clear", "close").on_click(cx.listener(|p, _, _, cx| {
                        p.query.update(cx, |q, cx| q.set_text("", cx));
                        p.run_search(cx);
                    })))
                    .child(ui::icon_button("search-collapse", "minus").on_click(cx.listener(|p, _, _, cx| {
                        let collapse = p.results.iter().any(|f| !f.collapsed);
                        for f in &mut p.results {
                            f.collapsed = collapse;
                        }
                        p.rebuild_rows();
                        cx.notify();
                    }))),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .pl(px(4.))
                    .pr(px(12.))
                    .gap(px(2.))
                    .child(
                        div()
                            .id("toggle-replace")
                            .flex()
                            .justify_center()
                            .w(px(18.))
                            .pt(px(5.))
                            .cursor_pointer()
                            .rounded(px(3.))
                            .hover(|s| s.bg(theme::hover()))
                            .on_click(cx.listener(|p, _, _, cx| {
                                p.show_replace = !p.show_replace;
                                cx.notify();
                            }))
                            .child(ui::small_icon(if self.show_replace { "chevron-down" } else { "chevron-right" }, theme::fg())),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(px(4.))
                            .child(
                                self.input_box(&self.query, window, cx)
                                    .child(self.toggle_button("opt-case", "Aa", self.opts.case, 0, "Match Case (⌥⌘C)", cx))
                                    .child(self.toggle_button("opt-word", "ab", self.opts.word, 1, "Match Whole Word (⌥⌘W)", cx))
                                    .child(self.toggle_button("opt-regex", ".*", self.opts.regex, 2, "Use Regular Expression (⌥⌘R)", cx)),
                            )
                            .when(self.show_replace, |d| {
                                d.child(
                                    div().flex().gap(px(2.)).child(self.input_box(&self.replace, window, cx)).child(
                                        ui::icon_button("replace-all", "check")
                                            .tooltip(ui::tooltip("Replace All (Enter in replace box)"))
                                            .on_click(cx.listener(|p, _, _, cx| p.replace_all(cx))),
                                    ),
                                )
                            })
                            .child(
                                div()
                                    .flex()
                                    .justify_end()
                                    .child(ui::icon_button("toggle-filters", "ellipsis").on_click(cx.listener(|p, _, _, cx| {
                                        p.show_filters = !p.show_filters;
                                        cx.notify();
                                    }))),
                            )
                            .when(self.show_filters, |d| {
                                d.child(div().text_size(px(10.5)).text_color(theme::fg_dim()).child("files to include"))
                                    .child(div().flex().child(self.input_box(&self.include, window, cx)))
                                    .child(div().text_size(px(10.5)).text_color(theme::fg_dim()).child("files to exclude"))
                                    .child(div().flex().child(self.input_box(&self.exclude, window, cx)))
                            }),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .px(px(22.))
                    .py(px(6.))
                    .text_size(px(11.))
                    .text_color(if self.error.is_some() { theme::git_deleted() } else { theme::fg_dim() })
                    .child(summary),
            )
            .child(
                div()
                    .key_context("SearchResults")
                    .track_focus(&self.results_focus)
                    .on_action(cx.listener(|p, _: &ResultDown, _, cx| p.move_sel(1, cx)))
                    .on_action(cx.listener(|p, _: &ResultUp, _, cx| p.move_sel(-1, cx)))
                    .on_action(cx.listener(|p, _: &OpenResult, _, cx| {
                        if let Some(r) = p.selected {
                            if let Row::File(fi) = r {
                                p.results[fi].collapsed = !p.results[fi].collapsed;
                                p.rebuild_rows();
                                cx.notify();
                            } else {
                                p.open(r, cx);
                            }
                        }
                    }))
                    .on_action(cx.listener(|p, _: &DismissResult, _, cx| {
                        if let Some(r) = p.selected {
                            p.dismiss(r, cx);
                        }
                    }))
                    .on_action(cx.listener(|p, _: &ReplaceResult, _, cx| {
                        if let Some(r) = p.selected {
                            p.do_replace(Some(r), cx);
                        }
                    }))
                    .on_action(cx.listener(|p, _: &CollapseFile, _, cx| p.set_collapsed(true, cx)))
                    .on_action(cx.listener(|p, _: &ExpandFile, _, cx| p.set_collapsed(false, cx)))
                    .on_action(cx.listener(|p, _: &FocusQuery, window, cx| p.focus_query(window, cx)))
                    .flex_1()
                    .min_h_0()
                    .child(
                        uniform_list(
                            "search-results",
                            rows_count,
                            cx.processor(move |p, range: Range<usize>, _window, cx| {
                                range.map(|ix| p.render_row(ix, results_focused, &replacement, cx)).collect::<Vec<_>>()
                            }),
                        )
                        .track_scroll(self.scroll.clone())
                        .size_full(),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{Options, SearchPanel, globs, replace_kept};
    use std::collections::HashSet;

    #[test]
    fn globs_match_like_vscode() {
        let g = globs("*.ts, dist, src/**/ui");
        assert!(g[0].is_match("src/a/b.ts"));
        assert!(!g[0].is_match("src/a/b.tsx"));
        assert!(g[1].is_match("dist/x.js"));
        assert!(g[1].is_match("pkg/dist/x.js"));
        assert!(g[2].is_match("src/components/ui/a.tsx"));
        assert!(!g[2].is_match("lib/ui/a.tsx"));
    }

    #[test]
    fn word_and_case_options() {
        let re = SearchPanel::matcher("sum", Options { case: false, word: true, regex: false }).unwrap();
        assert!(re.is_match("let SUM = 1"));
        assert!(!re.is_match("summary"));
        let re = SearchPanel::matcher("Sum", Options { case: true, word: false, regex: false }).unwrap();
        assert!(!re.is_match("sum"));
    }

    #[test]
    fn replace_skips_dismissed_matches() {
        let content = "foo bar\nfoo foo\n";
        let re = SearchPanel::matcher("foo", Options::default()).unwrap();
        // Keep the 1st and 3rd match (offsets 0 and 12); the 2nd (8) was dismissed.
        let keep: HashSet<usize> = [0, 12].into_iter().collect();
        let (out, n) = replace_kept(content, &re, &keep, "X", false);
        assert_eq!(out, "X bar\nfoo X\n");
        assert_eq!(n, 2);
        let re = SearchPanel::matcher(r"(\w+) bar", Options { regex: true, ..Default::default() }).unwrap();
        let (out, _) = replace_kept(content, &re, &[0].into_iter().collect(), "$1-baz", true);
        assert_eq!(out, "foo-baz\nfoo foo\n");
    }
}
