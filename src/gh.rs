//! GitHub pull requests through the `gh` CLI (its sign-in, hosts and tokens, like a terminal).
//! `OSKL_GH` names another program to run instead (the tests' fake).

use super::run as git_run;
use anyhow::{Result, bail};
use serde::Deserialize;
use std::path::Path;
use std::process::Stdio;

/// The installed `gh`: on the login shell's PATH, else where Homebrew and the installer put it
/// (from the Dock the app's own PATH has neither).
fn gh_bin() -> std::path::PathBuf {
    if let Some(bin) = std::env::var_os("OSKL_GH") {
        return bin.into();
    }
    let dirs = crate::tools::search_path().into_iter().chain(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(Into::into));
    dirs.map(|d| d.join("gh")).find(|p| p.is_file()).unwrap_or_else(|| "gh".into())
}

fn gh(root: &Path, args: &[&str]) -> Result<String> {
    let out = crate::tools::command(&gh_bin()).current_dir(root).env("GH_PROMPT_DISABLED", "1").env("NO_COLOR", "1").args(args).stdin(Stdio::null()).output()?;
    if !out.status.success() {
        // `gh api` says only "Unprocessable Entity (HTTP 422)": GitHub's reason is in the answer.
        let why = serde_json::from_slice::<serde_json::Value>(&out.stdout).ok().map(|j| {
            let mut parts: Vec<String> = Vec::new();
            if let Some(m) = j.get("message").and_then(|m| m.as_str()) {
                parts.push(m.to_string());
            }
            for e in j.get("errors").and_then(|e| e.as_array()).into_iter().flatten() {
                match e.as_str().or_else(|| e.get("message").and_then(|m| m.as_str())) {
                    Some(m) => parts.push(m.to_string()),
                    None => parts.push(e.to_string()),
                }
            }
            parts.join(": ")
        });
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        match why.filter(|w| !w.is_empty()) {
            Some(w) => bail!("{w} ({})", err.trim_start_matches("gh: ")),
            None => bail!("{err}"),
        }
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Author {
    #[serde(default)]
    pub login: String,
}

/// A check run or a commit status (they name their outcome differently).
#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Check {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub context: String,
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    /// The workflow a check run belongs to ("CI" in "CI / build").
    #[serde(default, rename = "workflowName")]
    pub workflow_name: String,
    /// Where a check run's log is (a commit status names its `targetUrl`).
    #[serde(default, rename = "detailsUrl")]
    pub details_url: String,
    #[serde(default, rename = "targetUrl")]
    pub target_url: String,
    #[serde(default)]
    pub description: String,
}

/// Where one check stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CheckState {
    Failing,
    Running,
    Queued,
    Passed,
    Skipped,
}

impl Check {
    pub fn name(&self) -> String {
        let name = if self.name.is_empty() { &self.context } else { &self.name };
        if self.workflow_name.is_empty() { name.clone() } else { format!("{} / {name}", self.workflow_name) }
    }

    /// Its page on GitHub (the run's log), if it has one.
    pub fn url(&self) -> Option<&str> {
        [&self.details_url, &self.target_url].into_iter().find(|u| !u.is_empty()).map(String::as_str)
    }

