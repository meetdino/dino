//! Pull requests through the GitHub CLI: open one for a session's branch, follow its checks, merge it.
//!
//! Every call goes through `gh` on `PATH`, so it uses the user's own GitHub login. A PR belongs to a
//! branch, whoever opened it: one an agent made with `gh pr create` shows up the same way.

use std::path::Path;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::worktree::{self, git};

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct PrInfo {
    pub number: u32,
    pub url: String,
    pub title: String,
    /// "open", "merged" or "closed".
    pub state: String,
    pub draft: bool,
    /// All zero when the repo has no CI.
    pub checks: Checks,
    /// "approved", "changes_requested" or "review_required"; None when no review is asked for.
    pub review: Option<String>,
    /// The head commit, so an automatic fix is asked for once per push.
    #[serde(default)]
    pub head: String,
}

impl PrInfo {
    pub fn is_open(&self) -> bool {
        self.state == "open"
    }

    /// Merged, or closed without merging.
    pub fn is_done(&self) -> bool {
        self.state == "merged" || self.state == "closed"
    }

    /// Checks have finished, and some failed.
    pub fn failing(&self) -> bool {
        self.is_open() && self.checks.pending == 0 && self.checks.failed > 0
    }

    /// Ready to merge on its own: open, not a draft, every check passed (or none), no changes asked for.
    pub fn mergeable(&self) -> bool {
        self.is_open() && !self.draft && self.checks.pending == 0 && self.checks.failed == 0 && self.review.as_deref() != Some("changes_requested")
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct Checks {
    pub passed: u32,
    pub failed: u32,
    pub pending: u32,
    /// Names of the failed checks.
    pub failing: Vec<String>,
}

/// What the Create PR form starts from.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct PrDraft {
    /// The head branch, and the suggested base.
    pub branch: String,
    pub base: String,
    pub title: String,
    pub body: String,
    /// Changed files not committed yet: they're committed with the title as the message first.
    pub uncommitted: u32,
    /// Commits on the branch that aren't on the base.
    pub commits: u32,
    /// Why a PR can't be opened here; the form isn't shown then.
    pub note: Option<String>,
}

const VIEW_FIELDS: &str = "number,url,title,state,isDraft,reviewDecision,statusCheckRollup,headRefOid";

fn gh(dir: &Path, args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new("gh")
        .args(args)
        .current_dir(dir)
        .env("GH_PROMPT_DISABLED", "1")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .output()
        .map_err(|_| anyhow::anyhow!("GitHub CLI isn't installed — brew install gh"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("gh {}: {}", args.first().unwrap_or(&""), err.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The branch checked out in `dir`; None when detached.
pub fn branch(dir: &Path) -> Option<String> {
    git(dir, &["symbolic-ref", "--short", "-q", "HEAD"]).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// The branch PRs go into by default: origin's HEAD, else main or master if there is one.
pub fn default_branch(dir: &Path) -> String {
    if let Ok(r) = git(dir, &["symbolic-ref", "-q", "refs/remotes/origin/HEAD"]) {
        if let Some(b) = r.trim().strip_prefix("refs/remotes/origin/") {
            return b.to_string();
        }
    }
    ["main", "master"]
        .into_iter()
        .find(|b| has_ref(dir, &format!("refs/remotes/origin/{b}")) || has_ref(dir, &format!("refs/heads/{b}")))
        .unwrap_or("main")
        .to_string()
}

fn has_ref(dir: &Path, name: &str) -> bool {
    git(dir, &["rev-parse", "--verify", "--quiet", name]).is_ok()
}

/// The base to suggest: the branch the session's worktree came from if origin has it, else the default.
pub fn suggested_base(default: &str, came_from: Option<&str>, on_origin: impl Fn(&str) -> bool) -> String {
    match came_from {
        Some(b) if b != default && on_origin(b) => b.to_string(),
        _ => default.to_string(),
    }
}

/// One commit: its subject. Otherwise what the agent titled its terminal, or the branch in words.
pub fn prefill_title(subjects: &[String], terminal_title: Option<&str>, branch: &str) -> String {
    if let [one] = subjects {
        return one.clone();
    }
    // Agents decorate titles with spinners and status glyphs: keep from the first word on.
    let titled = terminal_title.map(|t| t.trim_start_matches(|c: char| !c.is_alphanumeric()).trim()).filter(|t| !t.is_empty());
    if let Some(t) = titled {
        return t.to_string();
    }
    let words = branch.rsplit('/').next().unwrap_or(branch).replace(['-', '_'], " ");
    let mut chars = words.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

pub fn prefill_body(subjects: &[String]) -> String {
    subjects.iter().map(|s| format!("- {s}")).collect::<Vec<_>>().join("\n")
}

/// What a PR from the branch checked out in `dir` would hold. `came_from` is the branch of the
/// checkout a session's worktree was made from; `terminal_title` the agent's own title.
pub fn draft(dir: &Path, came_from: Option<&str>, terminal_title: Option<&str>) -> PrDraft {
    let note = |n: String| PrDraft { note: Some(n), ..PrDraft::default() };
    let Ok(root) = worktree::repo_root(dir) else { return note("Not a git repository".into()) };
    if git(&root, &["remote", "get-url", "origin"]).is_err() {
        return note("No GitHub remote (origin)".into());
    }
    let Some(branch) = branch(&root) else { return note("Not on a branch: PRs need one — start the session in a worktree".into()) };
    let default = default_branch(&root);
    if branch == default {
        return note(format!("On {default}: PRs need a branch — start the session in a worktree"));
    }
    if crate::which("gh").is_none() {
        return note("GitHub CLI isn't installed — brew install gh".into());
    }
    if gh(&root, &["auth", "status"]).is_err() {
        return note("gh isn't logged in — run gh auth login".into());
    }
    let base = suggested_base(&default, came_from, |b| has_ref(&root, &format!("refs/remotes/origin/{b}")));
    let subjects = subjects(&root, &base);
    let uncommitted = git(&root, &["status", "--porcelain", "--untracked-files=all"]).map(|s| s.lines().count() as u32).unwrap_or(0);
    let mut d = PrDraft {
        title: prefill_title(&subjects, terminal_title, &branch),
        body: prefill_body(&subjects),
        commits: subjects.len() as u32,
        uncommitted,
        branch,
        base,
        note: None,
    };
    if d.commits == 0 && d.uncommitted == 0 {
        d.note = Some("Nothing to open a PR with yet".into());
    }
    d
}

/// Subjects of the commits on HEAD that aren't on `base`, oldest first.
fn subjects(root: &Path, base: &str) -> Vec<String> {
    let from = [format!("refs/remotes/origin/{base}"), format!("refs/heads/{base}")].into_iter().find(|r| has_ref(root, r));
    let range = from.map_or_else(|| "HEAD".to_string(), |r| format!("{r}..HEAD"));
    git(root, &["log", "--reverse", "--format=%s", &range]).map(|s| s.lines().map(String::from).collect()).unwrap_or_default()
}

/// Commit what's uncommitted (with `title` as the message), push the branch, and open the PR.
/// A PR already open for the branch is returned as it is.
pub fn create(dir: &Path, title: &str, body: &str, base: &str, draft: bool) -> anyhow::Result<PrInfo> {
    let root = worktree::repo_root(dir)?;
    let branch = branch(&root).ok_or_else(|| anyhow::anyhow!("Not on a branch"))?;
    anyhow::ensure!(branch != default_branch(&root), "On {branch}: PRs need a branch — start the session in a worktree");
    if !git(&root, &["status", "--porcelain"])?.trim().is_empty() {
        git(&root, &["add", "-A"])?;
        git(&root, &["commit", "-q", "-m", title])?;
    }
    git(&root, &["push", "-q", "-u", "origin", "HEAD"])?;
    if let Ok(pr) = view(&root, &branch) {
        if pr.state == "open" {
            return Ok(pr);
        }
    }
    let mut args = vec!["pr", "create", "--base", base, "--head", &branch, "--title", title, "--body", body];
    if draft {
        args.push("--draft");
    }
    gh(&root, &args)?;
    view(&root, &branch)
}

/// The latest PR from `branch`, open or not.
pub fn view(dir: &Path, branch: &str) -> anyhow::Result<PrInfo> {
    Ok(parse_view(&gh(dir, &["pr", "view", branch, "--json", VIEW_FIELDS])?)?.0)
}

pub fn merge(dir: &Path, branch: &str) -> anyhow::Result<PrInfo> {
    gh(dir, &["pr", "merge", branch, "--squash"])?;
    view(dir, branch)
}

/// Nothing in `dir` that removing it would lose: no changes, and every commit pushed.
pub fn nothing_to_lose(dir: &Path) -> bool {
    let clean = git(dir, &["status", "--porcelain", "--untracked-files=all"]).is_ok_and(|s| s.trim().is_empty());
    // Without an upstream there's no telling what's pushed.
    let pushed = git(dir, &["rev-list", "--count", "@{upstream}..HEAD"]).is_ok_and(|n| n.trim() == "0");
    clean && pushed
}

/// What to tell an agent whose PR's checks failed: which ones, with the end of each failed
/// GitHub Actions log.
pub fn fix_message(dir: &Path, branch: &str) -> anyhow::Result<String> {
    let (pr, failed) = parse_view(&gh(dir, &["pr", "view", branch, "--json", VIEW_FIELDS])?)?;
    anyhow::ensure!(!failed.is_empty(), "No failed checks on PR #{}", pr.number);
    let mut msg = format!("CI failed on PR #{} ({}): {}.\n", pr.number, pr.url, pr.checks.failing.join(", "));
    let mut seen = vec![];
    for (name, link) in &failed {
        let Some(run) = link.as_deref().and_then(run_id) else { continue };
        if seen.contains(&run) {
            continue;
        }
        seen.push(run.clone());
        if let Ok(log) = gh(dir, &["run", "view", &run, "--log-failed"]) {
            msg += &format!("\n--- {name}: end of the failed log ---\n{}\n", tail(&strip_ansi(&log), 120));
        }
    }
    msg += "\nFix these failures, commit, and push to the branch.";
    Ok(msg)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct View {
    number: u32,
    url: String,
    title: String,
    state: String,
    is_draft: bool,
    #[serde(default)]
    review_decision: Option<String>,
    #[serde(default)]
    status_check_rollup: Vec<Check>,
    #[serde(default)]
    head_ref_oid: String,
}

/// A GitHub Actions run (`CheckRun`: status, conclusion) or a commit status (`StatusContext`: state).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Check {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    details_url: Option<String>,
    #[serde(default)]
    target_url: Option<String>,
}

enum Outcome {
    Passed,
    Failed,
    Pending,
}

impl Check {
    fn outcome(&self) -> Outcome {
        let word = |s: &Option<String>| s.as_deref().unwrap_or("").to_ascii_uppercase();
        if let Some(state) = &self.state {
            return match word(&Some(state.clone())).as_str() {
                "SUCCESS" => Outcome::Passed,
                "FAILURE" | "ERROR" => Outcome::Failed,
                _ => Outcome::Pending,
            };
        }
        if word(&self.status) != "COMPLETED" {
            return Outcome::Pending;
        }
        match word(&self.conclusion).as_str() {
            "SUCCESS" | "NEUTRAL" | "SKIPPED" => Outcome::Passed,
            _ => Outcome::Failed,
        }
    }
}

/// The PR, and its failed checks with their links.
fn parse_view(json: &str) -> anyhow::Result<(PrInfo, Vec<(String, Option<String>)>)> {
    let v: View = serde_json::from_str(json)?;
    let mut checks = Checks::default();
    let mut failed = vec![];
    for c in &v.status_check_rollup {
        match c.outcome() {
            Outcome::Passed => checks.passed += 1,
            Outcome::Pending => checks.pending += 1,
            Outcome::Failed => {
                checks.failed += 1;
                let name = c.name.clone().or_else(|| c.context.clone()).unwrap_or_else(|| "a check".into());
                checks.failing.push(name.clone());
                failed.push((name, c.details_url.clone().or_else(|| c.target_url.clone())));
            }
        }
    }
    let review = v.review_decision.map(|r| r.to_ascii_lowercase()).filter(|r| !r.is_empty());
    let pr = PrInfo { number: v.number, url: v.url, title: v.title, state: v.state.to_ascii_lowercase(), draft: v.is_draft, checks, review, head: v.head_ref_oid };
    Ok((pr, failed))
}

/// `…/actions/runs/<id>/job/<job>` → `<id>`.
fn run_id(link: &str) -> Option<String> {
    let rest = &link[link.find("/actions/runs/")? + "/actions/runs/".len()..];
    let id: String = rest.chars().take_while(char::is_ascii_digit).collect();
    (!id.is_empty()).then_some(id)
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            // Parameters, then one final letter.
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn tail(s: &str, lines: usize) -> String {
    let all: Vec<&str> = s.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_checks_and_review() {
        let json = r#"{
            "number": 12, "url": "https://github.com/o/r/pull/12", "title": "Add a thing",
            "state": "OPEN", "isDraft": false, "reviewDecision": "CHANGES_REQUESTED",
            "statusCheckRollup": [
                {"__typename": "CheckRun", "name": "build", "status": "COMPLETED", "conclusion": "FAILURE",
                 "detailsUrl": "https://github.com/o/r/actions/runs/987/job/1"},
                {"__typename": "CheckRun", "name": "lint", "status": "COMPLETED", "conclusion": "SUCCESS"},
                {"__typename": "CheckRun", "name": "docs", "status": "COMPLETED", "conclusion": "SKIPPED"},
                {"__typename": "CheckRun", "name": "e2e", "status": "IN_PROGRESS", "conclusion": ""},
                {"__typename": "StatusContext", "context": "ci/legacy", "state": "ERROR", "targetUrl": "https://ci.example/1"},
                {"__typename": "StatusContext", "context": "deploy", "state": "PENDING"},
                {"__typename": "StatusContext", "context": "cla", "state": "SUCCESS"}
            ]
        }"#;
        let (pr, failed) = parse_view(json).unwrap();
        assert_eq!((pr.number, pr.state.as_str(), pr.draft, pr.review.as_deref()), (12, "open", false, Some("changes_requested")));
        assert_eq!(pr.checks, Checks { passed: 3, failed: 2, pending: 2, failing: vec!["build".into(), "ci/legacy".into()] });
        assert_eq!(failed[0].1.as_deref().and_then(run_id).as_deref(), Some("987"));
        assert_eq!(failed[1].1.as_deref().and_then(run_id), None);
    }

    #[test]
    fn parses_a_merged_pr_without_ci() {
        let json = r#"{"number": 3, "url": "u", "title": "t", "state": "MERGED", "isDraft": false, "reviewDecision": "", "statusCheckRollup": []}"#;
        let (pr, _) = parse_view(json).unwrap();
        assert_eq!((pr.state.as_str(), pr.review, pr.checks), ("merged", None, Checks::default()));
    }

    #[test]
    fn ready_to_merge_or_fix() {
        let pr = |state: &str, draft, passed, failed, pending, review: Option<&str>| PrInfo {
            state: state.into(),
            draft,
            checks: Checks { passed, failed, pending, failing: vec![] },
            review: review.map(String::from),
            ..PrInfo::default()
        };
        assert!(pr("open", false, 2, 0, 0, None).mergeable());
        assert!(pr("open", false, 0, 0, 0, Some("approved")).mergeable(), "no CI");
        assert!(!pr("open", false, 2, 0, 1, None).mergeable(), "still running");
        assert!(!pr("open", true, 2, 0, 0, None).mergeable(), "draft");
        assert!(!pr("open", false, 2, 0, 0, Some("changes_requested")).mergeable());
        assert!(!pr("merged", false, 2, 0, 0, None).mergeable());
        assert!(pr("open", false, 1, 1, 0, None).failing());
        assert!(!pr("open", false, 1, 1, 1, None).failing(), "not settled yet");
        assert!(!pr("closed", false, 1, 1, 0, None).failing());
    }

    #[test]
    fn prefills() {
        let one = vec!["Fix the parser".to_string()];
        let two = vec!["Add tests".to_string(), "Fix the parser".to_string()];
        assert_eq!(prefill_title(&one, Some("✳ Something else"), "dino/claude-ab12"), "Fix the parser");
        assert_eq!(prefill_title(&two, Some("✳ Parser work"), "dino/claude-ab12"), "Parser work");
        assert_eq!(prefill_title(&two, Some("⠂ "), "dino/fix-the_parser"), "Fix the parser");
        assert_eq!(prefill_title(&[], None, "feature"), "Feature");
        assert_eq!(prefill_body(&two), "- Add tests\n- Fix the parser");

        let on_origin = |b: &str| b == "release";
        assert_eq!(suggested_base("main", Some("release"), on_origin), "release");
        assert_eq!(suggested_base("main", Some("local-only"), on_origin), "main");
        assert_eq!(suggested_base("main", None, on_origin), "main");
    }

    #[test]
    fn strips_ansi_and_tails() {
        assert_eq!(strip_ansi("\u{1b}[31merror\u{1b}[0m: bad"), "error: bad");
        assert_eq!(tail("a\nb\nc", 2), "b\nc");
    }

    #[test]
    fn default_branch_and_draft() {
        let tmp = std::env::temp_dir().join(format!("dino-pr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let (origin, repo) = (tmp.join("origin.git"), tmp.join("repo"));
        std::fs::create_dir_all(&repo).unwrap();
        git(&tmp, &["init", "-q", "--bare", "-b", "trunk", &origin.to_string_lossy()]).unwrap();
        git(&repo, &["init", "-q", "-b", "trunk"]).unwrap();
        assert_eq!(default_branch(&repo), "main", "nothing to go on");
        let commit = |msg: &str| git(&repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", msg]).unwrap();
        commit("init");
        git(&repo, &["remote", "add", "origin", &origin.to_string_lossy()]).unwrap();
        git(&repo, &["push", "-q", "-u", "origin", "trunk"]).unwrap();
        git(&repo, &["remote", "set-head", "origin", "trunk"]).unwrap();
        assert_eq!(default_branch(&repo), "trunk");

        git(&repo, &["checkout", "-q", "-b", "dino/claude-ab12"]).unwrap();
        commit("Teach the parser commas");
        std::fs::write(repo.join("new.txt"), "hi\n").unwrap();
        assert_eq!(subjects(&repo, "trunk"), vec!["Teach the parser commas".to_string()]);
        assert!(!nothing_to_lose(&repo), "an untracked file, and no upstream");
        std::fs::remove_file(repo.join("new.txt")).unwrap();
        assert!(!nothing_to_lose(&repo), "a commit that isn't pushed");
        git(&repo, &["push", "-q", "-u", "origin", "HEAD"]).unwrap();
        assert!(nothing_to_lose(&repo));
        std::fs::write(repo.join("new.txt"), "hi\n").unwrap();
        // The branch notes come before any gh call, so these never reach GitHub.
        git(&repo, &["checkout", "-q", "trunk"]).unwrap();
        assert_eq!(draft(&repo, None, None).note.as_deref(), Some("On trunk: PRs need a branch — start the session in a worktree"));
        git(&repo, &["checkout", "-q", "--detach"]).unwrap();
        assert!(draft(&repo, None, None).note.unwrap().starts_with("Not on a branch"));
        assert_eq!(draft(&tmp, None, None).note.as_deref(), Some("Not a git repository"));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
