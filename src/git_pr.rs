//! Reviewing a pull request in its own tab, as on GitHub: its conversation (Markdown, Mermaid
//! diagrams) with its commits, checks and whether it can merge, and a box to comment; its commits;
//! and Files changed, a tree beside the picked file's diff, where a + on a line comments on it
//! (at once, or kept pending until the review is finished: comment, approve or request changes).

use crate::commit_view::{FileRow, file_rows};
use crate::diff_view::{CommentOn, DiffView};
use crate::editor::{Editor, EditorEvent, EditorMode};
use crate::git::gh::{self, CheckState, Checks, DraftComment, LineComment, PrDetail};
use crate::md_preview::MdPreview;
use crate::git::{self, Change, FileStatus, Rev};
use crate::preview::Markdown;
use crate::repo::Repo;
use crate::request::{DiffSpec, Request};
use crate::ui::px;
use crate::{theme, ui};
use gpui::prelude::*;
use gpui::*;
use std::cell::Cell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub enum PrEvent {
    /// Something changed on GitHub (a review, a merge): lists should refresh.
    Changed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Conversation,
    Commits,
    Files,
}

/// What a pull request (or one of its commits) changed: the two sides and the files, with the
/// tree the sidebar shows (folders first, single-folder chains in one row).
pub struct Files {
    base: String,
    head: String,
    pub list: Vec<Change>,
    pub rows: Vec<FileRow>,
}

impl Files {
    fn new(base: String, head: String, mut list: Vec<Change>, collapsed: &HashSet<String>) -> Self {
        list.sort_by(|a, b| a.path.cmp(&b.path));
        let rows = file_rows(&list, true, collapsed);
        Files { base, head, list, rows }
    }
}

/// The files between `base` and `head` (both here): renames found, nothing else read, so a
/// pull request of tens of thousands of files lists at once.
fn changed_files(root: &Path, base: &str, head: &str) -> anyhow::Result<Vec<Change>> {
    let out = git::run(root, &["diff", "-z", "-M", "--name-status", base, head])?;
    Ok(git::parse_name_status(&out))
}

/// Rows of the file tree (it draws only what's on screen, so 30 000 files scroll freely).
const ROW_H: f32 = 22.;
const INDENT: f32 = 12.;

/// The comment box open under a line of the diff.
#[derive(Clone, PartialEq)]
pub struct LineBox {
    pub path: String,
    pub new_side: bool,
    pub line: usize,
    /// On several lines: the first (Shift-click a second line's +).
    pub start: Option<usize>,
    /// The line clicked first: Shift-click picks from it to another.
    pub anchor: usize,
    /// Answering the thread this comment starts (sent at once).
    pub reply_to: Option<u64>,
}

pub struct PrView {
    focus: FocusHandle,
    pub repo: Entity<Repo>,
    pub number: u64,
    /// The diff's code size (the editors').
    pub font_size: f32,
    pub detail: Option<Result<(PrDetail, Vec<LineComment>), String>>,
    /// The whole pull request's files (its commits fetched here first).
    pub files: Option<Result<Files, String>>,
    /// A commit picked in Commits: the tree shows only what it changed.
    pub commit: Option<String>,
    commit_files: Option<Result<Files, String>>,
    /// Folders closed in the tree.
    collapsed: HashSet<String>,
    pub tab: Tab,
    pub file: Option<String>,
    pub diff: Option<Entity<DiffView>>,
    /// The commit the open diff is from (None: the whole pull request).
    diff_commit: Option<String>,
    /// The review's summary (Finish your review).
    pub input: Entity<Editor>,
    /// Comments on lines kept for the review, until it's sent (kept on disk meanwhile).
    pub pending: Vec<DraftComment>,
    pending_file: Option<PathBuf>,
    pub line_box: Option<LineBox>,
    line_input: Entity<Editor>,
    focus_line: bool,
    /// Finish your review: comment, approve or request-changes.
    pub review_kind: &'static str,
    /// The conversation's box at the bottom, beside its preview.
    comment_input: Entity<Editor>,
    comment_preview: Entity<MdPreview>,
    /// The tree beside the diff: its width (drag its edge), its filter.
    tree_w: f32,
    dragging_tree: bool,
    files_bounds: Rc<Cell<Bounds<Pixels>>>,
    filter: Entity<Editor>,
    /// Files whose path holds the filter's text (by index), for (text, files shown, how many).
    matches: Option<(String, usize, usize, Rc<Vec<usize>>)>,
    tree_scroll: UniformListScrollHandle,
    /// The description, then each comment and review, then each line comment, parsed once.
    md: Vec<Rc<Markdown>>,
    images: Entity<RetainAllImageCache>,
    busy: Option<String>,
    /// The review dropdown is open, hanging from here.
    pub review_at: Option<Point<Pixels>>,
    _load: Option<Task<()>>,
    _commit_load: Option<Task<()>>,
    _act: Option<Task<()>>,
    /// Reads it again while its checks run (or GitHub works out whether it merges).
    _poll: Option<Task<()>>,
    _diff_sub: Option<Subscription>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<Request> for PrView {}
impl EventEmitter<PrEvent> for PrView {}

impl PrView {
    pub fn new(repo: Entity<Repo>, number: u64, cx: &mut Context<Self>) -> Self {
        let editor = |placeholder: &str, single: bool, cx: &mut Context<Self>| {
            let placeholder = placeholder.to_string();
            cx.new(move |cx| {
                let mut ed = Editor::new(if single { EditorMode::SingleLine } else { EditorMode::MultiLine }, cx);
                ed.font_size = 12.;
                ed.placeholder = placeholder.into();
                ed.wrap = !single;
                ed
            })
        };
        let input = editor("Leave a comment (Markdown)", false, cx);
        let line_input = editor("Leave a comment (Markdown)", false, cx);
        let comment_input = editor("Add your comment here… (Markdown)", false, cx);
        let filter = editor("Filter files", true, cx);
        let root = repo.read(cx).root.clone();
        let comment_preview = {
            let (c, r) = (comment_input.clone(), root.clone());
            cx.new(|cx| MdPreview::new(c, r, 160., "pr-comment-input", cx))
        };
        let subs = vec![
            cx.subscribe(&line_input, |v, _, ev: &EditorEvent, cx| match ev {
                EditorEvent::Submit => match &v.line_box {
                    Some(b) if b.reply_to.is_some() => v.single_comment(cx),
                    _ if !v.pending.is_empty() => v.add_pending(cx),
                    _ => v.single_comment(cx),
                },
                EditorEvent::Cancel => v.close_line_box(cx),
                _ => {}
            }),
            cx.subscribe(&comment_input, |v, _, ev: &EditorEvent, cx| {
                if matches!(ev, EditorEvent::Submit) {
                    v.post_comment(cx);
                }
            }),
            cx.subscribe(&input, |v, _, ev: &EditorEvent, cx| {
                if matches!(ev, EditorEvent::Submit) {
                    v.submit_review(cx);
                }
            }),
            cx.subscribe(&filter, |v, _, ev: &EditorEvent, cx| {
                if matches!(ev, EditorEvent::Changed) {
                    v.tree_scroll.scroll_to_item(0, ScrollStrategy::Top);
                    cx.notify();
                }
            }),
        ];
        let images = RetainAllImageCache::new(cx);
        let mut v = Self {
            focus: cx.focus_handle(),
            repo,
            number,
            font_size: 13.,
            detail: None,
            files: None,
            commit: None,
            commit_files: None,
            collapsed: HashSet::new(),
            tab: Tab::Conversation,
            file: None,
            diff: None,
            diff_commit: None,
            input,
            pending: Vec::new(),
            pending_file: None,
            line_box: None,
            line_input,
            focus_line: false,
            review_kind: "comment",
            comment_input,
            comment_preview,
            tree_w: 280.,
            dragging_tree: false,
            files_bounds: Default::default(),
            filter,
            matches: None,
            tree_scroll: UniformListScrollHandle::new(),
            md: Vec::new(),
            images,
            busy: None,
            review_at: None,
            _load: None,
            _commit_load: None,
            _act: None,
            _poll: None,
            _diff_sub: None,
            _subs: subs,
        };
        v.read_pending(root, cx);
        v.load(cx);
        v
    }

    /// The review's pending comments, as left last time (in the repository's git folder).
    fn read_pending(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        let n = self.number;
        let task = cx.background_executor().spawn(async move {
            let file = root.join(git::run(&root, &["rev-parse", "--git-path", &format!("oskl/review-{n}.json")]).ok()?.trim());
            let pending: Vec<DraftComment> = std::fs::read_to_string(&file).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
            Some((file, pending))
        });
        cx.spawn(async move |v, cx| {
            let Some((file, pending)) = task.await else { return };
            v.update(cx, |v, cx| {
                v.pending_file = Some(file);
                if v.pending.is_empty() {
                    v.pending = pending;
                }
                v.refresh_notes(cx);
            })
            .ok();
        })
        .detach();
    }

    fn save_pending(&self) {
        let Some(file) = &self.pending_file else { return };
        if self.pending.is_empty() {
            let _ = std::fs::remove_file(file);
        } else if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
            let _ = std::fs::write(file, serde_json::to_string_pretty(&self.pending).unwrap_or_default());
        }
    }

