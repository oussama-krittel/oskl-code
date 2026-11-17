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
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
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
    pub author: Author,
    pub body: String,
    pub created_at: String,
}

#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Review {
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
    let detail: PrDetail = serde_json::from_str(&gh(root, &["pr", "view", &n.to_string(), "--json", fields])?)?;
    // Line comments aren't in `pr view`; without them the review still works.
    let lines = gh(root, &["api", &format!("repos/{{owner}}/{{repo}}/pulls/{n}/comments")]).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    Ok((detail, lines))
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