    pub fn state(&self) -> CheckState {
        let outcome = self.conclusion.clone().filter(|c| !c.is_empty()).or(self.state.clone()).unwrap_or_default().to_uppercase();
        match outcome.as_str() {
            "FAILURE" | "ERROR" | "CANCELLED" | "TIMED_OUT" | "ACTION_REQUIRED" | "STARTUP_FAILURE" => CheckState::Failing,
            "SUCCESS" => CheckState::Passed,
            "NEUTRAL" | "SKIPPED" | "STALE" => CheckState::Skipped,
            "EXPECTED" | "PENDING" => CheckState::Queued,
            _ => match self.status.as_deref().unwrap_or("").to_uppercase().as_str() {
                "IN_PROGRESS" => CheckState::Running,
                _ => CheckState::Queued,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Checks {
    None,
    Passing,
    Failing,
    Pending,
}

pub fn checks(list: &[Check]) -> Checks {
    if list.is_empty() {
        return Checks::None;
    }
    if list.iter().any(|c| c.state() == CheckState::Failing) {
        Checks::Failing
    } else if list.iter().all(|c| matches!(c.state(), CheckState::Passed | CheckState::Skipped)) {
        Checks::Passing
    } else {
        Checks::Pending
    }
}

#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Pr {
    pub number: u64,
    pub title: String,
    pub author: Author,
    pub head_ref_name: String,
    pub base_ref_name: String,
    pub is_draft: bool,
    pub review_decision: Option<String>,
    pub status_check_rollup: Vec<Check>,
    pub updated_at: String,
    /// OPEN, MERGED or CLOSED.
    pub state: String,
}

#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Comment {
    /// GitHub's node id (to edit or delete it).
    pub id: String,
    pub author: Author,
    pub body: String,
    pub created_at: String,
}

#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Review {
    /// GitHub's node id (to edit its text).
    pub id: String,
    /// Its number in the REST API, which the comments on lines name.
    #[serde(skip)]
    pub database_id: u64,
    pub author: Author,
    pub state: String,
    pub body: String,
    pub submitted_at: String,
}

#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct PrDetail {
    pub number: u64,
    pub title: String,
    pub body: String,
    pub author: Author,
    pub state: String,
    pub url: String,
    pub head_ref_name: String,
    pub base_ref_name: String,
    pub head_ref_oid: String,
    pub additions: u64,
    pub deletions: u64,
    pub is_draft: bool,
    pub review_decision: Option<String>,
    pub mergeable: String,
    /// CLEAN, BLOCKED, BEHIND, DIRTY, DRAFT, UNSTABLE, HAS_HOOKS or UNKNOWN (still working it out).
    pub merge_state_status: String,
    pub comments: Vec<Comment>,
    pub reviews: Vec<Review>,
    pub status_check_rollup: Vec<Check>,
    pub commits: Vec<PrCommit>,
    /// Threads of comments on lines, resolved or not.
    #[serde(skip)]
    pub threads: Vec<Thread>,
    /// Its branch pushed over (force-pushed): who, from which commit to which, when.
    #[serde(skip)]
    pub force_pushes: Vec<ForcePush>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ForcePush {
    pub actor: String,
    pub before: String,
    pub after: String,
    pub at: String,
}

/// A thread of comments on lines: resolved or not, and the comment it starts with.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Thread {
    pub id: String,
    pub resolved: bool,
    pub root: u64,
}

/// One of a pull request's commits.
#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct PrCommit {
    pub oid: String,
    pub message_headline: String,
    pub authors: Vec<Author>,
    pub committed_date: String,
}

/// A comment on a line of the diff.
#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct LineComment {
    pub id: u64,
    /// The review it was sent with.
    pub pull_request_review_id: Option<u64>,
    /// The thread's first comment, for a reply.
    pub in_reply_to_id: Option<u64>,
    pub created_at: String,
    pub path: String,
    pub line: Option<u64>,
    pub original_line: Option<u64>,
    /// A comment on several lines: the first of them (to `line`), on `start_side`.
    pub start_line: Option<u64>,
    pub start_side: Option<String>,
    /// The diff up to the comment's line (its last lines are what it's about).
    pub diff_hunk: String,
    pub side: Option<String>,
    pub body: String,
    pub user: Author,
}

/// The pull requests in `state`: open, merged, closed (without merging) or all.
pub fn list(root: &Path, state: &str) -> Result<Vec<Pr>> {
    let out = gh(root, &["pr", "list", "--state", state, "--limit", "50", "--json", "number,title,author,headRefName,baseRefName,isDraft,reviewDecision,statusCheckRollup,updatedAt,state"])?;
    let mut prs: Vec<Pr> = serde_json::from_str(&out)?;
    // gh counts a merged pull request as closed too.
    if state == "closed" {
        prs.retain(|p| p.state != "MERGED");
    }
    Ok(prs)
}