    pub fn load(&mut self, cx: &mut Context<Self>) {
        let (root, n) = (self.repo.read(cx).root.clone(), self.number);
        let remote = {
            let refs = self.repo.read(cx).refs.clone();
            refs.remotes.iter().map(|r| r.0.clone()).find(|r| r == "origin").or(refs.remotes.first().map(|r| r.0.clone())).unwrap_or_else(|| "origin".into())
        };
        // The page shows as soon as gh answers; the commits (for the files) come after, on their own.
        let view = cx.background_executor().spawn(async move { gh::view(&root, n).map_err(|e| e.to_string()) });
        let root = self.repo.read(cx).root.clone();
        self._load = Some(cx.spawn(async move |v, cx| {
            let detail = view.await;
            let base = detail.as_ref().ok().map(|(d, _)| d.base_ref_name.clone());
            let failed = detail.as_ref().err().cloned();
            v.update(cx, |v, cx| {
                v.got_detail(detail, cx);
                v.files = failed.map(Err);
            })
            .ok();
            let Some(base) = base else { return };
            let found = cx
                .background_executor()
                .spawn(async move {
                    let (base, head) = gh::fetch(&root, &remote, n, &base)?;
                    let mb = git::run(&root, &["merge-base", &base, &head])?.trim().to_string();
                    let head = git::run(&root, &["rev-parse", &head])?.trim().to_string();
                    let list = changed_files(&root, &mb, &head)?;
                    anyhow::Ok((mb, head, list))
                })
                .await
                .map_err(|e| format!("Couldn't bring its commits here: {e}"));
            v.update(cx, |v, cx| {
                v.files = Some(found.map(|(b, h, list)| Files::new(b, h, list, &v.collapsed)));
                cx.notify();
            })
            .ok();
        }));
    }

    /// Reads the page again (comments, checks, whether it merges), not its files.
    pub fn refresh_detail(&mut self, cx: &mut Context<Self>) {
        let (root, n) = (self.repo.read(cx).root.clone(), self.number);
        let view = cx.background_executor().spawn(async move { gh::view(&root, n).map_err(|e| e.to_string()) });
        self._poll = Some(cx.spawn(async move |v, cx| {
            let detail = view.await;
            v.update(cx, |v, cx| {
                // A failed read keeps what was shown.
                if detail.is_ok() {
                    v.got_detail(detail, cx);
                }
            })
            .ok();
        }));
    }

    fn got_detail(&mut self, detail: Result<(PrDetail, Vec<LineComment>), String>, cx: &mut Context<Self>) {
        self.detail = Some(detail);
        self.parse_md(cx);
        self.refresh_notes(cx);
        self.poll_while_working(cx);
        cx.notify();
    }

    /// Checks still running, or GitHub still working out whether it merges: read it again soon.
    fn poll_while_working(&mut self, cx: &mut Context<Self>) {
        let Some(Ok((d, _))) = &self.detail else { return };
        let checks_running = d.status_check_rollup.iter().any(|c| matches!(c.state(), CheckState::Running | CheckState::Queued));
        let undecided = d.mergeable == "UNKNOWN" || d.merge_state_status == "UNKNOWN";
        if d.state != "OPEN" || !(checks_running || undecided) {
            return;
        }
        self._poll = Some(cx.spawn(async move |v, cx| {
            cx.background_executor().timer(std::time::Duration::from_secs(10)).await;
            v.update(cx, |v, cx| v.refresh_detail(cx)).ok();
        }));
    }

    /// The description and comments as Markdown (again once a diagram or picture is ready).
    fn parse_md(&mut self, cx: &mut Context<Self>) {
        let Some(Ok((d, lines))) = &self.detail else { return };
        let mut texts = vec![d.body.clone()];
        texts.extend(d.comments.iter().map(|c| c.body.clone()));
        texts.extend(d.reviews.iter().map(|r| r.body.clone()));
        texts.extend(lines.iter().map(|l| l.body.clone()));
        self.md = texts.iter().map(|t| Rc::new(Markdown::parse_doc(t))).collect();
        Markdown::fetch_waiting(cx, |v: &mut Self, cx| {
            v.parse_md(cx);
            cx.notify();
        });
    }

    /// Where the line comments' Markdown starts in `md`.
    fn first_line_md(&self) -> usize {
        match &self.detail {
            Some(Ok((d, _))) => 1 + d.comments.len() + d.reviews.len(),
            _ => 0,
        }
    }

    /// A commit open in Commits (its files and their diff, a back button to the list).
    fn in_commit(&self) -> bool {
        self.tab == Tab::Commits && self.commit.is_some()
    }

    /// The files the tree shows: the open commit's, else the whole pull request's.
    pub fn shown_files(&self) -> Option<&Result<Files, String>> {
        if self.in_commit() { self.commit_files.as_ref() } else { self.files.as_ref() }
    }

