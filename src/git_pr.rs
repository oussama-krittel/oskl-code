//! Reviewing a pull request in its own tab: its conversation (Markdown, Mermaid diagrams), its
//! commits, and the diff of a file picked in the sidebar's tree of what it changed; a review box
//! (comment, comment on a line, approve, request changes); checkout and merge.

use crate::commit_view::{FileRow, file_rows};
use crate::diff_view::DiffView;
use crate::editor::{Editor, EditorMode};
use crate::git::gh::{self, Checks, LineComment, PrDetail};
use crate::git::{self, Change, FileStatus, Rev};
use crate::preview::Markdown;
use crate::repo::Repo;
use crate::request::{DiffSpec, Request};
use crate::ui::px;
use crate::{theme, ui};
use gpui::prelude::*;
use gpui::*;
use std::collections::HashSet;
use std::path::Path;
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
    pub input: Entity<Editor>,
    /// The description, then each comment and review, parsed once.
    md: Vec<Rc<Markdown>>,
    images: Entity<RetainAllImageCache>,
    busy: Option<String>,
    /// The review dropdown is open, hanging from here.
    pub review_at: Option<Point<Pixels>>,
    _load: Option<Task<()>>,
    _commit_load: Option<Task<()>>,
    _act: Option<Task<()>>,
}

impl EventEmitter<Request> for PrView {}
impl EventEmitter<PrEvent> for PrView {}