pub fn view(root: &Path, n: u64) -> Result<(PrDetail, Vec<LineComment>)> {
    let fields = "number,title,body,author,state,url,headRefName,baseRefName,headRefOid,additions,deletions,isDraft,reviewDecision,mergeable,mergeStateStatus,comments,reviews,statusCheckRollup,commits";
    let mut detail: PrDetail = serde_json::from_str(&gh(root, &["pr", "view", &n.to_string(), "--json", fields])?)?;
    // Line comments aren't in `pr view`; without them the review still works.
    let lines = gh(root, &["api", &format!("repos/{{owner}}/{{repo}}/pulls/{n}/comments?per_page=100")]).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    // The reviews' numbers, which their comments on lines name.
    #[derive(Deserialize)]
    struct Rest {
        id: u64,
        node_id: String,
    }
    let rest: Vec<Rest> = gh(root, &["api", &format!("repos/{{owner}}/{{repo}}/pulls/{n}/reviews?per_page=100")]).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    for r in &mut detail.reviews {
        r.database_id = rest.iter().find(|x| x.node_id == r.id).map(|x| x.id).unwrap_or(0);
    }
    if let Ok((threads, pushes)) = threads(root, n) {
        detail.threads = threads;
        detail.force_pushes = pushes;
    }
    Ok((detail, lines))
}

/// The pull request's threads of line comments (resolved or not), and its force pushes.
fn threads(root: &Path, n: u64) -> Result<(Vec<Thread>, Vec<ForcePush>)> {
    let query = "query($owner: String!, $name: String!, $n: Int!) { repository(owner: $owner, name: $name) { pullRequest(number: $n) { reviewThreads(first: 100) { nodes { id isResolved comments(first: 1) { nodes { databaseId } } } } timelineItems(itemTypes: [HEAD_REF_FORCE_PUSHED_EVENT], last: 50) { nodes { ... on HeadRefForcePushedEvent { actor { login } beforeCommit { abbreviatedOid } afterCommit { abbreviatedOid } createdAt } } } } } }";
    let out = gh(root, &["api", "graphql", "-F", "owner={owner}", "-F", "name={repo}", "-F", &format!("n={n}"), "-f", &format!("query={query}")])?;
    let j: serde_json::Value = serde_json::from_str(&out)?;
    let nodes = j.pointer("/data/repository/pullRequest/reviewThreads/nodes").and_then(|n| n.as_array()).cloned().unwrap_or_default();
    let threads = nodes
        .iter()
        .map(|t| Thread {
            id: t["id"].as_str().unwrap_or_default().to_string(),
            resolved: t["isResolved"].as_bool().unwrap_or(false),
            root: t.pointer("/comments/nodes/0/databaseId").and_then(|d| d.as_u64()).unwrap_or(0),
        })
        .collect();
    let s = |v: &serde_json::Value, p: &str| v.pointer(p).and_then(|x| x.as_str()).unwrap_or_default().to_string();
    let pushes = j
        .pointer("/data/repository/pullRequest/timelineItems/nodes")
        .and_then(|n| n.as_array())
        .into_iter()
        .flatten()
        .map(|e| ForcePush { actor: s(e, "/actor/login"), before: s(e, "/beforeCommit/abbreviatedOid"), after: s(e, "/afterCommit/abbreviatedOid"), at: s(e, "/createdAt") })
        .filter(|f| !f.at.is_empty())
        .collect();
    Ok((threads, pushes))
}

/// Who gh is signed in as.
pub fn viewer(root: &Path) -> Option<String> {
    gh(root, &["api", "user", "--jq", ".login"]).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn graphql(root: &Path, query: &str, vars: &[(&str, &str)]) -> Result<String> {
    let mut args: Vec<String> = vec!["api".into(), "graphql".into(), "-f".into(), format!("query={query}")];
    for (k, v) in vars {
        args.extend(["-f".into(), format!("{k}={v}")]);
    }
    gh(root, &args.iter().map(String::as_str).collect::<Vec<_>>())
}

/// What can be edited or deleted on the page.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Editable {
    /// The pull request's description.
    Body,
    /// A comment on the conversation (its node id).
    Comment(String),
    /// A review's text (its node id).
    Review(String),
    /// A comment on a line (its number).
    Line(u64),
}