    /// Shows `tab`; a diff open from elsewhere (a commit, or the whole pull request) closes.
    pub fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tab = tab;
        let scope = if self.in_commit() { self.commit.clone() } else { None };
        if matches!(tab, Tab::Files | Tab::Commits) && self.diff.is_some() && self.diff_commit != scope {
            self.diff = None;
            self.file = None;
            self.line_box = None;
        }
        self.matches = None;
        cx.notify();
    }

    pub fn toggle_folder(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.collapsed.remove(key) {
            self.collapsed.insert(key.to_string());
        }
        let collapsed = self.collapsed.clone();
        for f in [&mut self.files, &mut self.commit_files].into_iter().flatten().flat_map(|f| f.as_mut().ok()) {
            f.rows = file_rows(&f.list, true, &collapsed);
        }
        cx.notify();
    }

    /// Opens commit `sha` in Commits: what it changed (None: back to the list).
    pub fn pick_commit(&mut self, sha: Option<String>, cx: &mut Context<Self>) {
        self.commit = sha.clone();
        self.commit_files = None;
        if self.diff_commit.is_some() {
            self.file = None;
            self.diff = None;
            self.diff_commit = None;
        }
        self.matches = None;
        let Some(sha) = sha else { return cx.notify() };
        self.set_tab(Tab::Commits, cx);
        let root = self.repo.read(cx).root.clone();
        let collapsed = self.collapsed.clone();
        let task = cx.background_executor().spawn(async move {
            let parent = git::run(&root, &["rev-parse", &format!("{sha}^")])?.trim().to_string();
            let list = changed_files(&root, &parent, &sha)?;
            anyhow::Ok(Files::new(parent, sha, list, &collapsed))
        });
        self._commit_load = Some(cx.spawn(async move |v, cx| {
            let files = task.await.map_err(|e| format!("Couldn't read this commit: {e}"));
            v.update(cx, |v, cx| {
                v.commit_files = Some(files);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Opens file `ix` of the shown files' diff, in Files changed. On the whole pull request
    /// (not one commit) its lines take comments.
    pub fn open_ix(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(Ok(f)) = self.shown_files() else { return };
        let Some(c) = f.list.get(ix).cloned() else { return };
        let (base, head) = (f.base.clone(), f.head.clone());
        let (root, email) = (self.repo.read(cx).root.clone(), self.repo.read(cx).user_email.clone());
        let left = if c.status == FileStatus::Added { Rev::Empty } else { Rev::Commit(base) };
        let right = if c.status == FileStatus::Deleted { Rev::Empty } else { Rev::Commit(head) };
        let spec = DiffSpec { path: c.path.clone(), old_path: c.orig_path.clone(), left, right, back: None };
        let commentable = !self.in_commit();
        self.diff_commit = if commentable { None } else { self.commit.clone() };
        let (me, path) = (cx.entity().downgrade(), c.path.clone());
        let annotate: crate::diff_view::Annotate = Rc::new(move |old, new, window, cx| me.upgrade()?.update(cx, |v, cx| v.line_thread(&path, old, new, window, cx)));
        let diff = cx.new(|cx| {
            let mut d = DiffView::new(root, spec, email, false, 12., cx);
            d.commentable = commentable;
            d.annotate = commentable.then_some(annotate);
            d
        });
        let path = c.path.clone();
        self._diff_sub = Some(cx.subscribe(&diff, move |v, _, ev: &CommentOn, cx| v.comment_on(&path, ev, cx)));
        if self.line_box.as_ref().is_some_and(|b| b.path != c.path) {
            self.line_box = None;
        }
        self.file = Some(c.path.clone());
        self.diff = Some(diff);
        cx.notify();
    }

    /// Opens the file of a pending comment.
    fn open_path(&mut self, path: &str, cx: &mut Context<Self>) {
        self.set_tab(Tab::Files, cx);
        let ix = match self.shown_files() {
            Some(Ok(f)) => f.list.iter().position(|c| c.path == path),
            _ => None,
        };
        if let Some(ix) = ix {
            self.review_at = None;
            self.open_ix(ix, cx);
        }
    }

    /// Line comments on `path` (sent and pending).
    pub fn comments_on(&self, path: &str) -> usize {
        let sent = match &self.detail {
            Some(Ok((_, lines))) => lines.iter().filter(|l| l.path == path).count(),
            _ => 0,
        };
        sent + self.pending.iter().filter(|p| p.path == path).count()
    }

    /// The picked commit's first line.
    pub fn commit_label(&self) -> Option<String> {
        let sha = self.commit.as_ref()?;
        let Some(Ok((d, _))) = &self.detail else { return Some(sha[..7.min(sha.len())].to_string()) };
        d.commits.iter().find(|c| &c.oid == sha).map(|c| format!("{} {}", &c.oid[..7.min(c.oid.len())], c.message_headline))
    }

    /// The diff draws what's under its lines again.
    fn refresh_notes(&mut self, cx: &mut Context<Self>) {
        let marked = self.line_box.as_ref().filter(|b| Some(&b.path) == self.file.as_ref()).map(|b| (b.new_side, b.start.unwrap_or(b.line), b.line));
        if let Some(d) = &self.diff {
            d.update(cx, |d, cx| {
                d.marked = marked;
                d.remeasure(cx);
            });
        }
        cx.notify();
    }

    /// The + on a line: a box under it, or with Shift (a box open on that side) the lines
    /// from the first picked to this one.
    fn comment_on(&mut self, path: &str, ev: &CommentOn, cx: &mut Context<Self>) {
        if ev.2
            && let Some(b) = &mut self.line_box
            && b.path == path
            && b.new_side == ev.0
            && b.reply_to.is_none()
        {
            let (a, z) = (b.anchor.min(ev.1), b.anchor.max(ev.1));
            b.start = (a < z).then_some(a);
            b.line = z;
            self.focus_line = true;
            return self.refresh_notes(cx);
        }
        self.open_line_box(LineBox { path: path.to_string(), new_side: ev.0, line: ev.1, start: None, anchor: ev.1, reply_to: None }, cx);
    }

    pub fn open_line_box(&mut self, b: LineBox, cx: &mut Context<Self>) {
        if self.line_box.as_ref() != Some(&b) {
            self.line_input.update(cx, |e, cx| e.set_text("", cx));
        }
        self.line_box = Some(b);
        self.focus_line = true;
        self.refresh_notes(cx);
    }

    fn close_line_box(&mut self, cx: &mut Context<Self>) {
        self.line_box = None;
        self.line_input.update(cx, |e, cx| e.set_text("", cx));
        self.refresh_notes(cx);
    }

    /// The open box's comment, kept for the review (Start a review / Add review comment).
    pub fn add_pending(&mut self, cx: &mut Context<Self>) {
        let body = self.line_input.read(cx).text();
        let Some(b) = self.line_box.clone() else { return };
        if body.trim().is_empty() {
            return cx.emit(Request::Error("Write the comment first".into()));
        }
        self.pending.push(DraftComment { path: b.path, line: b.line, new_side: b.new_side, body, start_line: b.start });
        self.save_pending();
        self.close_line_box(cx);
    }

    fn remove_pending(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.pending.len() {
            self.pending.remove(ix);
            self.save_pending();
        }
        self.refresh_notes(cx);
    }

    /// The open box's comment, sent at once (Add single comment, or Reply in a thread).
    pub fn single_comment(&mut self, cx: &mut Context<Self>) {
        let body = self.line_input.read(cx).text();
        let (Some(b), Some(Ok((d, _)))) = (self.line_box.clone(), &self.detail) else { return };
        if body.trim().is_empty() {
            return cx.emit(Request::Error("Write the comment first".into()));
        }
        let (n, commit) = (self.number, d.head_ref_oid.clone());
        let label = if b.reply_to.is_some() { "Reply".to_string() } else { format!("Comment on {}", lines_label(b.start, b.line)) };
        self.act(
            &label,
            move |root| match b.reply_to {
                Some(to) => gh::reply(root, n, to, &body),
                None => gh::line_comment(root, n, &commit, &b.path, b.start, b.line, b.new_side, &body),
            },
            |v, cx| v.close_line_box(cx),
            cx,
        );
    }

    /// Finish your review: the pending comments and the summary, as a comment, an approval or
    /// changes requested.
    pub fn submit_review(&mut self, cx: &mut Context<Self>) {
        let (n, body, kind) = (self.number, self.input.read(cx).text(), self.review_kind);
        let Some(Ok((d, _))) = &self.detail else { return };
        let commit = d.head_ref_oid.clone();
        let pending = self.pending.clone();
        if kind != "approve" && pending.is_empty() && body.trim().is_empty() {
            return cx.emit(Request::Error(if kind == "comment" { "Write the comment first".into() } else { "Say what should change first".into() }));
        }
        let label = match kind {
            "approve" => "Approve",
            "request-changes" => "Request changes",
            _ => "Review",
        };
        let sent = pending.len();
        self.act(
            label,
            move |root| {
                if pending.is_empty() {
                    gh::review(root, n, kind, &body)
                } else {
                    let event = match kind {
                        "approve" => "APPROVE",
                        "request-changes" => "REQUEST_CHANGES",
                        _ => "COMMENT",
                    };
                    gh::submit_review(root, n, &commit, event, &body, &pending)
                }
            },
            move |v, cx| {
                v.pending.drain(..sent.min(v.pending.len()));
                v.save_pending();
                v.review_at = None;
                v.review_kind = "comment";
                v.input.update(cx, |e, cx| e.set_text("", cx));
                v.refresh_notes(cx);
            },
            cx,
        );
    }

    /// The conversation's box: a comment on the whole pull request.
    pub fn post_comment(&mut self, cx: &mut Context<Self>) {
        let (n, body) = (self.number, self.comment_input.read(cx).text());
        if body.trim().is_empty() {
            return cx.emit(Request::Error("Write the comment first".into()));
        }
        self.act("Comment", move |root| gh::comment(root, n, &body), |v, cx| v.comment_input.update(cx, |e, cx| e.set_text("", cx)), cx);
    }

    /// Runs a `gh` action in the background; once done, `ok`, the page read again and the lists told.
    fn act(&mut self, label: &str, op: impl FnOnce(&std::path::Path) -> anyhow::Result<String> + Send + 'static, ok: impl FnOnce(&mut Self, &mut Context<Self>) + 'static, cx: &mut Context<Self>) {
        let root = self.repo.read(cx).root.clone();
        self.busy = Some(label.to_string());
        let label = label.to_string();
        let task = cx.background_executor().spawn(async move { op(&root) });
        self._act = Some(cx.spawn(async move |v, cx| {
            let result = task.await;
            v.update(cx, |v, cx| {
                v.busy = None;
                match result {
                    Ok(_) => {
                        cx.emit(Request::Message(format!("{label}: done")));
                        ok(v, cx);
                        cx.emit(PrEvent::Changed);
                        v.refresh_detail(cx);
                    }
                    Err(e) => cx.emit(Request::Error(format!("{label} failed: {e}"))),
                }
                v.repo.update(cx, |r, cx| r.refresh_all(cx));
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn ask_merge(&mut self, cx: &mut Context<Self>) {
        let me = cx.entity().downgrade();
        let n = self.number;
        cx.emit(Request::Ask(crate::dialog::Ask {
            title: format!("Merge pull request #{n}?"),
            detail: "On GitHub, into its base branch.".into(),
            buttons: vec!["Merge".into(), "Squash and Merge".into(), "Rebase and Merge".into()],
            danger: false,
            cancel_default: false,
            cancel_label: None,
            on_answer: Rc::new(move |ix, _, cx| {
                let how = ["merge", "squash", "rebase"][ix.min(2)];
                me.update(cx, |v, cx| v.act("Merge", move |root| gh::merge(root, n, how), |v, cx| v.load(cx), cx)).ok();
            }),
        }));
    }

    /// One row: state, title, the size; Check out, GitHub.
    fn header(&self, d: &PrDetail, cx: &mut Context<Self>) -> impl IntoElement {
        let (state, color) = state_label(d);
        let button = |id: &'static str, icon: &'static str, tip: &'static str| ui::icon_button(id, icon).debug_selector(move || id.to_string()).tooltip(ui::tooltip(tip));
        let url = d.url.clone();
        let tip = format!("{} wants to merge {} into {}", d.author.login, d.head_ref_name, d.base_ref_name);
        let check = match gh::checks(&d.status_check_rollup) {
            Checks::Passing => Some(("✓", theme::git_added())),
            Checks::Failing => Some(("✗", theme::git_deleted())),
            Checks::Pending => Some(("●", theme::git_modified())),
            Checks::None => None,
        };
        div()
            .id("pr-header")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .h(px(34.))
            .pl(px(12.))
            .pr(px(6.))
            .border_b_1()
            .border_color(theme::border())
            .whitespace_nowrap()
            .tooltip(ui::tooltip_text(tip))
            .child(div().flex_none().px(px(7.)).h(px(18.)).flex().items_center().rounded(px(9.)).border_1().border_color(color).text_color(color).text_size(px(10.5)).font_weight(FontWeight::SEMIBOLD).child(state))
            .child(div().min_w_0().overflow_hidden().text_ellipsis().text_color(theme::fg_bright()).font_weight(FontWeight::SEMIBOLD).text_size(px(13.)).child(format!("{} #{}", d.title, d.number)))
            .child(div().flex_none().text_color(theme::fg_muted()).child(format!("{} → {}", d.head_ref_name, d.base_ref_name)))
            .child(div().flex_none().text_color(theme::git_added()).child(format!("+{}", d.additions)))
            .child(div().flex_none().text_color(theme::git_deleted()).child(format!("−{}", d.deletions)))
            .when_some(check, |el, (c, color)| el.child(div().flex_none().text_color(color).child(c)))
            .child(div().flex_1())
            .child(button("pr-checkout", "arrow-down", "Check out this pull request's branch").on_click(cx.listener(|v, _, _, cx| {
                let n = v.number;
                v.act("Check out", move |root| gh::checkout(root, n), |_, _| {}, cx);
            })))
            .child(button("pr-browser", "globe", "Open on GitHub").on_click(cx.listener(move |_, _, _, _| open_url(&url))))
    }

    /// Who did what, when: a card with its Markdown.
    fn card(&self, root: &Path, who: String, did: String, when: String, excerpt: Option<Div>, body: Option<Rc<Markdown>>) -> Div {
        let color = if did == "approved" { theme::git_added() } else if did == "requested changes" { theme::git_deleted() } else { theme::fg_dim() };
        let opener: crate::preview::Opener = Rc::new(|path, _, _, _| {
            let _ = std::process::Command::new("open").arg(path).spawn();
        });
        div()
            .flex()
            .flex_col()
            .rounded(px(8.))
            .border_1()
            .border_color(theme::border())
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(12.))
                    .h(px(32.))
                    .bg(theme::bg_side())
                    .border_b_1()
                    .border_color(theme::border())
                    .child(div().text_color(theme::fg_bright()).font_weight(FontWeight::SEMIBOLD).child(who))
                    .child(div().text_color(color).child(did))
                    .child(div().text_color(theme::fg_muted()).child(when)),
            )
            .children(excerpt)
            .when_some(body, |d, md| d.child(div().px(px(14.)).py(px(10.)).child(md.render(root, &self.images, opener))))
    }

    fn render_conversation(&self, root: &Path, d: &PrDetail, lines: &[LineComment], cx: &mut Context<Self>) -> impl IntoElement {
        let md = |ix: usize| self.md.get(ix).cloned();
        enum Item {
            Card(String, String, Option<Rc<Markdown>>, Option<Div>),
            Commit(String, String, String),
        }
        // Comments, reviews, comments on lines and commits, oldest first.
        let mut items: Vec<(String, Item)> = d.comments.iter().enumerate().map(|(i, c)| (c.created_at.clone(), Item::Card(c.author.login.clone(), "commented".into(), md(1 + i), None))).collect();
        let n_comments = d.comments.len();
        items.extend(d.reviews.iter().enumerate().map(|(i, r)| {
            let did = match r.state.as_str() {
                "APPROVED" => "approved",
                "CHANGES_REQUESTED" => "requested changes",
                "DISMISSED" => "had a review dismissed",
                _ => "reviewed",
            };
            (r.submitted_at.clone(), Item::Card(r.author.login.clone(), did.into(), md(1 + n_comments + i).filter(|_| !r.body.trim().is_empty()), None))
        }));
        let first_line = self.first_line_md();
        items.extend(lines.iter().enumerate().map(|(i, l)| (l.created_at.clone(), Item::Card(l.user.login.clone(), format!("commented on {} {}", l.path, lines_label(l.start_line.map(|s| s as usize), l.line.or(l.original_line).unwrap_or(0) as usize)), md(first_line + i), hunk_excerpt(l)))));
        items.extend(d.commits.iter().map(|c| (c.committed_date.clone(), Item::Commit(c.oid.clone(), c.message_headline.clone(), c.authors.first().map(|a| a.login.clone()).unwrap_or_default()))));
        items.sort_by(|a, b| a.0.cmp(&b.0));
        let ago = |when: &str| crate::pr_panel::github_time(when).map(crate::util::relative_time).unwrap_or_default();
        let description = md(0).filter(|_| !d.body.trim().is_empty()).or_else(|| Some(Rc::new(Markdown::parse_doc("*No description provided.*"))));
        let mut timeline: Vec<AnyElement> = Vec::new();
        for (when, item) in items {
            timeline.push(match item {
                Item::Card(who, did, body, excerpt) => self.card(root, who, did, ago(&when), excerpt, body).into_any_element(),
                Item::Commit(sha, headline, who) => {
                    let short = sha.get(..7).unwrap_or(&sha).to_string();
                    let sel = format!("pr-timeline-commit-{short}");
                    div()
                        .id(SharedString::from(sel.clone()))
                        .debug_selector(move || sel)
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .pl(px(14.))
                        .pr(px(6.))
                        .h(px(26.))
                        .rounded(px(4.))
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .tooltip(ui::tooltip("Show what this commit changed"))
                        .on_click(cx.listener(move |v, _, _, cx| v.pick_commit(Some(sha.clone()), cx)))
                        .child(ui::small_icon("git", theme::fg_muted()))
                        .child(div().flex_none().text_color(theme::fg_muted()).child(who))
                        .child(div().min_w_0().flex_1().overflow_hidden().text_ellipsis().whitespace_nowrap().text_color(theme::fg()).child(headline))
                        .child(div().flex_none().font_family(theme::MONO_FONT).text_size(px(11.)).text_color(theme::fg_dim()).child(short))
                        .into_any_element()
                }
            });
        }
        let busy = self.busy.clone();
        div()
            .id("pr-conversation")
            .debug_selector(|| "pr-conversation".into())
            .size_full()
            .overflow_y_scroll()
            // Centred, up to a readable width; a narrow pane uses all of it.
            .flex()
            .flex_col()
            .items_center()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .p(px(16.))
                    .w_full()
                    .max_w(px(1200.))
                    .child(self.card(root, d.author.login.clone(), "opened it".into(), String::new(), None, description))
                    .children(timeline)
                    .child(self.render_merge_box(d, cx))
                    // Add a comment: written beside its preview.
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .pt(px(6.))
                            .child(div().text_color(theme::fg_bright()).font_weight(FontWeight::SEMIBOLD).text_size(px(13.)).child("Add a comment"))
                            .child(self.comment_preview.clone())
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.))
                                    .child(div().flex_1().text_size(px(11.)).text_color(theme::fg_muted()).child(busy.map(|b| format!("{b}…")).unwrap_or_else(|| "⌘Enter to send".into())))
                                    .child(action_button("pr-comment-post", "Comment", Some(theme::git_added())).on_click(cx.listener(|v, _, _, cx| v.post_comment(cx)))),
                            ),
                    ),
            )
    }

    /// Like GitHub's box above the comment: reviews, each check (a click opens its log in the
    /// browser), whether it can merge, and Merge.
    fn render_merge_box(&self, d: &PrDetail, cx: &mut Context<Self>) -> impl IntoElement {
        let section = |icon: AnyElement, title: String, sub: String, color: Hsla| {
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .px(px(14.))
                .py(px(10.))
                .border_b_1()
                .border_color(theme::border())
                .child(div().flex_none().size(px(26.)).rounded(px(13.)).flex().items_center().justify_center().bg(color.opacity(0.18)).text_color(color).child(icon))
                .child(div().flex().flex_col().min_w_0().child(div().text_color(theme::fg_bright()).font_weight(FontWeight::SEMIBOLD).child(title)).when(!sub.is_empty(), |el| el.child(div().text_size(px(11.)).text_color(theme::fg_muted()).child(sub))))
        };
        let text = |s: &'static str| div().font_weight(FontWeight::BOLD).child(s).into_any_element();
        let mut card = div().id("pr-merge-box").debug_selector(|| "pr-merge-box".into()).flex().flex_col().rounded(px(8.)).border_1().border_color(theme::border()).overflow_hidden();

        if d.state == "OPEN" {
            // Reviews.
            let (title, sub, color, icon) = match d.review_decision.as_deref() {
                Some("APPROVED") => ("Changes approved", "A reviewer approved these changes.", theme::git_added(), "✓"),
                Some("CHANGES_REQUESTED") => ("Changes requested", "A reviewer asked for changes before this merges.", theme::git_deleted(), "✗"),
                Some("REVIEW_REQUIRED") => ("Review required", "At least one approving review is required to merge.", theme::git_modified(), "●"),
                _ => ("No reviews yet", "", theme::fg_muted(), "○"),
            };
            card = card.child(section(text(icon), title.into(), sub.into(), color));

            // Checks, each with where it stands and a link to its run.
            let list = &d.status_check_rollup;
            if !list.is_empty() {
                let count = |s: CheckState| list.iter().filter(|c| c.state() == s).count();
                let (failing, running, queued, passed, skipped) = (count(CheckState::Failing), count(CheckState::Running), count(CheckState::Queued), count(CheckState::Passed), count(CheckState::Skipped));
                let (title, color) = if failing > 0 {
                    ("Some checks were not successful", theme::git_deleted())
                } else if running + queued > 0 {
                    ("Some checks haven't completed yet", theme::git_modified())
                } else {
                    ("All checks have passed", theme::git_added())
                };
                let sub = [(failing, "failing"), (running, "in progress"), (queued, "queued"), (passed, "successful"), (skipped, "skipped")]
                    .into_iter()
                    .filter(|(n, _)| *n > 0)
                    .map(|(n, what)| format!("{n} {what}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let icon = if failing > 0 {
                    text("✗")
                } else if running + queued > 0 {
                    crate::pr_panel::spinner("pr-checks-spin", 13.).into_any_element()
                } else {
                    text("✓")
                };
                card = card.child(section(icon, title.into(), format!("{sub} {}", if list.len() == 1 { "check" } else { "checks" }), color));
                let mut sorted: Vec<&gh::Check> = list.iter().collect();
                sorted.sort_by_key(|c| c.state());
                card = card.child(div().id("pr-checks").flex().flex_col().max_h(px(260.)).overflow_y_scroll().border_b_1().border_color(theme::border()).bg(theme::bg_side()).children(sorted.into_iter().enumerate().map(|(i, c)| check_row(i, c))));
            }

            // Whether it merges.
            let (title, sub, color, icon) = if d.is_draft || d.merge_state_status == "DRAFT" {
                ("This pull request is still a work in progress", "Draft pull requests can't be merged.", theme::fg_muted(), "◌")
            } else if d.mergeable == "CONFLICTING" || d.merge_state_status == "DIRTY" {
                ("This branch has conflicts that must be resolved", "Resolve them on GitHub or here, then push.", theme::git_deleted(), "!")
            } else if d.mergeable == "UNKNOWN" || d.merge_state_status == "UNKNOWN" {
                ("Checking for the ability to merge automatically…", "", theme::fg_muted(), "…")
            } else {
                match d.merge_state_status.as_str() {
                    "BLOCKED" => ("Merging is blocked", "The base branch's rules aren't met yet (reviews or checks).", theme::git_deleted(), "✗"),
                    "BEHIND" => ("This branch is out-of-date with the base branch", "Bring in the latest changes from the base branch first.", theme::git_modified(), "!"),
                    "UNSTABLE" => ("Ready to merge, though not every check passed", "This branch has no conflicts with the base branch.", theme::git_modified(), "✓"),
                    _ => ("Ready to merge", "This branch has no conflicts with the base branch.", theme::git_added(), "✓"),
                }
            };
            let can = !(d.is_draft || d.mergeable == "CONFLICTING" || d.merge_state_status == "DIRTY");
            card = card.child(section(text(icon), title.into(), sub.into(), color).id("pr-merge-state").debug_selector(|| "pr-merge-state".into())).child(
                div().flex().items_center().gap(px(8.)).px(px(14.)).py(px(10.)).child(
                    action_button("pr-merge", "Merge pull request…", can.then(theme::git_added))
                        .when(!can, |b| b.opacity(0.6))
                        .tooltip(ui::tooltip("Merge on GitHub, into its base branch"))
                        .on_click(cx.listener(|v, _, _, cx| v.ask_merge(cx))),
                ),
            );
        } else {
            let (title, sub, color) = if d.state == "MERGED" {
                ("Pull request successfully merged and closed", format!("{} is in {}.", d.head_ref_name, d.base_ref_name), theme::c(0xc678dd))
            } else {
                ("Closed with unmerged commits", format!("{} wasn't merged into {}.", d.head_ref_name, d.base_ref_name), theme::git_deleted())
            };
            card = card.child(section(ui::small_icon("pull-request", color).into_any_element(), title.into(), sub, color).id("pr-merge-state").debug_selector(|| "pr-merge-state".into()));
        }
        card
    }

    fn render_commits(&self, d: &PrDetail, cx: &mut Context<Self>) -> impl IntoElement {
        let picked = self.commit.clone();
        div()
            .id("pr-commits")
            .debug_selector(|| "pr-commits".into())
            .size_full()
            .overflow_y_scroll()
            .py(px(6.))
            .when(d.commits.is_empty(), |el| el.child(ui::empty_state("git", "No commits", "")))
            .children(d.commits.iter().map(|c| {
                let sha = c.oid.clone();
                let on = picked.as_deref() == Some(&sha);
                let short = sha.get(..7).unwrap_or(&sha).to_string();
                let who = c.authors.first().map(|a| a.login.clone()).unwrap_or_default();
                let when = crate::pr_panel::github_time(&c.committed_date).map(crate::util::relative_time).unwrap_or_default();
                div()
                    .id(SharedString::from(format!("pr-commit-{short}")))
                    .debug_selector(move || format!("pr-commit-{short}"))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .px(px(16.))
                    .py(px(6.))
                    .cursor_pointer()
                    .when(on, |d| d.bg(theme::selected()))
                    .when(!on, |d| d.hover(|s| s.bg(theme::hover())))
                    .on_click(cx.listener(move |v, _, _, cx| {
                        let sha = sha.clone();
                        let again = v.commit.as_deref() == Some(&sha);
                        v.pick_commit((!again).then_some(sha), cx);
                    }))
                    .child(ui::small_icon("git", theme::fg_muted()))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(div().overflow_hidden().text_ellipsis().whitespace_nowrap().text_color(theme::fg_bright()).child(c.message_headline.clone()))
                            .child(div().text_size(px(11.)).text_color(theme::fg_muted()).child(format!("{who} · {when}"))),
                    )
                    .child(div().flex_none().font_family(theme::MONO_FONT).text_size(px(11.)).text_color(theme::fg_dim()).child(c.oid.get(..7).unwrap_or("").to_string()))
            }))
    }

    /// The files whose path holds `q` (any case), by index; kept until the text or files change.
    fn matching(&mut self, q: &str) -> Option<Rc<Vec<usize>>> {
        let Some(Ok(f)) = self.shown_files() else { return None };
        let key = (q.to_string(), f.list.as_ptr() as usize, f.list.len());
        if let Some((t, ptr, len, m)) = &self.matches
            && (t, ptr, len) == (&key.0, &key.1, &key.2)
        {
            return Some(m.clone());
        }
        let q = q.to_lowercase();
        let m: Rc<Vec<usize>> = Rc::new(f.list.iter().enumerate().filter(|(_, c)| c.path.to_lowercase().contains(&q)).map(|(i, _)| i).collect());
        self.matches = Some((key.0, key.1, key.2, m.clone()));
        Some(m)
    }

    /// Files changed, beside the diff: a tree (folders first) or, filtered, the matching files.
    fn render_tree(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let q = self.filter.read(cx).text();
        let matched = if q.trim().is_empty() { None } else { self.matching(q.trim()) };
        let scope = if self.in_commit() { self.commit_label() } else { None };
        let (count, body): (usize, Option<AnyElement>) = match self.shown_files() {
            None => (0, Some(div().px(px(12.)).py(px(8.)).text_color(theme::fg_muted()).child("Bringing its commits here…").into_any_element())),
            Some(Err(e)) => (0, Some(div().px(px(12.)).py(px(8.)).text_color(theme::git_deleted()).child(e.clone()).into_any_element())),
            Some(Ok(f)) => (matched.as_ref().map_or(f.rows.len(), |m| m.len()), None),
        };
        let files_n = match self.shown_files() {
            Some(Ok(f)) => f.list.len(),
            _ => 0,
        };
        let body = body.unwrap_or_else(|| {
            let matched = matched.clone();
            uniform_list(
                "pr-tree",
                count,
                cx.processor(move |v: &mut Self, range: std::ops::Range<usize>, _, cx| {
                    let Some(Ok(f)) = v.shown_files() else { return Vec::new() };
                    let current = v.file.clone();
                    range
                        .map(|i| {
                            let row = match &matched {
                                Some(m) => FileRow::File { ix: m[i], depth: 0 },
                                None => f.rows[i].clone(),
                            };
                            match row {
                                FileRow::Folder { key, label, depth, open } => {
                                    let k = key.clone();
                                    div()
                                        .id(SharedString::from(format!("pr-dir-{key}")))
                                        .debug_selector(move || format!("pr-dir-{key}"))
                                        .flex()
                                        .items_center()
                                        .gap(px(4.))
                                        .h(px(ROW_H))
                                        .pl(px(10. + depth as f32 * INDENT))
                                        .pr(px(8.))
                                        .cursor_pointer()
                                        .hover(|s| s.bg(theme::hover()))
                                        .whitespace_nowrap()
                                        .on_click(cx.listener(move |v, _, _, cx| v.toggle_folder(&k, cx)))
                                        .child(ui::small_icon(if open { "chevron-down" } else { "chevron-right" }, theme::fg_muted()))
                                        .child(ui::folder_icon(&label, open).size(px(14.)).flex_none())
                                        .child(div().min_w_0().overflow_hidden().text_ellipsis().text_color(theme::fg()).child(label))
                                        .into_any_element()
                                }
                                FileRow::File { ix, depth } => {
                                    let c = &f.list[ix];
                                    let on = current.as_deref() == Some(c.path.as_str());
                                    let notes = v.comments_on(&c.path);
                                    let color = ui::status_color(c.status);
                                    let sel = format!("pr-file-{}", c.path);
                                    let flat = matched.is_some();
                                    div()
                                        .id(SharedString::from(sel.clone()))
                                        .debug_selector(move || sel)
                                        .flex()
                                        .items_center()
                                        .gap(px(5.))
                                        .h(px(ROW_H))
                                        .pl(px(10. + depth as f32 * INDENT + if flat { 0. } else { 16. }))
                                        .pr(px(8.))
                                        .cursor_pointer()
                                        .when(on, |d| d.bg(theme::selected()))
                                        .when(!on, |d| d.hover(|s| s.bg(theme::hover())))
                                        .whitespace_nowrap()
                                        .tooltip(ui::tooltip_text(c.path.clone()))
                                        .on_click(cx.listener(move |v, _, _, cx| v.open_ix(ix, cx)))
                                        .child(ui::file_icon(ui::file_name(&c.path)).size(px(14.)).flex_none())
                                        .child(div().flex_none().max_w(relative(0.75)).overflow_hidden().text_ellipsis().text_color(color).when(c.status == FileStatus::Deleted, |d| d.line_through()).child(ui::file_name(&c.path).to_string()))
                                        .when(flat, |d| d.child(div().min_w_0().overflow_hidden().text_ellipsis().text_size(px(10.5)).text_color(theme::fg_muted()).child(ui::file_dir(&c.path).to_string())))
                                        .child(div().flex_1())
                                        .when(notes > 0, |d| d.child(div().flex_none().text_size(px(10.)).text_color(theme::accent()).child(format!("💬{notes}"))))
                                        .child(div().flex_none().w(px(12.)).text_center().text_size(px(10.5)).font_family(theme::MONO_FONT).font_weight(FontWeight::SEMIBOLD).text_color(color).child(c.status.letter()))
                                        .into_any_element()
                                }
                            }
                        })
                        .collect()
                }),
            )
            .track_scroll(&self.tree_scroll)
            .flex_1()
            .into_any_element()
        });
        div()
            .id("pr-tree-panel")
            .debug_selector(|| "pr-tree-panel".into())
            .flex()
            .flex_col()
            .flex_none()
            .w(px(self.tree_w))
            .h_full()
            .bg(theme::bg_side())
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(6.))
                    .px(px(12.))
                    .h(px(30.))
                    .text_size(px(11.))
                    .text_color(theme::fg_muted())
                    .child(div().min_w_0().flex_1().overflow_hidden().text_ellipsis().whitespace_nowrap().child(match &scope {
                        Some(c) => format!("{files_n} files in {c}"),
                        None => format!("{files_n} files changed"),
                    }))
                    ,
            )
            .child(div().id("pr-files-filter").debug_selector(|| "pr-files-filter".into()).flex_none().mx(px(10.)).mb(px(6.)).h(px(24.)).px(px(6.)).flex().items_center().rounded(px(4.)).bg(theme::bg_input()).border_1().border_color(theme::border_input()).overflow_hidden().child(self.filter.clone()))
            .child(div().flex_none().h(px(1.)).bg(theme::border()))
            .child(div().flex().flex_col().flex_1().min_h_0().pt(px(4.)).child(body))
            .into_any_element()
    }

    /// Under a line of the diff: its comments (sent, then pending) and the box, if open there.
    fn line_thread(&mut self, path: &str, old: Option<usize>, new: Option<usize>, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let on = |new_side: bool, line: usize| if new_side { new == Some(line) } else { old == Some(line) };
        let sent: Vec<(usize, LineComment)> = match &self.detail {
            Some(Ok((_, lines))) => lines.iter().enumerate().filter(|(_, l)| l.path == path && l.line.is_some_and(|n| on(l.side.as_deref() != Some("LEFT"), n as usize))).map(|(i, l)| (i, l.clone())).collect(),
            _ => Vec::new(),
        };
        let pending: Vec<(usize, DraftComment)> = self.pending.iter().enumerate().filter(|(_, p)| p.path == path && on(p.new_side, p.line)).map(|(i, p)| (i, p.clone())).collect();
        let open = self.line_box.clone().filter(|b| b.path == path && on(b.new_side, b.line));
        if sent.is_empty() && pending.is_empty() && open.is_none() {
            return None;
        }
        if open.is_some() && self.focus_line {
            self.focus_line = false;
            window.focus(&self.line_input.focus_handle(cx), cx);
        }
        let root = self.repo.read(cx).root.clone();
        let first = self.first_line_md();
        let opener: crate::preview::Opener = Rc::new(|path, _, _, _| {
            let _ = std::process::Command::new("open").arg(path).spawn();
        });
        let comment = |who: String, badge: Option<&'static str>, lines: Option<String>, md: Option<Rc<Markdown>>, del: Option<AnyElement>| {
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .px(px(12.))
                .py(px(8.))
                .border_b_1()
                .border_color(theme::border())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(div().text_color(theme::fg_bright()).font_weight(FontWeight::SEMIBOLD).child(who))
                        .when_some(badge, |d, b| d.child(div().px(px(6.)).rounded(px(8.)).border_1().border_color(theme::git_modified()).text_color(theme::git_modified()).text_size(px(10.)).child(b)))
                        .when_some(lines, |d, l| d.child(div().text_size(px(11.)).text_color(theme::fg_muted()).child(format!("on {l}"))))
                        .child(div().flex_1())
                        .children(del),
                )
                .when_some(md, |d, md| d.child(md.render(&root, &self.images, opener.clone())))
        };
        // The thread a reply goes to: its first comment.
        let thread = sent.first().map(|(_, l)| l.in_reply_to_id.unwrap_or(l.id)).filter(|id| *id != 0);
        let anchor = open.clone().or_else(|| {
            let (_, l) = sent.first()?;
            let new_side = l.side.as_deref() != Some("LEFT");
            Some(LineBox { path: path.to_string(), new_side, line: l.line? as usize, start: None, anchor: l.line? as usize, reply_to: thread })
        });
        let mut card = div()
            .id(SharedString::from(format!("pr-thread-{path}-{}", old.or(new).unwrap_or(0))))
            .debug_selector({
                let s = format!("pr-thread-{path}-{}", new.or(old).unwrap_or(0));
                move || s
            })
            .font_family(theme::UI_FONT)
            .text_size(px(12.))
            .my(px(6.))
            .ml(px(60.))
            .mr(px(16.))
            .max_w(px(820.))
            .flex()
            .flex_col()
            .rounded(px(6.))
            .border_1()
            .border_color(theme::border_input())
            .bg(theme::bg_editor())
            .overflow_hidden()
            // Clicks in it stay in it (the row would take them).
            .on_click(|_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        for (i, l) in &sent {
            let range = l.start_line.filter(|s| Some(*s) < l.line).map(|s| lines_label(Some(s as usize), l.line.unwrap_or(0) as usize));
            card = card.child(comment(l.user.login.clone(), None, range, self.md.get(first + i).cloned(), None));
        }
        for (i, p) in &pending {
            let ix = *i;
            let del = div()
                .id(("pr-pending-del", ix))
                .debug_selector(move || format!("pr-pending-del-{ix}"))
                .cursor_pointer()
                .text_color(theme::fg_muted())
                .hover(|s| s.text_color(theme::git_deleted()))
                .tooltip(ui::tooltip("Delete this pending comment"))
                .on_click(cx.listener(move |v, _, _, cx| v.remove_pending(ix, cx)))
                .child("Delete")
                .into_any_element();
            let range = p.start_line.filter(|s| *s < p.line).map(|s| lines_label(Some(s), p.line));
            card = card.child(comment("You".into(), Some("Pending"), range, Some(Rc::new(Markdown::parse_doc(&p.body))), Some(del)));
        }
        match open {
            Some(b) => {
                let reply = b.reply_to.is_some();
                let review_label = if self.pending.is_empty() { "Start a review" } else { "Add review comment" };
                card = card.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .p(px(8.))
                        .child(div().id("pr-line-input").debug_selector(|| "pr-line-input".into()).h(px(90.)).px(px(6.)).py(px(4.)).rounded(px(4.)).bg(theme::bg_input()).border_1().border_color(theme::border_input()).overflow_hidden().child(self.line_input.clone()))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .child(div().flex_1().text_size(px(11.)).text_color(theme::fg_muted()).child(match &self.busy {
                                    Some(b) => format!("{b}…"),
                                    None => format!("Comment on {} ({}) · Shift-click + on another line for several", lines_label(b.start, b.line), if b.new_side { "new" } else { "old" }),
                                }))
                                .child(action_button("pr-line-cancel", "Cancel", None).on_click(cx.listener(|v, _, _, cx| v.close_line_box(cx))))
                                .when(reply, |d| d.child(action_button("pr-line-reply", "Reply", Some(theme::git_added())).on_click(cx.listener(|v, _, _, cx| v.single_comment(cx)))))
                                .when(!reply, |d| {
                                    d.child(action_button("pr-line-single", "Add single comment", None).on_click(cx.listener(|v, _, _, cx| v.single_comment(cx))))
                                        .child(action_button("pr-line-review", review_label, Some(theme::git_added())).on_click(cx.listener(|v, _, _, cx| v.add_pending(cx))))
                                }),
                        ),
                );
            }
            None => {
                // Reply…: answer the thread (or, with no thread, comment on the line again).
                let anchor = anchor.clone();
                card = card.child(
                    div()
                        .id("pr-thread-reply")
                        .debug_selector(|| "pr-thread-reply".into())
                        .m(px(8.))
                        .px(px(8.))
                        .h(px(26.))
                        .flex()
                        .items_center()
                        .rounded(px(4.))
                        .border_1()
                        .border_color(theme::border_input())
                        .bg(theme::bg_input())
                        .text_color(theme::fg_muted())
                        .cursor_text()
                        .on_click(cx.listener(move |v, _, _, cx| {
                            if let Some(b) = anchor.clone() {
                                v.open_line_box(b, cx);
                            }
                        }))
                        .child(if thread.is_some() { "Reply…" } else { "Comment on this line again…" }),
                );
            }
        }
        Some(card.into_any_element())
    }

    /// Finish your review: the pending comments, a summary, and Comment, Approve or Request changes.
    fn render_review_box(&self, at: Point<Pixels>, cx: &mut Context<Self>) -> AnyElement {
        let kind = self.review_kind;
        let option = |id: &'static str, value: &'static str, title: &'static str, sub: &'static str, cx: &mut Context<Self>| {
            let on = kind == value;
            div()
                .id(id)
                .debug_selector(move || id.to_string())
                .flex()
                .items_start()
                .gap(px(8.))
                .py(px(3.))
                .cursor_pointer()
                .on_click(cx.listener(move |v, _, _, cx| {
                    v.review_kind = value;
                    cx.notify();
                }))
                .child(div().mt(px(2.)).flex_none().size(px(13.)).rounded(px(7.)).border_1().border_color(if on { theme::accent() } else { theme::border_input() }).flex().items_center().justify_center().when(on, |d| d.child(div().size(px(7.)).rounded(px(4.)).bg(theme::accent()))))
                .child(div().flex().flex_col().child(div().text_color(theme::fg_bright()).child(title)).child(div().text_size(px(11.)).text_color(theme::fg_muted()).child(sub)))
        };
        let pending: Vec<AnyElement> = self
            .pending
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let path = p.path.clone();
                div()
                    .id(("pr-pending", i))
                    .debug_selector(move || format!("pr-pending-{i}"))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(6.))
                    .h(px(22.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::hover()))
                    .tooltip(ui::tooltip("Show its file"))
                    .on_click(cx.listener(move |v, _, _, cx| v.open_path(&path, cx)))
                    .child(div().flex_none().font_family(theme::MONO_FONT).text_size(px(11.)).text_color(theme::accent()).child(match p.start_line.filter(|s| *s < p.line) {
                        Some(s) => format!("{}:{s}-{}", p.path, p.line),
                        None => format!("{}:{}", p.path, p.line),
                    }))
                    .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_color(theme::fg_dim()).child(p.body.lines().next().unwrap_or("").to_string()))
                    .into_any_element()
            })
            .collect();
        let n = pending.len();
        let card = div()
            .id("pr-review-box")
            .debug_selector(|| "pr-review-box".into())
            .occlude()
            .w(px(500.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .p(px(12.))
            .rounded(px(8.))
            .bg(theme::bg_popup())
            .border_1()
            .border_color(theme::border_input())
            .shadow_lg()
            .text_size(px(12.))
            .on_mouse_down_out(cx.listener(|v, _, _, cx| {
                v.review_at = None;
                cx.notify();
            }))
            .child(div().text_color(theme::fg_bright()).font_weight(FontWeight::SEMIBOLD).text_size(px(13.)).child("Finish your review"))
            .when(n > 0, |d| {
                d.child(div().text_color(theme::fg_dim()).child(format!("{n} pending {} on the code, sent with the review:", if n == 1 { "comment" } else { "comments" })))
                    .child(div().id("pr-pending-list").flex().flex_col().max_h(px(150.)).overflow_y_scroll().children(pending))
            })
            .when(n == 0, |d| d.child(div().text_color(theme::fg_muted()).text_size(px(11.)).child("No pending comments. Click + on a line in Files changed to add some.")))
            .child(div().id("pr-input").debug_selector(|| "pr-input".into()).h(px(100.)).px(px(6.)).py(px(4.)).rounded(px(4.)).bg(theme::bg_input()).border_1().border_color(theme::border_input()).overflow_hidden().child(self.input.clone()))
            .child(option("pr-kind-comment", "comment", "Comment", "Submit general feedback without explicit approval.", cx))
            .child(option("pr-kind-approve", "approve", "Approve", "Give your approval to merge these changes.", cx))
            .child(option("pr-kind-request-changes", "request-changes", "Request changes", "Submit feedback that must be addressed before merging.", cx))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .when_some(self.busy.clone(), |d, b| d.child(div().text_color(theme::fg_muted()).child(format!("{b}…"))))
                    .child(div().flex_1())
                    .child(action_button("pr-submit-review", "Submit review", Some(theme::git_added())).on_click(cx.listener(|v, _, _, cx| v.submit_review(cx)))),
            );
        deferred(anchored().anchor(Anchor::TopRight).position(at).snap_to_window().child(card)).with_priority(2).into_any_element()
    }

    /// Files changed: the tree, its edge (drag to resize), the picked file's diff.
    fn render_files(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let diff: AnyElement = match (&self.diff, self.shown_files()) {
            (Some(diff), _) => diff.clone().into_any_element(),
            (None, Some(Err(e))) => ui::empty_state("warning", "Couldn't list the files", e.clone()).into_any_element(),
            (None, None) => ui::empty_state("refresh", "Bringing its commits here…", "The files show beside, once they're here.").into_any_element(),
            (None, Some(Ok(f))) if f.list.is_empty() => ui::empty_state("files", "No files changed", "").into_any_element(),
            (None, Some(Ok(_))) => ui::empty_state("files", "Pick a file", "Its diff shows here. Hover a line and click + to comment on it.").into_any_element(),
        };
        let cell = self.files_bounds.clone();
        div()
            .id("pr-files")
            .size_full()
            .relative()
            .flex()
            .child(canvas(move |b, _, _| cell.set(b), |_, _, _, _| {}).absolute().size_full())
            .child(self.render_tree(cx))
            .child(
                div()
                    .id("pr-tree-edge")
                    .debug_selector(|| "pr-tree-edge".into())
                    .flex()
                    .flex_none()
                    .justify_center()
                    .w(px(5.))
                    .h_full()
                    .cursor(CursorStyle::ResizeLeftRight)
                    .hover(|s| s.bg(theme::frame_focused().opacity(0.35)))
                    .on_mouse_down(MouseButton::Left, cx.listener(|v, _, _, cx| {
                        v.dragging_tree = true;
                        cx.stop_propagation();
                    }))
                    .child(div().w(px(1.)).h_full().bg(theme::border())),
            )
            .child(div().flex_1().min_w_0().h_full().child(diff))
            .on_mouse_move(cx.listener(|v, ev: &MouseMoveEvent, _, cx| {
                if !v.dragging_tree {
                    return;
                }
                if !ev.dragging() {
                    v.dragging_tree = false;
                    return;
                }
                let b = v.files_bounds.get();
                let w = f32::from(ev.position.x - b.left());
                v.tree_w = w.clamp(160., (f32::from(b.size.width) - 240.).max(160.));
                cx.notify();
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|v, _, _, _| v.dragging_tree = false))
            .into_any_element()
    }
}