impl PrView {
    pub fn new(repo: Entity<Repo>, number: u64, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            let mut ed = Editor::new(EditorMode::MultiLine, cx);
            ed.font_size = 12.;
            ed.placeholder = "Leave a comment (Markdown)".into();
            ed
        });
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
            input,
            md: Vec::new(),
            images,
            busy: None,
            review_at: None,
            _load: None,
            _commit_load: None,
            _act: None,
        };
        v.load(cx);
        v
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
                v.detail = Some(detail);
                v.files = failed.map(Err);
                v.parse_md(cx);
                cx.notify();
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

    /// The files the sidebar's tree shows: the picked commit's, else the whole pull request's.
    pub fn shown_files(&self) -> Option<&Result<Files, String>> {
        if self.commit.is_some() { self.commit_files.as_ref() } else { self.files.as_ref() }
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

    /// Shows only what `sha` changed (None: the whole pull request again).
    pub fn pick_commit(&mut self, sha: Option<String>, cx: &mut Context<Self>) {
        self.commit = sha.clone();
        self.commit_files = None;
        self.file = None;
        self.diff = None;
        let Some(sha) = sha else { return cx.notify() };
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

    /// Opens file `ix` of the shown files' diff, in Files changed.
    pub fn open_ix(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(Ok(f)) = self.shown_files() else { return };
        let Some(c) = f.list.get(ix).cloned() else { return };
        let (base, head) = (f.base.clone(), f.head.clone());
        let (root, email) = (self.repo.read(cx).root.clone(), self.repo.read(cx).user_email.clone());
        let left = if c.status == FileStatus::Added { Rev::Empty } else { Rev::Commit(base) };
        let right = if c.status == FileStatus::Deleted { Rev::Empty } else { Rev::Commit(head) };
        let spec = DiffSpec { path: c.path.clone(), old_path: c.orig_path.clone(), left, right, back: None };
        self.file = Some(c.path.clone());
        self.diff = Some(cx.new(|cx| DiffView::new(root, spec, email, false, 12., cx)));
        self.tab = Tab::Files;
        cx.notify();
    }

    /// Line comments on `path`.
    pub fn comments_on(&self, path: &str) -> usize {
        match &self.detail {
            Some(Ok((_, lines))) => lines.iter().filter(|l| l.path == path).count(),
            _ => 0,
        }
    }

    /// The picked commit's first line.
    pub fn commit_label(&self) -> Option<String> {
        let sha = self.commit.as_ref()?;
        let Some(Ok((d, _))) = &self.detail else { return Some(sha[..7.min(sha.len())].to_string()) };
        d.commits.iter().find(|c| &c.oid == sha).map(|c| format!("{} {}", &c.oid[..7.min(c.oid.len())], c.message_headline))
    }

    /// Runs a `gh` action in the background, then reloads and tells the lists.
    fn act(&mut self, label: &str, clear: bool, op: impl FnOnce(&std::path::Path) -> anyhow::Result<String> + Send + 'static, cx: &mut Context<Self>) {
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
                        v.review_at = None;
                        if clear {
                            v.input.update(cx, |e, cx| e.set_text("", cx));
                        }
                        cx.emit(PrEvent::Changed);
                        v.load(cx);
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

    /// `kind`: comment, approve, request-changes.
    pub fn review(&mut self, kind: &'static str, cx: &mut Context<Self>) {
        let (n, body) = (self.number, self.input.read(cx).text());
        if kind != "approve" && body.trim().is_empty() {
            return cx.emit(Request::Error(if kind == "comment" { "Write the comment first".into() } else { "Say what should change first".into() }));
        }
        let label = match kind {
            "approve" => "Approve",
            "request-changes" => "Request changes",
            _ => "Comment",
        };
        self.act(label, true, move |root| if kind == "comment" { gh::comment(root, n, &body) } else { gh::review(root, n, kind, &body) }, cx);
    }

    /// The comment box's text, on the line selected in the diff.
    pub fn comment_line(&mut self, cx: &mut Context<Self>) {
        let body = self.input.read(cx).text();
        let (Some(diff), Some(path), Some(Ok((d, _)))) = (&self.diff, self.file.clone(), &self.detail) else { return };
        let Some((new_side, line)) = diff.read(cx).selected_line() else {
            return cx.emit(Request::Error("Click a line of the diff first".into()));
        };
        if body.trim().is_empty() {
            return cx.emit(Request::Error("Write the comment first".into()));
        }
        let (n, commit) = (self.number, d.head_ref_oid.clone());
        self.act(&format!("Comment on line {line}"), true, move |root| gh::line_comment(root, n, &commit, &path, line, new_side, &body), cx);
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
                me.update(cx, |v, cx| v.act("Merge", false, move |root| gh::merge(root, n, how), cx)).ok();
            }),
        }));
    }

    /// One row: state, title, who merges what, the size, the checks; Check out, GitHub, Merge.
    fn header(&self, d: &PrDetail, cx: &mut Context<Self>) -> impl IntoElement {
        let (state, color) = match (d.state.as_str(), d.is_draft) {
            ("MERGED", _) => ("Merged", theme::c(0xc678dd)),
            ("CLOSED", _) => ("Closed", theme::git_deleted()),
            (_, true) => ("Draft", theme::fg_muted()),
            _ => ("Open", theme::git_added()),
        };
        let (check, check_text, check_color) = match gh::checks(&d.status_check_rollup) {
            Checks::Passing => ("✓", "Checks passing", theme::git_added()),
            Checks::Failing => ("✗", "Checks failing", theme::git_deleted()),
            Checks::Pending => ("●", "Checks running", theme::git_modified()),
            Checks::None => ("", "No checks", theme::fg_muted()),
        };
        let review = match d.review_decision.as_deref() {
            Some("APPROVED") => "approved",
            Some("CHANGES_REQUESTED") => "changes requested",
            Some("REVIEW_REQUIRED") => "review required",
            _ => "",
        };
        let button = |id: &'static str, icon: &'static str, tip: &'static str| ui::icon_button(id, icon).debug_selector(move || id.to_string()).tooltip(ui::tooltip(tip));
        let url = d.url.clone();
        let open = d.state == "OPEN";
        let tip = format!("{} wants to merge {} into {}\n{check_text}{}", d.author.login, d.head_ref_name, d.base_ref_name, if review.is_empty() { String::new() } else { format!(" · {review}") });
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
            .child(div().flex_none().text_color(theme::git_added()).child(format!("+{}", d.additions)))
            .child(div().flex_none().text_color(theme::git_deleted()).child(format!("−{}", d.deletions)))
            .when(!check.is_empty(), |el| el.child(div().flex_none().text_color(check_color).child(check)))
            .child(div().flex_1())
            .child(button("pr-checkout", "arrow-down", "Check out this pull request's branch").on_click(cx.listener(|v, _, _, cx| {
                let n = v.number;
                v.act("Check out", false, move |root| gh::checkout(root, n), cx);
            })))
            .child(button("pr-browser", "globe", "Open on GitHub").on_click(cx.listener(move |_, _, _, _| {
                let _ = std::process::Command::new("open").arg(&url).spawn();
            })))
            .when(open, |el| {
                el.child(
                    div()
                        .id("pr-merge")
                        .debug_selector(|| "pr-merge".into())
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .ml(px(2.))
                        .px(px(8.))
                        .h(px(22.))
                        .rounded(px(5.))
                        .cursor_pointer()
                        .border_1()
                        .border_color(theme::border_input())
                        .hover(|s| s.bg(theme::hover()))
                        .tooltip(ui::tooltip("Merge on GitHub, into its base branch"))
                        .child(ui::small_icon("pull-request", theme::git_added()))
                        .child("Merge…")
                        .on_click(cx.listener(|v, _, _, cx| v.ask_merge(cx))),
                )
            })
    }

    fn render_conversation(&self, root: &Path, d: &PrDetail, lines: &[LineComment]) -> impl IntoElement {
        let md = |ix: usize| self.md.get(ix).cloned();
        // Comments and reviews, oldest first; each with its Markdown (description first).
        let mut items: Vec<(String, String, String, Option<Rc<Markdown>>)> = d.comments.iter().enumerate().map(|(i, c)| (c.created_at.clone(), c.author.login.clone(), "commented".to_string(), md(1 + i))).collect();
        let n_comments = d.comments.len();
        items.extend(d.reviews.iter().enumerate().map(|(i, r)| {
            let did = match r.state.as_str() {
                "APPROVED" => "approved",
                "CHANGES_REQUESTED" => "requested changes",
                "DISMISSED" => "had a review dismissed",
                _ => "reviewed",
            };
            (r.submitted_at.clone(), r.author.login.clone(), did.to_string(), md(1 + n_comments + i).filter(|_| !r.body.trim().is_empty()))
        }));
        items.sort_by(|a, b| a.0.cmp(&b.0));
        let first_line = 1 + n_comments + d.reviews.len();
        let images = self.images.clone();
        let opener: crate::preview::Opener = Rc::new(|path, _, _, _| {
            let _ = std::process::Command::new("open").arg(path).spawn();
        });
        let card = |who: String, did: String, when: String, body: Option<Rc<Markdown>>| {
            let color = if did == "approved" { theme::git_added() } else if did == "requested changes" { theme::git_deleted() } else { theme::fg_dim() };
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
                        .child(div().text_color(theme::fg_muted()).child(when.get(..10).unwrap_or("").to_string())),
                )
                .when_some(body, |d, md| d.child(div().px(px(14.)).py(px(10.)).child(md.render(root, &images, opener.clone()))))
        };
        let description = md(0).filter(|_| !d.body.trim().is_empty());
        div()
            .id("pr-conversation")
            .debug_selector(|| "pr-conversation".into())
            .size_full()
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .p(px(16.))
                    .max_w(px(980.))
                    .child(card(d.author.login.clone(), "opened it".into(), String::new(), description.or_else(|| Some(Rc::new(Markdown::parse_doc("*No description.*"))))))
                    .children(items.into_iter().map(|(when, who, did, body)| card(who, did, when, body)))
                    .children(lines.iter().enumerate().map(|(i, l)| card(l.user.login.clone(), format!("on {}:{}", l.path, l.line.or(l.original_line).unwrap_or(0)), String::new(), md(first_line + i))))
                    .when(d.comments.is_empty() && d.reviews.is_empty() && lines.is_empty(), |d| d.child(div().id("pr-no-comments").debug_selector(|| "pr-no-comments".into()).py(px(8.)).text_center().text_color(theme::fg_muted()).child("No comments or reviews yet. Write one below."))),
            )
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

    /// The review dropdown: the comment (Markdown), on the line selected in the diff or on
    /// the whole pull request, approve or request changes.
    fn render_review_box(&self, at: Point<Pixels>, cx: &mut Context<Self>) -> AnyElement {
        let line = (self.tab == Tab::Files).then(|| self.diff.as_ref().and_then(|d| d.read(cx).selected_line())).flatten();
        let button = |id: &'static str, label: String, color: Option<Hsla>| {
            div()
                .id(id)
                .debug_selector(move || id.to_string())
                .flex_none()
                .px(px(10.))
                .h(px(24.))
                .flex()
                .items_center()
                .rounded(px(5.))
                .cursor_pointer()
                .when_some(color, |d, c| d.bg(c).text_color(theme::on(c)))
                .when(color.is_none(), |d| d.border_1().border_color(theme::border_input()).hover(|s| s.bg(theme::hover())))
                .child(label)
        };
        let card = div()
            .id("pr-review-box")
            .debug_selector(|| "pr-review-box".into())
            .occlude()
            .w(px(480.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .p(px(10.))
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
            .child(div().text_color(theme::fg_dim()).child(match line {
                Some((_, l)) => format!("Comment on line {l}, or on the whole pull request"),
                None => "Comment on the pull request, or finish your review".into(),
            }))
            .child(div().id("pr-input").debug_selector(|| "pr-input".into()).h(px(110.)).px(px(6.)).py(px(4.)).rounded(px(4.)).bg(theme::bg_input()).border_1().border_color(theme::border_input()).overflow_hidden().child(self.input.clone()))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(6.))
                    .when_some(self.busy.clone(), |d, b| d.child(div().text_color(theme::fg_muted()).child(format!("{b}…"))))
                    .child(div().flex_1())
                    .when_some(line, |d, (_, l)| d.child(button("pr-comment-line", format!("Comment on line {l}"), None).on_click(cx.listener(|v, _, _, cx| v.comment_line(cx)))))
                    .child(button("pr-comment", "Comment".into(), None).on_click(cx.listener(|v, _, _, cx| v.review("comment", cx))))
                    .child(button("pr-request-changes", "Request changes".into(), Some(theme::git_deleted())).on_click(cx.listener(|v, _, _, cx| v.review("request-changes", cx))))
                    .child(button("pr-approve", "Approve".into(), Some(theme::git_added())).on_click(cx.listener(|v, _, _, cx| v.review("approve", cx)))),
            );
        deferred(anchored().anchor(Anchor::TopRight).position(at).snap_to_window().child(card)).with_priority(2).into_any_element()
    }
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
                .on_click(cx.listener(move |v, _, _, cx| {
                    v.tab = which;
                    cx.notify();
                }))
        };
        let root = self.repo.read(cx).root.clone();
        let body: AnyElement = match self.tab {
            Tab::Conversation => self.render_conversation(&root, &d, &lines).into_any_element(),
            Tab::Commits => self.render_commits(&d, cx).into_any_element(),
            Tab::Files => match (&self.diff, self.shown_files()) {
                (Some(diff), _) => diff.clone().into_any_element(),
                (None, Some(Err(e))) => ui::empty_state("warning", "Couldn't list the files", e.clone()).into_any_element(),
                (None, None) => ui::empty_state("refresh", "Bringing its commits here…", "The files show in the sidebar once they're here.").into_any_element(),
                (None, Some(Ok(f))) if f.list.is_empty() => ui::empty_state("files", "No files changed", "").into_any_element(),
                (None, Some(Ok(_))) => ui::empty_state("files", "Pick a file", "Its diff shows here. The sidebar lists every file this pull request changed.").into_any_element(),
            },
        };
        let (tc, cur) = (self.tab, self.commit_label());
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
                    .child({
                        let line = (tc == Tab::Files).then(|| self.diff.as_ref().and_then(|d| d.read(cx).selected_line())).flatten();
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
                            .bg(theme::accent())
                            .text_color(theme::on(theme::accent()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .tooltip(ui::tooltip("Comment, approve or request changes"))
                            .on_click(cx.listener(|v, ev: &ClickEvent, window, cx| {
                                v.review_at = if v.review_at.is_some() { None } else { Some(point(ev.position().x.max(gpui::px(500.)), ev.position().y + gpui::px(14.))) };
                                if v.review_at.is_some() {
                                    window.focus(&v.input.focus_handle(cx), cx);
                                }
                                cx.notify();
                            }))
                            .child(match line {
                                Some((_, l)) => format!("Review · line {l}"),
                                None => "Review".into(),
                            })
                            .child(ui::small_icon("chevron-down", theme::on(theme::accent())))
                    })
                    .when_some(cur, |el, label| {
                        el.child(
                            div()
                                .id("pr-commit-chip")
                                .debug_selector(|| "pr-commit-chip".into())
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .max_w(px(320.))
                                .px(px(8.))
                                .h(px(22.))
                                .rounded(px(11.))
                                .bg(theme::hover())
                                .cursor_pointer()
                                .tooltip(ui::tooltip("Show the whole pull request again"))
                                .on_click(cx.listener(|v, _, _, cx| v.pick_commit(None, cx)))
                                .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_color(theme::fg_dim()).child(label))
                                .child(div().flex_none().text_color(theme::fg_muted()).child("×")),
                        )
                    }),
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