pub fn edit(root: &Path, n: u64, what: &Editable, body: &str) -> Result<String> {
    match what {
        Editable::Body => gh(root, &["pr", "edit", &n.to_string(), "--body", body]),
        Editable::Comment(id) => graphql(root, "mutation($id: ID!, $body: String!) { updateIssueComment(input: {id: $id, body: $body}) { clientMutationId } }", &[("id", id), ("body", body)]),
        Editable::Review(id) => graphql(root, "mutation($id: ID!, $body: String!) { updatePullRequestReview(input: {pullRequestReviewId: $id, body: $body}) { clientMutationId } }", &[("id", id), ("body", body)]),
        Editable::Line(id) => gh(root, &["api", "--method", "PATCH", &format!("repos/{{owner}}/{{repo}}/pulls/comments/{id}"), "-f", &format!("body={body}")]),
    }
}

pub fn delete(root: &Path, what: &Editable) -> Result<String> {
    match what {
        Editable::Comment(id) => graphql(root, "mutation($id: ID!) { deleteIssueComment(input: {id: $id}) { clientMutationId } }", &[("id", id)]),
        Editable::Line(id) => gh(root, &["api", "--method", "DELETE", &format!("repos/{{owner}}/{{repo}}/pulls/comments/{id}")]),
        _ => bail!("This can't be deleted"),
    }
}

/// Resolves the thread `id` (or opens it again).
pub fn resolve(root: &Path, id: &str, resolved: bool) -> Result<String> {
    let m = if resolved { "resolveReviewThread" } else { "unresolveReviewThread" };
    graphql(root, &format!("mutation($id: ID!) {{ {m}(input: {{threadId: $id}}) {{ clientMutationId }} }}"), &[("id", id)])
}

/// Brings the PR's commits here (as `refs/oskl/pr/<n>`) and its base up to date; returns
/// (base, head) to diff.
pub fn fetch(root: &Path, remote: &str, n: u64, base: &str) -> Result<(String, String)> {
    let head = format!("refs/oskl/pr/{n}");
    // Only gh's sign-in: git's own helpers (the keychain, Git Credential Manager) can wait
    // forever on a prompt nobody sees when the app runs them.
    let args = with_gh_auth(&["fetch", "--quiet", remote, &format!("+refs/pull/{n}/head:{head}"), &format!("+refs/heads/{base}:refs/remotes/{remote}/{base}")]);
    git_run(root, &args.iter().map(String::as_str).collect::<Vec<_>>())?;
    Ok((format!("refs/remotes/{remote}/{base}"), head))
}

/// Git's own credential helpers swapped for gh's sign-in: the keychain or Git Credential
/// Manager can wait forever on a prompt nobody sees when the app runs them.
fn with_gh_auth(args: &[&str]) -> Vec<String> {
    let mut all: Vec<String> = Vec::new();
    if std::env::var_os("OSKL_GH").is_none() {
        all.extend(["-c".into(), "credential.helper=".into(), "-c".into(), format!("credential.helper=!'{}' auth git-credential", gh_bin().display())]);
    }
    all.extend(args.iter().map(|a| a.to_string()));
    all
}