/// "line 4", or "lines 3 to 7".
fn lines_label(start: Option<usize>, line: usize) -> String {
    match start.filter(|s| *s < line) {
        Some(s) => format!("lines {s} to {line}"),
        None => format!("line {line}"),
    }
}

/// The lines a comment is about, from the end of its diff hunk (as GitHub shows them over it).
fn hunk_excerpt(l: &LineComment) -> Option<Div> {
    let n = match (l.start_line, l.line.or(l.original_line)) {
        (Some(s), Some(e)) if s < e => (e - s + 1) as usize,
        _ => 4,
    };
    let lines: Vec<&str> = l.diff_hunk.lines().filter(|t| !t.starts_with("@@")).collect();
    if lines.is_empty() {
        return None;
    }
    let shown = &lines[lines.len().saturating_sub(n.min(40))..];
    Some(
        div()
            .flex()
            .flex_col()
            .py(px(4.))
            .bg(theme::bg_side())
            .border_b_1()
            .border_color(theme::border())
            .font_family(theme::MONO_FONT)
            .text_size(px(11.5))
            .children(shown.iter().map(|t| {
                let bg = if t.starts_with('+') { Some(theme::diff_ins_line()) } else if t.starts_with('-') { Some(theme::diff_del_line()) } else { None };
                div().px(px(12.)).whitespace_nowrap().overflow_hidden().text_ellipsis().text_color(theme::fg()).when_some(bg, |d, c| d.bg(c)).child(t.replace('\t', "    "))
            })),
    )
}

