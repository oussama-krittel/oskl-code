//! Reviewing a pull request in its own tab, as on GitHub: its conversation (Markdown, Mermaid
//! diagrams) with its commits, checks and whether it can merge, and a box to comment; its commits;
//! and Files changed, a tree beside the picked file's diff, where a + on a line comments on it
//! (at once, or kept pending until the review is finished: comment, approve or request changes).

use crate::commit_view::{FileRow, file_rows};
use crate::diff_view::{CommentOn, DiffView};
use crate::editor::{Editor, EditorEvent, EditorMode};
use crate::git::gh::{self, CheckState, Checks, DraftComment, Editable, LineComment, PrDetail};
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
    /// What the action being sent is for (a button shows "Resolving…" and waits).
    busy_target: Option<String>,
    /// Threads resolved or opened again here, shown so at once and kept until GitHub's own
    /// answer agrees (a read right after can still say the old thing).
    resolve_override: std::collections::HashMap<String, bool>,
    /// Reload clicked: the page is being read again.
    reloading: bool,
    /// Commits on its branch here that GitHub doesn't have yet.
    pub unpushed: usize,
    /// The review dropdown is open, hanging from here.
    pub review_at: Option<Point<Pixels>>,
    /// Who gh is signed in as: their comments can be edited and deleted.
    pub viewer: Option<String>,
    /// What's being edited, in `edit_input` (with Write and Preview).
    pub editing: Option<Editable>,
    edit_input: Entity<Editor>,
    edit_preview: Entity<MdPreview>,
    /// Resolved threads opened to read (by thread id).
    open_threads: HashSet<String>,
    /// How Merge merges (merge, squash or rebase), the ways the repository allows, the choice
    /// open, and the confirm form (its commit message) shown.
    pub merge_how: &'static str,
    merge_methods: Vec<&'static str>,
    merge_menu: bool,
    pub merging: bool,
    merge_subject: Entity<Editor>,
    merge_body: Entity<Editor>,
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
        let edit_input = editor("Leave a comment (Markdown)", false, cx);
        let merge_subject = editor("Commit message", true, cx);
        let merge_body = editor("Extended description", false, cx);
        merge_body.update(cx, |e, _| e.fill_height = true);
        let root = repo.read(cx).root.clone();
        let edit_preview = {
            let (c, r) = (edit_input.clone(), root.clone());
            cx.new(|cx| MdPreview::new(c, r, 160., "pr-edit-input", cx))
        };
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
            busy_target: None,
            resolve_override: Default::default(),
            reloading: false,
            unpushed: 0,
            review_at: None,
            viewer: None,
            editing: None,
            edit_input,
            edit_preview,
            open_threads: HashSet::new(),
            merge_how: "merge",
            merge_methods: vec!["merge", "squash", "rebase"],
            merge_menu: false,
            merging: false,
            merge_subject,
            merge_body,
            _load: None,
            _commit_load: None,
            _act: None,
            _poll: None,
            _diff_sub: None,
            _subs: subs,
        };
        v.read_pending(root.clone(), cx);
        let who = cx.background_executor().spawn(async move { (gh::viewer(&root), gh::merge_methods(&root)) });
        cx.spawn(async move |v, cx| {
            let (who, methods) = who.await;
            v.update(cx, |v, cx| {
                v.viewer = who;
                if !methods.contains(&v.merge_how) {
                    v.merge_how = methods[0];
                }
                v.merge_methods = methods;
                v.refresh_notes(cx);
            })
            .ok();
        })
        .detach();
        v.load(cx);
        v
    }

    /// `login` is who gh is signed in as.
    fn mine(&self, login: &str) -> bool {
        self.viewer.as_deref() == Some(login)
    }

    /// The thread comment `l` is in.
    fn thread_of(&self, l: &LineComment) -> Option<gh::Thread> {
        let root = l.in_reply_to_id.unwrap_or(l.id);
        let mut t = match &self.detail {
            Some(Ok((d, _))) => d.threads.iter().find(|t| t.root == root).cloned(),
            _ => None,
        }?;
        if let Some(r) = self.resolve_override.get(&t.id) {
            t.resolved = *r;
        }
        Some(t)
    }

    /// The action for `target` is being sent (its button says so and waits).
    fn sending(&self, target: &str) -> bool {
        self.busy_target.as_deref() == Some(target)
    }

    /// A button that sends to GitHub: while it's sending, `busy` ("Commenting…"), dimmed.
    fn send_button(&self, id: &'static str, label: impl Into<SharedString>, busy: &'static str, target: &str, color: Option<Hsla>) -> Stateful<Div> {
        let on = self.sending(target);
        action_button(id, if on { SharedString::from(busy) } else { label.into() }, color).when(on, |b| b.opacity(0.6).cursor_default())
    }

    /// Reads everything again (the Reload button): page, comments, checks and files.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.reloading = true;
        // Fresh from GitHub: what it says about each thread counts again.
        self.resolve_override.clear();
        self.load(cx);
        cx.notify();
    }

    pub fn start_edit(&mut self, target: Editable, text: String, cx: &mut Context<Self>) {
        self.edit_input.update(cx, |e, cx| e.set_text(&text, cx));
        self.edit_preview.update(cx, |p, cx| {
            p.previewing = false;
            cx.notify();
        });
        self.editing = Some(target);
        self.refresh_notes(cx);
    }

    fn cancel_edit(&mut self, cx: &mut Context<Self>) {
        self.editing = None;
        self.edit_input.update(cx, |e, cx| e.set_text("", cx));
        self.refresh_notes(cx);
    }

    /// Update comment: the edited text, on GitHub.
    pub fn save_edit(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.editing.clone() else { return };
        let (n, body) = (self.number, self.edit_input.read(cx).text());
        if body.trim().is_empty() && target != Editable::Body {
            return cx.emit(Request::Error("A comment can't be empty (delete it instead)".into()));
        }
        self.busy_target = Some("edit".into());
        self.act("Edit", move |root| gh::edit(root, n, &target, &body), |v, cx| v.cancel_edit(cx), cx);
    }

    fn ask_delete(&mut self, target: Editable, cx: &mut Context<Self>) {
        let me = cx.entity().downgrade();
        cx.emit(Request::Ask(crate::dialog::Ask {
            title: "Delete this comment?".into(),
            detail: "On GitHub, for everyone. It can't be undone.".into(),
            buttons: vec!["Delete".into()],
            danger: true,
            cancel_default: true,
            cancel_label: None,
            on_answer: Rc::new(move |_, _, cx| {
                let t = target.clone();
                me.update(cx, |v, cx| {
                    if v.busy.is_none() {
                        v.busy_target = Some(format!("delete:{t:?}"));
                    }
                    v.act("Delete", move |root| gh::delete(root, &t), |_, _| {}, cx)
                })
                .ok();
            }),
        }));
    }

    /// Resolves the thread (or opens it again).
    /// Shown so at once (a resolved thread folds, an unresolved one stays open); undone if
    /// GitHub refuses.
    fn set_resolved(&mut self, thread: String, resolved: bool, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        if resolved {
            self.open_threads.remove(&thread);
        }
        self.resolve_override.insert(thread.clone(), resolved);
        self.busy_target = Some(thread.clone());
        self.refresh_notes(cx);
        let id = thread.clone();
        self.act_full(
            if resolved { "Resolve" } else { "Unresolve" },
            move |root| gh::resolve(root, &thread, resolved),
            |_, _| {},
            move |v, cx| {
                v.resolve_override.remove(&id);
                v.refresh_notes(cx);
            },
            cx,
        );
    }

    fn toggle_thread(&mut self, thread: String, cx: &mut Context<Self>) {
        if !self.open_threads.remove(&thread) {
            self.open_threads.insert(thread);
        }
        self.refresh_notes(cx);
    }

    /// Edit, and Delete, for what's yours.
    fn tools(&self, target: Editable, text: String, delete: bool, cx: &mut Context<Self>) -> AnyElement {
        let key = match &target {
            Editable::Body => "body".to_string(),
            Editable::Comment(id) | Editable::Review(id) => id.clone(),
            Editable::Line(n) => n.to_string(),
        };
        let t2 = target.clone();
        div()
            .flex()
            .flex_none()
            .gap(px(10.))
            .text_size(px(11.))
            .child(text_link(format!("pr-edit-{key}"), "Edit", theme::fg_muted()).on_click(cx.listener(move |v, _, _, cx| v.start_edit(target.clone(), text.clone(), cx))))
            .when(delete, |d| {
                let deleting = self.sending(&format!("delete:{t2:?}"));
                d.child(text_link(format!("pr-delete-{key}"), if deleting { "Deleting…" } else { "Delete" }, theme::git_deleted()).on_click(cx.listener(move |v, _, _, cx| v.ask_delete(t2.clone(), cx))))
            })
            .into_any_element()
    }

    /// The box editing a comment: Write and Preview, Cancel and Update comment.
    fn edit_box(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .p(px(10.))
            .child(self.edit_preview.clone())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().flex_1().text_size(px(11.)).text_color(theme::fg_muted()).children(self.busy.clone().map(|b| format!("{b}…"))))
                    .child(action_button("pr-edit-cancel", "Cancel", None).on_click(cx.listener(|v, _, _, cx| v.cancel_edit(cx))))
                    .child(self.send_button("pr-edit-save", "Update comment", "Updating…", "edit", Some(theme::git_added())).on_click(cx.listener(|v, _, _, cx| v.save_edit(cx)))),
            )
            .into_any_element()
    }

    /// One comment on a line, in a thread: who, its text (or its edit box), Edit and Delete if yours.
    fn line_comment_el(&self, l: &LineComment, md: Option<Rc<Markdown>>, cx: &mut Context<Self>) -> AnyElement {
        let root = self.repo.read(cx).root.clone();
        let target = Editable::Line(l.id);
        let editing = self.editing.as_ref() == Some(&target);
        let tools = (self.mine(&l.user.login) && !editing && l.id != 0).then(|| self.tools(target, l.body.clone(), true, cx));
        let when = crate::pr_panel::github_time(&l.created_at).map(crate::util::relative_time).unwrap_or_default();
        let range = l.start_line.filter(|s| Some(*s) < l.line).map(|s| lines_label(Some(s as usize), l.line.unwrap_or(0) as usize));
        let opener: crate::preview::Opener = Rc::new(|path, _, _, _| {
            let _ = std::process::Command::new("open").arg(path).spawn();
        });
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .px(px(16.))
            .py(px(12.))
            .border_b_1()
            .border_color(theme::border())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(div().text_color(theme::fg_bright()).font_weight(FontWeight::SEMIBOLD).child(l.user.login.clone()))
                    .child(div().text_size(px(11.)).text_color(theme::fg_muted()).child(when))
                    .when_some(range, |d, r| d.child(div().text_size(px(11.)).text_color(theme::fg_muted()).child(format!("on {r}"))))
                    .child(div().flex_1())
                    .children(tools),
            )
            .child(if editing { self.edit_box(cx) } else { md.map(|m| m.render(&root, &self.images, opener).into_any_element()).unwrap_or_else(|| div().into_any_element()) })
            .into_any_element()
    }

    /// Resolve conversation / Unresolve conversation.
    fn resolve_button(&self, t: &gh::Thread, cx: &mut Context<Self>) -> AnyElement {
        let (id, resolved) = (t.id.clone(), t.resolved);
        let sel = format!("pr-resolve-{}", t.root);
        let sending = self.sending(&t.id);
        div()
            .id(SharedString::from(sel.clone()))
            .debug_selector(move || sel)
            .flex_none()
            .px(px(10.))
            .h(px(30.))
            .flex()
            .items_center()
            .rounded(px(4.))
            .border_1()
            .border_color(theme::border_input())
            .when(!sending, |d| d.cursor_pointer().hover(|s| s.bg(theme::hover())))
            .when(sending, |d| d.opacity(0.6))
            .child(match (sending, resolved) {
                // Already shown the other way: this says what's being sent.
                (true, true) => "Resolving…",
                (true, false) => "Unresolving…",
                (false, true) => "Unresolve conversation",
                (false, false) => "Resolve conversation",
            })
            .on_click(cx.listener(move |v, _, _, cx| v.set_resolved(id.clone(), !resolved, cx)))
            .into_any_element()
    }

    /// A resolved thread folded to one line: open it to read it.
    fn folded_thread(&self, t: &gh::Thread, label: String, open: bool, cx: &mut Context<Self>) -> AnyElement {
        let id = t.id.clone();
        let sel = format!("pr-thread-toggle-{}", t.root);
        div()
            .id(SharedString::from(sel.clone()))
            .debug_selector(move || sel)
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(16.))
            .h(px(40.))
            .bg(theme::bg_card_head())
            .cursor_pointer()
            .hover(|s| s.bg(theme::hover()))
            .on_click(cx.listener(move |v, _, _, cx| v.toggle_thread(id.clone(), cx)))
            .child(ui::small_icon(if open { "chevron-down" } else { "chevron-right" }, theme::fg_muted()))
            .child(div().min_w_0().flex_1().overflow_hidden().text_ellipsis().whitespace_nowrap().text_color(theme::fg_dim()).child(label))
            .child(div().flex_none().px(px(6.)).rounded(px(8.)).border_1().border_color(theme::c(0xc678dd)).text_color(theme::c(0xc678dd)).text_size(px(10.)).child("Resolved"))
            .child(div().flex_none().text_size(px(11.)).text_color(theme::accent()).child(if open { "Hide" } else { "Show resolved" }))
            .into_any_element()
    }

    /// Commits pushed one after another: "who added N commits, when", then a row each (a
    /// click shows what it changed).
    fn commit_group(&self, commits: Vec<(String, String, String, String)>, cx: &mut Context<Self>) -> AnyElement {
        let who = commits.first().map(|c| c.3.clone()).unwrap_or_default();
        let when = commits.last().and_then(|c| crate::pr_panel::github_time(&c.0)).map(crate::util::relative_time).unwrap_or_default();
        let n = commits.len();
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .pl(px(4.))
                    .h(px(40.))
                    .child(timeline_dot(ui::small_icon("git", theme::fg_muted()).into_any_element()))
                    .child(div().flex_none().text_color(theme::fg_bright()).font_weight(FontWeight::SEMIBOLD).child(who))
                    .child(div().flex_none().text_color(theme::fg_dim()).child(format!("added {n} {}", if n == 1 { "commit" } else { "commits" })))
                    .child(div().flex_none().text_color(theme::fg_muted()).child(when)),
            )
            .children(commits.into_iter().map(|(_, sha, headline, _)| {
                let short = sha.get(..7).unwrap_or(&sha).to_string();
                let sel = format!("pr-timeline-commit-{short}");
                div()
                    .id(SharedString::from(sel.clone()))
                    .debug_selector(move || sel)
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .pl(px(14.))
                    .pr(px(8.))
                    .h(px(34.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::hover()))
                    .tooltip(ui::tooltip("Show what this commit changed"))
                    .on_click(cx.listener(move |v, _, _, cx| v.pick_commit(Some(sha.clone()), cx)))
                    // The thread's line, with a dot for the commit.
                    .child(div().flex_none().w(px(12.)).h_full().flex().items_center().justify_center().child(div().size(px(9.)).rounded(px(5.)).border_2().border_color(theme::fg_muted())))
                    .child(div().min_w_0().flex_1().overflow_hidden().text_ellipsis().whitespace_nowrap().font_family(theme::MONO_FONT).text_size(px(11.5)).text_color(theme::fg()).child(headline))
                    .child(div().flex_none().font_family(theme::MONO_FONT).text_size(px(11.)).text_color(theme::accent()).child(short))
            }))
            .into_any_element()
    }

    /// A thread of comments on lines, in the conversation: where, the code, the comments, and
    /// Resolve (a resolved one folded until opened).
    fn conversation_thread(&self, root_ix: usize, lines: &[LineComment], cx: &mut Context<Self>) -> AnyElement {
        let first = self.first_line_md();
        let root = &lines[root_ix];
        let label = format!("{} {}", root.path, lines_label(root.start_line.map(|s| s as usize), root.line.or(root.original_line).unwrap_or(0) as usize));
        let thread = self.thread_of(root);
        let resolved = thread.as_ref().is_some_and(|t| t.resolved);
        let open = !resolved || thread.as_ref().is_some_and(|t| self.open_threads.contains(&t.id));
        let mut el = div().flex().flex_col().rounded(px(6.)).border_1().border_color(theme::border()).overflow_hidden().bg(theme::bg_card());
        el = match (&thread, resolved) {
            (Some(t), true) => el.child(self.folded_thread(t, label, open, cx)),
            _ => el.child(div().px(px(16.)).h(px(38.)).flex().items_center().bg(theme::bg_card_head()).font_family(theme::MONO_FONT).text_size(px(11.)).text_color(theme::accent()).child(label)),
        };
        if open {
            el = el.children(hunk_excerpt(root));
            for (i, l) in lines.iter().enumerate().filter(|(i, l)| *i == root_ix || l.in_reply_to_id == Some(root.id)) {
                el = el.child(self.line_comment_el(l, self.md.get(first + i).cloned(), cx));
            }
            if let Some(t) = &thread {
                el = el.child(div().flex().px(px(16.)).py(px(12.)).child(self.resolve_button(t, cx)));
            }
        }
        el.into_any_element()
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
            let branch = detail.as_ref().ok().map(|(d, _)| d.head_ref_name.clone()).unwrap_or_default();
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
                    // Its branch here, with commits the pull request doesn't have yet.
                    let local = format!("refs/heads/{branch}");
                    let unpushed = git::run(&root, &["rev-parse", "--verify", "--quiet", &local])
                        .ok()
                        .and_then(|_| git::run(&root, &["rev-list", "--count", &format!("{head}..{local}")]).ok())
                        .and_then(|s| s.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    anyhow::Ok((mb, head, list, unpushed))
                })
                .await
                .map_err(|e| format!("Couldn't bring its commits here: {e}"));
            v.update(cx, |v, cx| {
                v.unpushed = found.as_ref().map(|f| f.3).unwrap_or(0);
                v.files = Some(found.map(|(b, h, list, _)| Files::new(b, h, list, &v.collapsed)));
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
        self.reloading = false;
        // GitHub now says what was done here: its own answer counts again.
        if let Ok((d, _)) = &detail {
            self.resolve_override.retain(|id, r| d.threads.iter().find(|t| &t.id == id).is_some_and(|t| t.resolved != *r));
        }
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
        let (base_for_hunks, head_for_hunks) = (base.clone(), head.clone());
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
        // Where GitHub takes comments: the parts of the diff as git cuts them.
        if commentable {
            let (root, path, d) = (self.repo.read(cx).root.clone(), c.path.clone(), diff.downgrade());
            let task = cx.background_executor().spawn(async move { gh::hunks(&root, &base_for_hunks, &head_for_hunks, &path) });
            cx.spawn(async move |_, cx| {
                let h = task.await;
                d.update(cx, |d, cx| {
                    d.git_hunks = Some(h);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
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
            if let Some(d) = &self.diff
                && !d.read(cx).same_hunk(ev.0, a, z)
            {
                return cx.emit(Request::Error("GitHub takes a comment on several lines only within one changed part of the diff".into()));
            }
            b.start = (a < z).then_some(a);
            b.line = z;
            self.focus_line = true;
            return self.refresh_notes(cx);
        }
        let first = ev.3.unwrap_or(ev.1);
        self.open_line_box(LineBox { path: path.to_string(), new_side: ev.0, line: ev.1, start: (first < ev.1).then_some(first), anchor: first, reply_to: None }, cx);
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
        let (commit, base) = match &self.files {
            Some(Ok(f)) => (f.head.clone(), Some(f.base.clone())),
            _ => (commit, None),
        };
        let label = if b.reply_to.is_some() { "Reply".to_string() } else { format!("Comment on {}", lines_label(b.start, b.line)) };
        if self.busy.is_none() {
            self.busy_target = Some("line".into());
        }
        self.act(
            &label,
            move |root| match b.reply_to {
                Some(to) => gh::reply(root, n, to, &body),
                None => {
                    let draft = DraftComment { path: b.path.clone(), line: b.line, new_side: b.new_side, body: body.clone(), start_line: b.start };
                    if let Some(base) = &base
                        && !gh::fits(&gh::hunks(root, base, &commit, &b.path), &draft)
                    {
                        anyhow::bail!("GitHub takes comments only on the diff's changed parts (a change and 3 lines around it)");
                    }
                    gh::line_comment(root, n, &commit, &b.path, b.start, b.line, b.new_side, &body)
                }
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
        // The diff the comments were written on (its commits here), else GitHub's head.
        let (base, commit) = match &self.files {
            Some(Ok(f)) => (Some(f.base.clone()), f.head.clone()),
            _ => (None, d.head_ref_oid.clone()),
        };
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
        if self.busy.is_none() {
            self.busy_target = Some("review".into());
        }
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
                    match base {
                        Some(base) => gh::send_review(root, n, &base, &commit, event, &body, &pending).map(|moved| match moved {
                            0 => String::new(),
                            m => format!("{m} comment(s) on lines outside the diff's changed parts went into the review's text"),
                        }),
                        None => gh::submit_review(root, n, &commit, event, &body, &pending),
                    }
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
        if self.busy.is_none() {
            self.busy_target = Some("comment".into());
        }
        self.act("Comment", move |root| gh::comment(root, n, &body), |v, cx| v.comment_input.update(cx, |e, cx| e.set_text("", cx)), cx);
    }

    /// Close pull request (with the box's comment, if any), or Reopen pull request.
    pub fn close_or_reopen(&mut self, cx: &mut Context<Self>) {
        let Some(Ok((d, _))) = &self.detail else { return };
        let open = d.state == "CLOSED";
        let (n, body) = (self.number, self.comment_input.read(cx).text());
        let label = if open { "Reopen" } else { "Close" };
        if self.busy.is_none() {
            self.busy_target = Some("close".into());
        }
        self.act(label, move |root| gh::set_open(root, n, open, &body), |v, cx| {
            v.comment_input.update(cx, |e, cx| e.set_text("", cx));
            cx.emit(PrEvent::Changed);
        }, cx);
    }

    /// Runs a `gh` action in the background; once done, `ok`, the page read again and the lists told.
    fn act(&mut self, label: &str, op: impl FnOnce(&std::path::Path) -> anyhow::Result<String> + Send + 'static, ok: impl FnOnce(&mut Self, &mut Context<Self>) + 'static, cx: &mut Context<Self>) {
        self.act_full(label, op, ok, |_, _| {}, cx);
    }

    /// `act`, and `failed` when GitHub refuses.
    fn act_full(
        &mut self,
        label: &str,
        op: impl FnOnce(&std::path::Path) -> anyhow::Result<String> + Send + 'static,
        ok: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
        failed: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        // Already sending (⌘Enter pressed again, a second click): once is enough.
        if self.busy.is_some() {
            return;
        }
        let root = self.repo.read(cx).root.clone();
        self.busy = Some(label.to_string());
        let label = label.to_string();
        let task = cx.background_executor().spawn(async move { op(&root) });
        self._act = Some(cx.spawn(async move |v, cx| {
            let result = task.await;
            v.update(cx, |v, cx| {
                v.busy = None;
                v.busy_target = None;
                match result {
                    Ok(out) => {
                        let note = if out.contains("outside the diff") { format!(" ({out})") } else { String::new() };
                        cx.emit(Request::Message(format!("{label}: done{note}")));
                        ok(v, cx);
                        cx.emit(PrEvent::Changed);
                        v.refresh_detail(cx);
                    }
                    Err(e) => {
                        let e = e.to_string();
                        let hint = if e.contains("could not be resolved") { " — a comment is on a line outside the diff's changed parts: remove it from Finish your review and send again" } else { "" };
                        failed(v, cx);
                        cx.emit(Request::Error(format!("{label} failed: {e}{hint}")))
                    }
                }
                v.repo.update(cx, |r, cx| r.refresh_all(cx));
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// The merge button: its confirm form, with the commit message GitHub would suggest.
    pub fn start_merge(&mut self, cx: &mut Context<Self>) {
        let Some(Ok((d, _))) = &self.detail else { return };
        let (subject, body) = match self.merge_how {
            "squash" => (
                format!("{} (#{})", d.title, d.number),
                d.commits
                    .iter()
                    .map(|c| if c.message_body.trim().is_empty() { format!("* {}", c.message_headline) } else { format!("* {}\n\n{}", c.message_headline, c.message_body.trim()) })
                    .collect::<Vec<_>>()
                    .join("\n\n"),
            ),
            _ => (format!("Merge pull request #{} from {}/{}", d.number, d.head_repository_owner.login, d.head_ref_name), d.title.clone()),
        };
        self.merge_subject.update(cx, |e, cx| e.set_text(&subject, cx));
        self.merge_body.update(cx, |e, cx| e.set_text(&body, cx));
        self.merging = true;
        self.merge_menu = false;
        cx.notify();
    }

    pub fn confirm_merge(&mut self, cx: &mut Context<Self>) {
        let (n, how) = (self.number, self.merge_how);
        let (subject, body) = (self.merge_subject.read(cx).text(), self.merge_body.read(cx).text());
        if how != "rebase" && subject.trim().is_empty() {
            return cx.emit(Request::Error("Give the commit a message first".into()));
        }
        if self.busy.is_none() {
            self.busy_target = Some("merge".into());
        }
        self.act(
            "Merge",
            move |root| gh::merge(root, n, how, subject.trim(), &body),
            |v, cx| {
                v.merging = false;
                v.load(cx);
            },
            cx,
        );
    }

    /// GitHub's merge: the chosen way as one button, the arrow beside it for the others; then
    /// its confirm form (the commit's message, or for a rebase only Confirm).
    fn render_merge_action(&self, d: &PrDetail, can: bool, cx: &mut Context<Self>) -> AnyElement {
        let label = |how: &str| match how {
            "squash" => "Squash and merge",
            "rebase" => "Rebase and merge",
            _ => "Merge pull request",
        };
        let n = d.commits.len();
        let commits = if n == 1 { "The 1 commit".to_string() } else { format!("The {n} commits") };
        if self.merging {
            let how = self.merge_how;
            let confirm = match how {
                "squash" => "Confirm squash and merge",
                "rebase" => "Confirm rebase and merge",
                _ => "Confirm merge",
            };
            let field = |label: &'static str, sel: &'static str, child: AnyElement, h: Option<f32>| {
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(div().text_color(theme::fg_bright()).font_weight(FontWeight::SEMIBOLD).child(label))
                    .child(div().debug_selector(move || sel.into()).when_some(h, |d, h| d.h(px(h))).p(px(6.)).rounded(px(5.)).bg(theme::bg_input()).border_1().border_color(theme::border_input()).overflow_hidden().child(child))
            };
            return div()
                .id("pr-merge-form")
                .debug_selector(|| "pr-merge-form".into())
                .flex()
                .flex_col()
                .gap(px(12.))
                .px(px(14.))
                .py(px(12.))
                .when(how == "rebase", |el| el.child(div().text_color(theme::fg_dim()).child(format!("This will rebase your changes and merge them into {}.", d.base_ref_name))))
                .when(how != "rebase", |el| {
                    el.child(field("Commit message", "pr-merge-subject", self.merge_subject.clone().into_any_element(), None))
                        .child(field("Extended description", "pr-merge-body", self.merge_body.clone().into_any_element(), Some(150.)))
                })
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(8.))
                        .child(self.send_button(
                            "pr-merge-confirm",
                            confirm,
                            match how {
                                "squash" => "Squashing and merging…",
                                "rebase" => "Rebasing and merging…",
                                _ => "Merging…",
                            },
                            "merge",
                            Some(theme::git_added()),
                        ).on_click(cx.listener(|v, _, _, cx| v.confirm_merge(cx))))
                        .child(action_button("pr-merge-cancel", "Cancel", None).on_click(cx.listener(|v, _, _, cx| {
                            v.merging = false;
                            cx.notify();
                        })))
                        .children(self.busy.clone().map(|b| div().text_color(theme::fg_muted()).child(format!("{b}…")))),
                )
                .into_any_element();
        }
        let how = self.merge_how;
        let color = theme::git_added();
        let fg = if can { theme::on(color) } else { theme::fg_muted() };
        let bg = if can { color } else { theme::hover() };
        let menu = self.merge_menu.then(|| {
            let options = self.merge_methods.iter().map(|&m| {
                let (title, sub) = match m {
                    "squash" => ("Squash and merge", format!("{commits} from this branch will be combined into one commit in the base branch.")),
                    "rebase" => ("Rebase and merge", format!("{commits} from this branch will be rebased and added to the base branch.")),
                    _ => ("Create a merge commit", "All commits from this branch will be added to the base branch via a merge commit.".to_string()),
                };
                let sel = format!("pr-merge-how-{m}");
                div()
                    .id(SharedString::from(sel.clone()))
                    .debug_selector(move || sel)
                    .flex()
                    .items_start()
                    .gap(px(8.))
                    .px(px(12.))
                    .py(px(8.))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::hover()))
                    .on_click(cx.listener(move |v, _, _, cx| {
                        v.merge_how = m;
                        v.merge_menu = false;
                        cx.notify();
                    }))
                    .child(div().flex_none().w(px(14.)).text_color(theme::fg_bright()).child(if m == how { "✓" } else { "" }))
                    .child(div().flex().flex_col().gap(px(2.)).child(div().text_color(theme::fg_bright()).font_weight(FontWeight::SEMIBOLD).child(title)).child(div().text_size(px(11.)).text_color(theme::fg_muted()).child(sub)))
            });
            deferred(
                anchored().snap_to_window().child(
                    div()
                        .id("pr-merge-menu")
                        .debug_selector(|| "pr-merge-menu".into())
                        .occlude()
                        .mt(px(4.))
                        .w(px(360.))
                        .flex()
                        .flex_col()
                        .py(px(4.))
                        .rounded(px(8.))
                        .bg(theme::bg_popup())
                        .border_1()
                        .border_color(theme::border_input())
                        .shadow_lg()
                        .text_size(px(12.))
                        .on_mouse_down_out(cx.listener(|v, _, _, cx| {
                            v.merge_menu = false;
                            cx.notify();
                        }))
                        .children(options),
                ),
            )
            .with_priority(2)
        });
        div()
            .flex()
            .flex_col()
            // As wide as its label, not the whole box.
            .items_start()
            .px(px(14.))
            .py(px(10.))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .h(px(28.))
                    .rounded(px(5.))
                    .overflow_hidden()
                    .bg(bg)
                    .text_color(fg)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(
                        div()
                            .id("pr-merge")
                            .debug_selector(|| "pr-merge".into())
                            .flex()
                            .items_center()
                            .px(px(14.))
                            .when(can, |d| d.cursor_pointer().hover(|s| s.opacity(0.9)).on_click(cx.listener(|v, _, _, cx| v.start_merge(cx))))
                            .child(label(how)),
                    )
                    .child(div().w(px(1.)).h_full().bg(theme::bg_editor().opacity(0.4)))
                    .child(
                        div()
                            .id("pr-merge-choose")
                            .debug_selector(|| "pr-merge-choose".into())
                            .flex()
                            .items_center()
                            .px(px(8.))
                            .cursor_pointer()
                            .hover(|s| s.opacity(0.9))
                            .tooltip(ui::tooltip("Choose how it merges"))
                            .on_click(cx.listener(|v, _, _, cx| {
                                v.merge_menu = !v.merge_menu;
                                cx.notify();
                            }))
                            .child(ui::small_icon("chevron-down", fg)),
                    ),
            )
            .children(menu)
            .into_any_element()
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
            .child(if self.reloading {
                div().id("pr-reloading").debug_selector(|| "pr-reloading".into()).size(px(24.)).flex().flex_none().items_center().justify_center().child(crate::pr_panel::spinner("pr-reload-spin", 14.)).into_any_element()
            } else {
                button("pr-reload", "refresh", "Reload: read it again from GitHub (comments, checks, files)").on_click(cx.listener(|v, _, _, cx| v.reload(cx))).into_any_element()
            })
            .child(button("pr-checkout", "arrow-down", "Check out this pull request's branch").on_click(cx.listener(|v, _, _, cx| {
                let n = v.number;
                v.act("Check out", move |root| gh::checkout(root, n), |_, _| {}, cx);
            })))
            .child(button("pr-browser", "globe", "Open on GitHub").on_click(cx.listener(move |_, _, _, _| open_url(&url))))
    }

    /// Its branch here is ahead of GitHub's: says so, with Push.
    fn render_unpushed(&self, d: &PrDetail, cx: &mut Context<Self>) -> Option<AnyElement> {
        let n = self.unpushed;
        if n == 0 || d.state != "OPEN" {
            return None;
        }
        let branch = d.head_ref_name.clone();
        let pushing = self.sending("push");
        Some(
            div()
                .id("pr-unpushed")
                .debug_selector(|| "pr-unpushed".into())
                .flex()
                .flex_none()
                .items_center()
                .gap(px(10.))
                .px(px(12.))
                .py(px(6.))
                .border_b_1()
                .border_color(theme::border())
                .bg(theme::git_modified().opacity(0.08))
                .child(ui::small_icon("arrow-up", theme::git_modified()))
                .child(div().flex_1().text_color(theme::fg()).child(format!("origin/{branch} is {n} {} behind your local {branch}: this pull request doesn't have {} yet.", if n == 1 { "commit" } else { "commits" }, if n == 1 { "it" } else { "them" })))
                .child(self.send_button("pr-push", "Push", "Pushing…", "push", None).when(!pushing, |b| {
                    b.on_click(cx.listener(move |v, _, _, cx| {
                        let branch = branch.clone();
                        if v.busy.is_none() {
                            v.busy_target = Some("push".into());
                        }
                        v.act("Push", move |root| gh::push(root, &branch), |v, cx| v.reload(cx), cx);
                    }))
                }))
                .into_any_element(),
        )
    }

    /// Who did what, when: a card with its Markdown.
    /// `target`: what Edit changes (and the card shows its edit box while it's edited).
    #[allow(clippy::too_many_arguments)]
    fn card(&self, root: &Path, who: String, did: String, when: String, excerpt: Option<Div>, body: Option<Rc<Markdown>>, edit: Option<(Editable, String, bool)>, extra: Vec<AnyElement>, cx: &mut Context<Self>) -> Div {
        let color = if did.starts_with("approved") { theme::git_added() } else if did.starts_with("requested changes") { theme::git_deleted() } else { theme::fg_dim() };
        let editing = edit.as_ref().is_some_and(|(t, _, _)| self.editing.as_ref() == Some(t));
        let tools = edit.filter(|_| !editing && self.mine(&who)).map(|(t, text, del)| self.tools(t, text, del, cx));
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
            .bg(theme::bg_card())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(16.))
                    .h(px(44.))
                    .bg(theme::bg_card_head())
                    .border_b_1()
                    .border_color(theme::border())
                    .child(div().text_color(theme::fg_bright()).font_weight(FontWeight::SEMIBOLD).child(who))
                    .child(div().text_color(color).child(did))
                    .child(div().text_color(theme::fg_muted()).child(when))
                    .child(div().flex_1())
                    .children(tools),
            )
            .children(excerpt)
            .when(editing, |d| d.child(self.edit_box(cx)))
            .when_some(body.filter(|_| !editing), |d, md| d.child(div().px(px(18.)).py(px(16.)).child(md.render(root, &self.images, opener))))
            .when(!extra.is_empty(), |d| d.child(div().flex().flex_col().gap(px(14.)).p(px(16.)).children(extra)))
    }

    fn render_conversation(&self, root: &Path, d: &PrDetail, lines: &[LineComment], cx: &mut Context<Self>) -> impl IntoElement {
        let md = |ix: usize| self.md.get(ix).cloned();
        enum Item {
            Comment(usize),
            Review(usize),
            /// A thread of comments on lines that no review shows.
            Thread(usize),
            Commit(String, String, String),
            ForcePush(usize),
        }
        // Comments, reviews (with their threads), lone threads and commits, oldest first.
        let mut items: Vec<(String, Item)> = d.comments.iter().enumerate().map(|(i, c)| (c.created_at.clone(), Item::Comment(i))).collect();
        let n_comments = d.comments.len();
        let review_ids: HashSet<u64> = d.reviews.iter().map(|r| r.database_id).filter(|id| *id != 0).collect();
        let roots_of = |id: u64| lines.iter().enumerate().filter(move |(_, l)| l.in_reply_to_id.is_none() && l.pull_request_review_id == Some(id)).map(|(i, _)| i).collect::<Vec<_>>();
        items.extend(d.reviews.iter().enumerate().filter(|(_, r)| !(r.state == "COMMENTED" && r.body.trim().is_empty() && (r.database_id == 0 || roots_of(r.database_id).is_empty()))).map(|(i, r)| (r.submitted_at.clone(), Item::Review(i))));
        items.extend(lines.iter().enumerate().filter(|(_, l)| l.in_reply_to_id.is_none() && !l.pull_request_review_id.is_some_and(|id| review_ids.contains(&id))).map(|(i, l)| (l.created_at.clone(), Item::Thread(i))));
        items.extend(d.commits.iter().map(|c| (c.committed_date.clone(), Item::Commit(c.oid.clone(), c.message_headline.clone(), c.authors.first().map(|a| a.login.clone()).unwrap_or_default()))));
        items.extend(d.force_pushes.iter().enumerate().map(|(i, f)| (f.at.clone(), Item::ForcePush(i))));
        items.sort_by(|a, b| a.0.cmp(&b.0));
        let ago = |when: &str| crate::pr_panel::github_time(when).map(crate::util::relative_time).unwrap_or_default();
        let description = md(0).filter(|_| !d.body.trim().is_empty()).or_else(|| Some(Rc::new(Markdown::parse_doc("*No description provided.*"))));
        let mut timeline: Vec<AnyElement> = Vec::new();
        // Commits one after another make one group: "who added N commits", like GitHub.
        let mut group: Vec<(String, String, String, String)> = Vec::new();
        let mut items = items.into_iter().peekable();
        while let Some((when, item)) = items.next() {
            if let Item::Commit(sha, headline, who) = item {
                group.push((when, sha, headline, who));
                if matches!(items.peek(), Some((_, Item::Commit(..)))) {
                    continue;
                }
                let commits = std::mem::take(&mut group);
                timeline.push(self.commit_group(commits, cx));
                continue;
            }
            timeline.push(match item {
                Item::Comment(i) => {
                    let c = &d.comments[i];
                    self.card(root, c.author.login.clone(), "commented".into(), ago(&when), None, md(1 + i), Some((Editable::Comment(c.id.clone()), c.body.clone(), true)), Vec::new(), cx).into_any_element()
                }
                Item::Review(i) => {
                    let r = &d.reviews[i];
                    let did = match r.state.as_str() {
                        "APPROVED" => "approved these changes",
                        "CHANGES_REQUESTED" => "requested changes",
                        "DISMISSED" => "had a review dismissed",
                        _ => "reviewed",
                    };
                    let threads: Vec<AnyElement> = if r.database_id == 0 { Vec::new() } else { roots_of(r.database_id).into_iter().map(|ix| self.conversation_thread(ix, lines, cx)).collect() };
                    let body = md(1 + n_comments + i).filter(|_| !r.body.trim().is_empty());
                    let edit = (!r.body.trim().is_empty() && !r.id.is_empty()).then(|| (Editable::Review(r.id.clone()), r.body.clone(), false));
                    self.card(root, r.author.login.clone(), did.into(), ago(&when), None, body, edit, threads, cx).into_any_element()
                }
                Item::Thread(ix) => self.conversation_thread(ix, lines, cx),
                Item::Commit(..) => unreachable!(),
                Item::ForcePush(i) => {
                    let f = &d.force_pushes[i];
                    let sel = format!("pr-force-push-{i}");
                    div()
                        .debug_selector(move || sel)
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .pl(px(4.))
                        .h(px(40.))
                        .child(timeline_dot(ui::small_icon("arrow-up", theme::fg_muted()).into_any_element()))
                        .child(div().flex_none().text_color(theme::fg_bright()).font_weight(FontWeight::SEMIBOLD).child(f.actor.clone()))
                        .child(div().flex_none().text_color(theme::fg_dim()).child(format!("force-pushed the {} branch from", d.head_ref_name)))
                        .child(div().flex_none().font_family(theme::MONO_FONT).text_size(px(11.)).text_color(theme::accent()).child(f.before.clone()))
                        .child(div().flex_none().text_color(theme::fg_dim()).child("to"))
                        .child(div().flex_none().font_family(theme::MONO_FONT).text_size(px(11.)).text_color(theme::accent()).child(f.after.clone()))
                        .child(div().flex_none().text_color(theme::fg_muted()).child(ago(&f.at)))
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
                    .gap(px(20.))
                    .p(px(24.))
                    .w_full()
                    .max_w(px(1200.))
                    .child(self.card(root, d.author.login.clone(), "opened it".into(), String::new(), None, description, Some((Editable::Body, d.body.clone(), false)), Vec::new(), cx))
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
                                    .when(!self.comment_input.read(cx).text().is_empty(), |d| d.child(action_button("pr-comment-cancel", "Cancel", None).on_click(cx.listener(|v, _, _, cx| v.comment_input.update(cx, |e, cx| e.set_text("", cx))))))
                                    .when(d.state != "MERGED", |el| {
                                        let typed = !self.comment_input.read(cx).text().trim().is_empty();
                                        let (label, color) = match (d.state == "CLOSED", typed) {
                                            (true, false) => ("Reopen pull request", theme::git_added()),
                                            (true, true) => ("Reopen with comment", theme::git_added()),
                                            (false, false) => ("Close pull request", theme::git_deleted()),
                                            (false, true) => ("Close with comment", theme::git_deleted()),
                                        };
                                        el.child(
                                            self.send_button("pr-close", label, if d.state == "CLOSED" { "Reopening…" } else { "Closing…" }, "close", None)
                                                .text_color(color)
                                                .tooltip(ui::tooltip("On GitHub (the comment in the box goes first)"))
                                                .on_click(cx.listener(|v, _, _, cx| v.close_or_reopen(cx))),
                                        )
                                    })
                                    .child(self.send_button("pr-comment-post", "Comment", "Commenting…", "comment", Some(theme::git_added())).on_click(cx.listener(|v, _, _, cx| v.post_comment(cx)))),
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
                .px(px(16.))
                .py(px(14.))
                .border_b_1()
                .border_color(theme::border())
                .child(div().flex_none().size(px(30.)).rounded(px(13.)).flex().items_center().justify_center().bg(color.opacity(0.18)).text_color(color).child(icon))
                .child(div().flex().flex_col().min_w_0().child(div().text_color(theme::fg_bright()).font_weight(FontWeight::SEMIBOLD).child(title)).when(!sub.is_empty(), |el| el.child(div().text_size(px(11.)).text_color(theme::fg_muted()).child(sub))))
        };
        let text = |s: &'static str| div().font_weight(FontWeight::BOLD).child(s).into_any_element();
        let mut card = div().id("pr-merge-box").debug_selector(|| "pr-merge-box".into()).flex().flex_col().rounded(px(8.)).border_1().border_color(theme::border()).overflow_hidden().bg(theme::bg_card());

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
                card = card.child(div().id("pr-checks").flex().flex_col().max_h(px(260.)).overflow_y_scroll().border_b_1().border_color(theme::border()).bg(theme::bg_card_head()).children(sorted.into_iter().enumerate().map(|(i, c)| check_row(i, c))));
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
            card = card.child(section(text(icon), title.into(), sub.into(), color).id("pr-merge-state").debug_selector(|| "pr-merge-state".into())).child(self.render_merge_action(d, can, cx));
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
        let thread = sent.first().and_then(|(_, l)| self.thread_of(l));
        let folded = thread.as_ref().is_some_and(|t| t.resolved && !self.open_threads.contains(&t.id));
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
        let reply_to = sent.first().map(|(_, l)| l.in_reply_to_id.unwrap_or(l.id)).filter(|id| *id != 0);
        let anchor = open.clone().or_else(|| {
            let (_, l) = sent.first()?;
            let new_side = l.side.as_deref() != Some("LEFT");
            Some(LineBox { path: path.to_string(), new_side, line: l.line? as usize, start: None, anchor: l.line? as usize, reply_to })
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
            .bg(theme::bg_card())
            .overflow_hidden()
            // Clicks in it stay in it (the row would take them).
            .on_click(|_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        if let Some(t) = thread.as_ref().filter(|t| t.resolved) {
            let n = sent.len();
            card = card.child(self.folded_thread(t, format!("{n} {}", if n == 1 { "comment" } else { "comments" }), !folded, cx));
        }
        if !folded {
            for (i, l) in &sent {
                card = card.child(self.line_comment_el(l, self.md.get(first + i).cloned(), cx));
            }
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
                        .child(div().text_size(px(11.)).text_color(theme::fg_muted()).child(match &self.busy {
                            Some(b) => format!("{b}…"),
                            None => format!("Comment on {} ({}) · drag the + over more lines, or Shift-click another +, for several", lines_label(b.start, b.line), if b.new_side { "new" } else { "old" }),
                        }))
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .justify_end()
                                .items_center()
                                .gap(px(6.))
                                .child(action_button("pr-line-cancel", "Cancel", None).on_click(cx.listener(|v, _, _, cx| v.close_line_box(cx))))
                                .when(reply, |d| d.child(self.send_button("pr-line-reply", "Reply", "Replying…", "line", Some(theme::git_added())).on_click(cx.listener(|v, _, _, cx| v.single_comment(cx)))))
                                .when(!reply, |d| {
                                    d.child(self.send_button("pr-line-single", "Add single comment", "Commenting…", "line", None).on_click(cx.listener(|v, _, _, cx| v.single_comment(cx))))
                                        .child(action_button("pr-line-review", review_label, Some(theme::git_added())).on_click(cx.listener(|v, _, _, cx| v.add_pending(cx))))
                                }),
                        ),
                );
            }
            None if folded => {}
            None => {
                // Reply…: answer the thread (or, with no thread, comment on the line again).
                let anchor = anchor.clone();
                let resolve = thread.as_ref().map(|t| self.resolve_button(t, cx));
                card = card.child(
                    div().flex().items_center().gap(px(8.)).pr(px(8.)).child(
                    div()
                        .id("pr-thread-reply")
                        .flex_1()
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
                        .child(if reply_to.is_some() { "Reply…" } else { "Comment on this line again…" }),
                    ).children(resolve),
                );
            }
        }
        Some(card.into_any_element())
    }

    /// Finish your review: the pending comments, a summary, and Comment, Approve or Request changes.
    fn render_review_box(&self, at: Point<Pixels>, cx: &mut Context<Self>) -> AnyElement {
        let kind = self.review_kind;
        // GitHub refuses an approval or changes requested from the pull request's author.
        let own = matches!((&self.detail, &self.viewer), (Some(Ok((d, _))), Some(me)) if &d.author.login == me);
        let option = |id: &'static str, value: &'static str, title: &'static str, sub: &'static str, cx: &mut Context<Self>| {
            let on = kind == value;
            let barred = own && value != "comment";
            let sub = if barred { "Pull request authors can't approve their own pull request or request changes on it." } else { sub };
            div()
                .id(id)
                .debug_selector(move || id.to_string())
                .flex()
                .items_start()
                .gap(px(8.))
                .py(px(3.))
                .when(barred, |d| d.opacity(0.45))
                .when(!barred, |d| {
                    d.cursor_pointer().on_click(cx.listener(move |v, _, _, cx| {
                        v.review_kind = value;
                        cx.notify();
                    }))
                })
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
                    .child(div().min_w_0().flex_1().overflow_hidden().text_ellipsis().whitespace_nowrap().text_color(theme::fg_dim()).child(p.body.lines().next().unwrap_or("").to_string()))
                    .child(
                        div()
                            .id(("pr-pending-remove", i))
                            .debug_selector(move || format!("pr-pending-remove-{i}"))
                            .flex_none()
                            .px(px(4.))
                            .cursor_pointer()
                            .text_color(theme::fg_muted())
                            .hover(|s| s.text_color(theme::git_deleted()))
                            .tooltip(ui::tooltip("Remove this pending comment"))
                            .on_click(cx.listener(move |v, _, _, cx| {
                                cx.stop_propagation();
                                v.remove_pending(i, cx);
                            }))
                            .child("×"),
                    )
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
                    .child(self.send_button("pr-submit-review", "Submit review", "Submitting…", "review", Some(theme::git_added())).on_click(cx.listener(|v, _, _, cx| v.submit_review(cx)))),
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
    // Each line with its old and new numbers, counted from the hunk's header.
    let (mut old, mut new) = (0usize, 0usize);
    let mut lines: Vec<(Option<usize>, Option<usize>, &str)> = Vec::new();
    for t in l.diff_hunk.lines() {
        if let Some(h) = t.strip_prefix("@@ ") {
            let mut it = h.split_whitespace();
            let start = |s: Option<&str>, p: char| s.and_then(|s| s.strip_prefix(p)).and_then(|s| s.split(',').next()).and_then(|s| s.parse::<usize>().ok()).unwrap_or(1);
            old = start(it.next(), '-');
            new = start(it.next(), '+');
            continue;
        }
        match t.chars().next() {
            Some('+') => {
                lines.push((None, Some(new), t));
                new += 1;
            }
            Some('-') => {
                lines.push((Some(old), None, t));
                old += 1;
            }
            _ => {
                lines.push((Some(old), Some(new), t));
                old += 1;
                new += 1;
            }
        }
    }
    if lines.is_empty() {
        return None;
    }
    let shown = &lines[lines.len().saturating_sub(n.min(40))..];
    let num = |n: Option<usize>| div().flex_none().w(px(40.)).pr(px(10.)).flex().justify_end().text_color(theme::line_number()).child(n.map(|n| n.to_string()).unwrap_or_default());
    Some(
        div()
            .flex()
            .flex_col()
            .py(px(4.))
            .bg(theme::bg_side())
            .border_b_1()
            .border_color(theme::border())
            .font_family(theme::MONO_FONT)
            .text_size(px(12.))
            .children(shown.iter().map(|(o, nw, t)| {
                let (bg, sign) = match t.chars().next() {
                    Some('+') => (Some(theme::diff_ins_line()), "+"),
                    Some('-') => (Some(theme::diff_del_line()), "-"),
                    _ => (None, " "),
                };
                let text = t.get(1..).unwrap_or("").replace('\t', "    ");
                div()
                    .flex()
                    .h(px(22.))
                    .items_center()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_color(theme::fg())
                    .when_some(bg, |d, c| d.bg(c))
                    .child(num(*o))
                    .child(num(*nw))
                    .child(div().flex_none().w(px(18.)).text_color(theme::fg_muted()).child(sign))
                    .child(div().min_w_0().overflow_hidden().text_ellipsis().child(text))
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

/// A timeline event's round mark, with its icon.
fn timeline_dot(icon: AnyElement) -> Div {
    div().flex_none().size(px(26.)).rounded(px(13.)).flex().items_center().justify_center().bg(theme::bg_side()).border_1().border_color(theme::border()).child(icon)
}

/// A small text button (Edit, Delete).
fn text_link(id: String, label: &'static str, color: Hsla) -> Stateful<Div> {
    div().id(SharedString::from(id.clone())).debug_selector(move || id).cursor_pointer().text_color(color).hover(|s| s.text_color(theme::fg_bright())).child(label)
}

/// A button: filled with `color`, else outlined.
fn action_button(id: &'static str, label: impl Into<SharedString>, color: Option<Hsla>) -> Stateful<Div> {
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
        .child(label.into())
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
            None => {
                return base
                    .child(
                        div()
                            .debug_selector(|| "pr-tab-loading".into())
                            .size_full()
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .gap(px(14.))
                            .child(crate::pr_panel::fetching("pr-tab-loading", true))
                            .child(div().text_color(theme::fg_bright()).text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(format!("Loading pull request #{}", self.number)))
                            .child(div().text_color(theme::fg_muted()).child("Downloading its conversation, checks and commits from GitHub…")),
                    )
                    .into_any_element();
            }
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
            .children(self.render_unpushed(&d, cx))
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