/// The branch pull requests go into by default: the remote's HEAD, else main or master.
pub fn default_branch(root: &Path) -> String {
    if let Ok(head) = git_run(root, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]) {
        let head = head.trim();
        if let Some((_, b)) = head.split_once('/') {
            return b.to_string();
        }
    }
    ["main", "master", "develop"]
        .into_iter()
        .find(|b| git_run(root, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{b}")]).is_ok())
        .unwrap_or("main")
        .to_string()
}

/// A title and description to start from: one commit's subject and body, or (several) the
/// branch's name in words.
pub fn suggested_text(root: &Path, base: &str, head: &str) -> (String, String) {
    let subjects = git_run(root, &["log", "--format=%s", &format!("{base}..{head}")]).unwrap_or_default();
    let subjects: Vec<&str> = subjects.lines().filter(|l| !l.trim().is_empty()).collect();
    if subjects.len() == 1 {
        let body = git_run(root, &["log", "-1", "--format=%b", head]).unwrap_or_default();
        return (subjects[0].to_string(), body.trim().to_string());
    }
    let name = head.rsplit('/').next().unwrap_or(head).replace(['-', '_'], " ");
    let mut c = name.chars();
    let title = c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default();
    (title, String::new())
}

/// The open pull request from `head`, if there is one: its number.
pub fn open_from(root: &Path, head: &str) -> Option<u64> {
    let out = gh(root, &["pr", "list", "--head", head, "--state", "open", "--json", "number"]).ok()?;
    let prs: Vec<serde_json::Value> = serde_json::from_str(&out).ok()?;
    prs.first()?.get("number")?.as_u64()
}

/// Opens a pull request from `head` into `base`, pushing `head` first (its upstream set);
/// returns its number.
pub fn create(root: &Path, base: &str, head: &str, title: &str, body: &str, draft: bool) -> Result<u64> {
    let remote = git_run(root, &["config", &format!("branch.{head}.remote")]).ok().map(|r| r.trim().to_string()).filter(|r| !r.is_empty()).unwrap_or_else(|| "origin".into());
    let push = with_gh_auth(&["push", "--quiet", "-u", &remote, head]);
    git_run(root, &push.iter().map(String::as_str).collect::<Vec<_>>())?;
    let mut args = vec!["pr", "create", "--base", base, "--head", head, "--title", title, "--body", body];
    if draft {
        args.push("--draft");
    }
    let out = gh(root, &args)?;
    // gh prints the new pull request's address, ending in its number.
    out.lines()
        .rev()
        .find_map(|l| l.trim().rsplit('/').next().and_then(|n| n.parse().ok()))
        .ok_or_else(|| anyhow::anyhow!("gh didn't say which pull request it opened: {}", out.trim()))
}

/// `kind`: approve, request-changes or comment.
pub fn review(root: &Path, n: u64, kind: &str, body: &str) -> Result<String> {
    let (num, flag) = (n.to_string(), format!("--{kind}"));
    let mut args = vec!["pr", "review", num.as_str(), flag.as_str()];
    if !body.trim().is_empty() {
        args.extend(["--body", body]);
    }
    gh(root, &args)
}

pub fn comment(root: &Path, n: u64, body: &str) -> Result<String> {
    gh(root, &["pr", "comment", &n.to_string(), "--body", body])
}

/// A comment on `line` of `path` (new side, or the old one for a removed line).
/// From line `start` to `line`, for a comment on several.
pub fn line_comment(root: &Path, n: u64, commit: &str, path: &str, start: Option<usize>, line: usize, new_side: bool, body: &str) -> Result<String> {
    let side = if new_side { "RIGHT" } else { "LEFT" };
    let range: Vec<String> = match start.filter(|s| *s < line) {
        Some(s) => vec!["-F".into(), format!("start_line={s}"), "-f".into(), format!("start_side={side}")],
        None => Vec::new(),
    };
    let (url, body, commit, path, line) = (format!("repos/{{owner}}/{{repo}}/pulls/{n}/comments"), format!("body={body}"), format!("commit_id={commit}"), format!("path={path}"), format!("line={line}"));
    let mut args: Vec<&str> = vec!["api", "--method", "POST", &url, "-f", &body, "-f", &commit, "-f", &path, "-F", &line, "-f", if new_side { "side=RIGHT" } else { "side=LEFT" }];
    args.extend(range.iter().map(String::as_str));
    gh(root, &args)
}

/// A comment of a review not sent yet: on `line` of `path`, new side or (a removed line) the old one.
#[derive(serde::Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DraftComment {
    pub path: String,
    pub line: usize,
    pub new_side: bool,
    pub body: String,
    /// On several lines: the first (to `line`).
    #[serde(default)]
    pub start_line: Option<usize>,
}