/// The pull request's state, as its badge says it.
fn state_label(d: &PrDetail) -> (&'static str, Hsla) {
    match (d.state.as_str(), d.is_draft) {
        ("MERGED", _) => ("Merged", theme::c(0xc678dd)),
        ("CLOSED", _) => ("Closed", theme::git_deleted()),
        (_, true) => ("Draft", theme::fg_muted()),
        _ => ("Open", theme::git_added()),
    }
}

fn open_url(url: &str) {
    if !url.is_empty() {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
}

/// A button: filled with `color`, else outlined.
fn action_button(id: &'static str, label: &'static str, color: Option<Hsla>) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || id.to_string())
        .flex_none()
        .px(px(12.))
        .h(px(26.))
        .flex()
        .items_center()
        .rounded(px(5.))
        .cursor_pointer()
        .font_weight(FontWeight::SEMIBOLD)
        .when_some(color, |d, c| d.bg(c).text_color(theme::on(c)).hover(|s| s.opacity(0.9)))
        .when(color.is_none(), |d| d.border_1().border_color(theme::border_input()).hover(|s| s.bg(theme::hover())))
        .child(label)
}

/// One check: where it stands, its name, and (a click) its run on GitHub.
fn check_row(i: usize, c: &gh::Check) -> impl IntoElement {
    let state = c.state();
    let (word, color) = match state {
        CheckState::Failing => ("Failing", theme::git_deleted()),
        CheckState::Running => ("In progress", theme::git_modified()),
        CheckState::Queued => ("Queued", theme::git_modified()),
        CheckState::Passed => ("Successful", theme::git_added()),
        CheckState::Skipped => ("Skipped", theme::fg_muted()),
    };
    let icon: AnyElement = match state {
        CheckState::Running => crate::pr_panel::spinner(("pr-check-spin", i), 12.).into_any_element(),
        CheckState::Failing => div().text_color(color).font_weight(FontWeight::BOLD).child("✗").into_any_element(),
        CheckState::Passed => div().text_color(color).font_weight(FontWeight::BOLD).child("✓").into_any_element(),
        CheckState::Queued => div().text_color(color).child("●").into_any_element(),
        CheckState::Skipped => div().text_color(color).child("⊘").into_any_element(),
    };
    let name = c.name();
    let url = c.url().map(String::from);
    let sel = format!("pr-check-{name}");
    let detail = if c.description.is_empty() { word.to_string() } else { format!("{word} · {}", c.description) };
    div()
        .id(("pr-check", i))
        .debug_selector(move || sel)
        .flex()
        .items_center()
        .gap(px(8.))
        .px(px(14.))
        .h(px(28.))
        .when(url.is_some(), |d| d.cursor_pointer().hover(|s| s.bg(theme::hover())))
        .when_some(url.clone(), |d, u| d.tooltip(ui::tooltip_text(format!("Open in the browser: {u}"))).on_click(move |_, _, _| open_url(&u)))
        .child(div().flex_none().w(px(14.)).flex().justify_center().child(icon))
        .child(div().flex_none().max_w(relative(0.55)).overflow_hidden().text_ellipsis().whitespace_nowrap().text_color(theme::fg_bright()).child(name))
        .child(div().min_w_0().flex_1().overflow_hidden().text_ellipsis().whitespace_nowrap().text_size(px(11.)).text_color(theme::fg_muted()).child(detail))
        .when(url.is_some(), |d| d.child(div().flex_none().text_size(px(11.)).text_color(theme::accent()).child("Details ↗")))
}