/// A part of a file's diff, as git (and GitHub) cut it: (old start, old count, new start, new
/// count). Only lines in one take comments.
pub type Hunk = (usize, usize, usize, usize);

/// The parts of `path`'s diff from `base` to `head`.
pub fn hunks(root: &Path, base: &str, head: &str, path: &str) -> Vec<Hunk> {
    let out = git_run(root, &["diff", "-U3", "--no-color", "--no-ext-diff", base, head, "--", path]).unwrap_or_default();
    out.lines().filter_map(|l| parse_hunk_header(l.strip_prefix("@@ ")?)).collect()
}

fn parse_hunk_header(h: &str) -> Option<Hunk> {
    let mut it = h.split_whitespace();
    let side = |s: &str| -> Option<(usize, usize)> {
        let (a, n) = s.split_once(',').map(|(a, n)| (a, n.parse().ok())).unwrap_or((s, Some(1)));
        Some((a.parse().ok()?, n?))
    };
    let (os, oc) = side(it.next()?.strip_prefix('-')?)?;
    let (ns, nc) = side(it.next()?.strip_prefix('+')?)?;
    Some((os, oc, ns, nc))
}

/// Which part `line` of a side is in.
pub fn hunk_index(hunks: &[Hunk], new_side: bool, line: usize) -> Option<usize> {
    hunks.iter().position(|&(os, oc, ns, nc)| {
        let (s, c) = if new_side { (ns, nc) } else { (os, oc) };
        line >= s && line < s + c
    })
}

/// GitHub takes this comment: its line, and its first line, in the same part of the diff.
pub fn fits(hunks: &[Hunk], c: &DraftComment) -> bool {
    let end = hunk_index(hunks, c.new_side, c.line);
    end.is_some() && c.start_line.filter(|s| *s < c.line).is_none_or(|s| hunk_index(hunks, c.new_side, s) == end)
}

/// Sends a review (see `submit_review`) of the diff `base`..`head`, after checking each comment
/// against it: one GitHub wouldn't take on its line goes into the review's text instead, so
/// the review isn't refused. Says how many went there.
#[allow(clippy::too_many_arguments)]
pub fn send_review(root: &Path, n: u64, base: &str, head: &str, event: &str, body: &str, comments: &[DraftComment]) -> Result<usize> {
    let mut parts: std::collections::HashMap<&str, Vec<Hunk>> = Default::default();
    let (mut ok, mut moved) = (Vec::new(), Vec::new());
    for c in comments {
        let h = parts.entry(&c.path).or_insert_with(|| hunks(root, base, head, &c.path));
        if fits(h, c) { ok.push(c.clone()) } else { moved.push(c) }
    }
    let mut text = body.trim().to_string();
    if !moved.is_empty() {
        text.push_str(if text.is_empty() { "" } else { "\n\n" });
        for c in &moved {
            let at = match c.start_line.filter(|s| *s < c.line) {
                Some(s) => format!("{}:{s}-{}", c.path, c.line),
                None => format!("{}:{}", c.path, c.line),
            };
            text.push_str(&format!("- `{at}`: {}\n", c.body.trim()));
        }
    }
    submit_review(root, n, head, event, &text, &ok)?;
    Ok(moved.len())
}

/// Sends a review: its comments on lines and `body`, as `event` (COMMENT, APPROVE or
/// REQUEST_CHANGES), on the pull request at `commit`.
pub fn submit_review(root: &Path, n: u64, commit: &str, event: &str, body: &str, comments: &[DraftComment]) -> Result<String> {
    let comments: Vec<serde_json::Value> = comments
        .iter()
        .map(|c| {
            let side = if c.new_side { "RIGHT" } else { "LEFT" };
            let mut j = serde_json::json!({"path": c.path, "line": c.line, "side": side, "body": c.body});
            if let Some(s) = c.start_line.filter(|s| *s < c.line) {
                j["start_line"] = s.into();
                j["start_side"] = side.into();
            }
            j
        })
        .collect();
    let mut review = serde_json::json!({"commit_id": commit, "event": event, "comments": comments});
    if !body.trim().is_empty() {
        review["body"] = body.into();
    }
    let file = std::env::temp_dir().join(format!("oskl-review-{}-{n}.json", std::process::id()));
    std::fs::write(&file, review.to_string())?;
    let out = gh(root, &["api", "--method", "POST", &format!("repos/{{owner}}/{{repo}}/pulls/{n}/reviews"), "--input", &file.to_string_lossy()]);
    let _ = std::fs::remove_file(&file);
    out
}

/// An answer in the thread that comment `to` starts.
pub fn reply(root: &Path, n: u64, to: u64, body: &str) -> Result<String> {
    gh(root, &["api", "--method", "POST", &format!("repos/{{owner}}/{{repo}}/pulls/{n}/comments/{to}/replies"), "-f", &format!("body={body}")])
}

/// Closes the pull request (with a comment first, when there's one), or opens it again.
pub fn set_open(root: &Path, n: u64, open: bool, comment: &str) -> Result<String> {
    let num = n.to_string();
    if !comment.trim().is_empty() {
        gh(root, &["pr", "comment", &num, "--body", comment])?;
    }
    gh(root, &["pr", if open { "reopen" } else { "close" }, &num])
}

pub fn checkout(root: &Path, n: u64) -> Result<String> {
    gh(root, &["pr", "checkout", &n.to_string()])
}

/// `how`: merge, squash or rebase.
pub fn merge(root: &Path, n: u64, how: &str) -> Result<String> {
    gh(root, &["pr", "merge", &n.to_string(), &format!("--{how}")])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_sum_up_runs_and_statuses() {
        let c = |conclusion: Option<&str>, state: Option<&str>| Check { conclusion: conclusion.map(String::from), state: state.map(String::from), ..Default::default() };
        assert_eq!(checks(&[]), Checks::None);
        assert_eq!(checks(&[c(Some("SUCCESS"), None), c(None, Some("SUCCESS"))]), Checks::Passing);
        assert_eq!(checks(&[c(Some("SUCCESS"), None), c(Some("FAILURE"), None)]), Checks::Failing);
        assert_eq!(checks(&[c(Some("SUCCESS"), None), c(None, Some("PENDING"))]), Checks::Pending);
    }

    #[test]
    fn comments_fit_only_in_one_part_of_the_diff() {
        assert_eq!(parse_hunk_header("-34,6 +34,7 @@ mod java;"), Some((34, 6, 34, 7)));
        assert_eq!(parse_hunk_header("-0,0 +1 @@"), Some((0, 0, 1, 1)));
        let h = [(15, 7, 15, 8), (149, 8, 149, 8), (159, 12, 159, 46)];
        let c = |start: Option<usize>, line: usize| DraftComment { path: "a".into(), line, new_side: true, body: String::new(), start_line: start };
        assert!(fits(&h, &c(None, 20)));
        assert!(!fits(&h, &c(Some(20), 29)), "runs past its part");
        assert!(!fits(&h, &c(Some(155), 186)), "two parts");
        assert!(fits(&h, &c(Some(160), 170)));
        assert!(!fits(&h, &c(None, 1)));
    }

    #[test]
    fn finds_the_installed_gh() {
        let bin = gh_bin();
        assert!(bin.is_file() || bin == Path::new("gh") || std::env::var_os("OSKL_GH").is_some(), "{bin:?}");
    }

    #[test]
    fn reads_gh_json() {
        let prs: Vec<Pr> = serde_json::from_str(r#"[{"number":7,"title":"Login","author":{"login":"ann"},"headRefName":"login","baseRefName":"main","isDraft":false,"reviewDecision":"APPROVED","statusCheckRollup":[{"__typename":"CheckRun","name":"ci","conclusion":"SUCCESS","status":"COMPLETED"}],"updatedAt":"2026-11-14T10:00:00Z"}]"#).unwrap();
        assert_eq!(prs[0].number, 7);
        assert_eq!(prs[0].author.login, "ann");
        assert_eq!(checks(&prs[0].status_check_rollup), Checks::Passing);
    }
}