impl Focusable for PrView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for PrView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let base = div().id("pr-view").debug_selector(|| "pr-view".into()).track_focus(&self.focus).size_full().flex().flex_col().bg(theme::bg_editor()).text_size(px(12.));
        let (d, lines) = match &self.detail {
            None => return base.child(ui::empty_state("refresh", format!("Loading pull request #{}", self.number), "Reading it from GitHub with gh.")).into_any_element(),
            Some(Err(e)) => return base.child(ui::empty_state("warning", format!("Couldn't read pull request #{}", self.number), e.clone())).into_any_element(),
            Some(Ok((d, l))) => (d.clone(), l.clone()),
        };
        let n_files = match &self.files {
            Some(Ok(f)) => f.list.len().to_string(),
            Some(Err(_)) => "!".into(),
            None => "…".into(),
        };
        let tab = |id: &'static str, label: String, which: Tab, on: bool, cx: &mut Context<Self>| {
            div()
                .id(id)
                .debug_selector(move || id.to_string())
                .px(px(10.))
                .h(px(28.))
                .flex()
                .items_center()
                .cursor_pointer()
                .border_b_2()
                .border_color(if on { theme::accent() } else { gpui::transparent_black() })
                .text_color(if on { theme::fg_bright() } else { theme::fg_dim() })
                .hover(|s| s.text_color(theme::fg_bright()))
                .child(label)
                .on_click(cx.listener(move |v, _, _, cx| v.set_tab(which, cx)))
        };
        let root = self.repo.read(cx).root.clone();
        let body: AnyElement = match self.tab {
            Tab::Conversation => self.render_conversation(&root, &d, &lines, cx).into_any_element(),
            Tab::Commits if self.commit.is_some() => {
                let label = self.commit_label().unwrap_or_default();
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(px(8.))
                            .h(px(32.))
                            .px(px(8.))
                            .border_b_1()
                            .border_color(theme::border())
                            .child(
                                div()
                                    .id("pr-commit-back")
                                    .debug_selector(|| "pr-commit-back".into())
                                    .flex()
                                    .flex_none()
                                    .items_center()
                                    .gap(px(4.))
                                    .px(px(8.))
                                    .h(px(22.))
                                    .rounded(px(4.))
                                    .cursor_pointer()
                                    .text_color(theme::fg_dim())
                                    .hover(|s| s.bg(theme::hover()).text_color(theme::fg_bright()))
                                    .on_click(cx.listener(|v, _, _, cx| v.pick_commit(None, cx)))
                                    .child(ui::small_icon("chevron-left", theme::fg_dim()))
                                    .child("Commits"),
                            )
                            .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_color(theme::fg_bright()).child(label)),
                    )
                    .child(div().flex_1().min_h_0().child(self.render_files(cx)))
                    .into_any_element()
            }
            Tab::Commits => self.render_commits(&d, cx).into_any_element(),
            Tab::Files => self.render_files(cx),
        };
        let tc = self.tab;
        let n_pending = self.pending.len();
        base.child(self.header(&d, cx))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(4.))
                    .pl(px(8.))
                    .pr(px(8.))
                    .border_b_1()
                    .border_color(theme::border())
                    .child(tab("pr-tab-conversation", format!("Conversation ({})", d.comments.len() + d.reviews.len() + lines.len()), Tab::Conversation, tc == Tab::Conversation, cx))
                    .child(tab("pr-tab-commits", format!("Commits ({})", d.commits.len()), Tab::Commits, tc == Tab::Commits, cx))
                    .child(tab("pr-tab-files", format!("Files changed ({n_files})"), Tab::Files, tc == Tab::Files, cx))
                    .child(div().flex_1())
                    .child(
                        div()
                            .id("pr-review")
                            .debug_selector(|| "pr-review".into())
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(px(4.))
                            .px(px(9.))
                            .h(px(22.))
                            .rounded(px(5.))
                            .cursor_pointer()
                            .bg(theme::git_added())
                            .text_color(theme::on(theme::git_added()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .tooltip(ui::tooltip("Finish your review: send the pending comments, comment, approve or request changes"))
                            .on_click(cx.listener(|v, ev: &ClickEvent, window, cx| {
                                v.review_at = if v.review_at.is_some() { None } else { Some(point(ev.position().x.max(gpui::px(520.)), ev.position().y + gpui::px(14.))) };
                                if v.review_at.is_some() {
                                    window.focus(&v.input.focus_handle(cx), cx);
                                }
                                cx.notify();
                            }))
                            .child(if n_pending > 0 { format!("Finish your review ({n_pending})") } else { "Review changes".into() })
                            .child(ui::small_icon("chevron-down", theme::on(theme::git_added()))),
                    ),
            )
            .child(div().flex_1().min_h_0().min_w_0().overflow_hidden().child(body))
            .when_some(self.review_at, |el, at| el.child(self.render_review_box(at, cx)))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::Files;
    use crate::git::{Change, FileStatus};
    use std::collections::HashSet;

    #[test]
    fn a_thirty_thousand_file_pull_request_lists_at_once() {
        let list: Vec<Change> = (0..30_000).map(|i| Change { path: format!("src/m{}/d{}/file{i}.ts", i % 40, i % 300), orig_path: None, status: FileStatus::Modified }).collect();
        let t = std::time::Instant::now();
        let f = Files::new(String::new(), String::new(), list, &HashSet::new());
        let built = t.elapsed();
        assert_eq!(f.list.len(), 30_000);
        assert!(f.rows.len() > 30_000, "folders and files");
        // Folding a folder rebuilds the rows on the main thread: it must stay quick.
        let t = std::time::Instant::now();
        let rows = crate::commit_view::file_rows(&f.list, true, &HashSet::from(["src".to_string()]));
        let folded = t.elapsed();
        assert_eq!(rows.len(), 1, "everything under src is hidden");
        assert!(built.as_millis() < 1500 && folded.as_millis() < 500, "built in {built:?}, folded in {folded:?}");
        eprintln!("30k files: built {built:?}, folded {folded:?}");
    }
}
