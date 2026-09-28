//! Git worktrees for fan-out: one checkout per agent, compared, then one kept and the rest removed.
//!
//! Worktrees live in `<repo>/.dino/worktrees/<name>` (like Claude Desktop's `.claude/worktrees`),
//! so agents that trust the repo trust its worktrees. `.dino/` is hidden via `.git/info/exclude`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiffStat {
    pub files: u32,
    pub added: u32,
    pub removed: u32,
}

fn git(dir: &Path, args: &[&str]) -> anyhow::Result<String> {
    git_in(dir, args, None)
}

fn git_in(dir: &Path, args: &[&str], stdin: Option<&[u8]>) -> anyhow::Result<String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(input) = stdin {
        child.stdin.take().unwrap().write_all(input)?;
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("git {}: {}", args.first().unwrap_or(&""), err.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The top of the checkout containing `dir`.
pub fn repo_root(dir: &Path) -> anyhow::Result<PathBuf> {
    git(dir, &["rev-parse", "--show-toplevel"])
        .map(|s| PathBuf::from(s.trim()))
        .map_err(|_| anyhow::anyhow!("{} isn't in a git repository", dir.display()))
}

/// A checkout of a repo: its folder and branch (None when detached).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: String,
    pub branch: Option<String>,
}

/// Every worktree of the repo containing `dir`, the main checkout first.
pub fn list(dir: &Path) -> anyhow::Result<Vec<Worktree>> {
    let out = git(dir, &["worktree", "list", "--porcelain"])?;
    Ok(out
        .split("\n\n")
        .filter(|b| !b.lines().any(|l| l == "bare" || l.starts_with("prunable")))
        .filter_map(|b| {
            let path = b.lines().next()?.strip_prefix("worktree ")?.to_string();
            let branch = b.lines().find_map(|l| l.strip_prefix("branch refs/heads/")).map(String::from);
            Some(Worktree { path, branch })
        })
        .collect())
}

/// A commit of the checkout as it is now, uncommitted edits included, without touching it:
/// agents start from what the user sees, not from the last commit.
pub fn snapshot(repo: &Path) -> anyhow::Result<String> {
    let stash = git(repo, &["stash", "create", "dino fan-out base"])?;
    let commit = if stash.trim().is_empty() { git(repo, &["rev-parse", "HEAD"])? } else { stash };
    Ok(commit.trim().to_string())
}

pub fn worktrees_dir(repo: &Path) -> PathBuf {
    repo.join(".dino/worktrees")
}

/// A new worktree at `worktrees_dir(repo)/<name>` on a new `branch` from `base`.
pub fn add(repo: &Path, name: &str, branch: &str, base: &str) -> anyhow::Result<PathBuf> {
    exclude_dino_dir(repo)?;
    let dir = worktrees_dir(repo).join(name);
    std::fs::create_dir_all(dir.parent().unwrap())?;
    git(repo, &["worktree", "add", "--quiet", "-b", branch, &dir.to_string_lossy(), base])?;
    Ok(dir)
}

fn exclude_dino_dir(repo: &Path) -> anyhow::Result<()> {
    let common = git(repo, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
    let exclude = PathBuf::from(common.trim()).join("info/exclude");
    let current = std::fs::read_to_string(&exclude).unwrap_or_default();
    if !current.lines().any(|l| l.trim() == "/.dino/") {
        std::fs::create_dir_all(exclude.parent().unwrap())?;
        let sep = if current.is_empty() || current.ends_with('\n') { "" } else { "\n" };
        std::fs::write(&exclude, format!("{current}{sep}/.dino/\n"))?;
    }
    Ok(())
}

/// Size of everything the agent changed in `dir` since `base`: edits, new files, its own commits.
pub fn stat(dir: &Path, base: &str) -> anyhow::Result<DiffStat> {
    track_new_files(dir)?;
    let mut stat = DiffStat::default();
    for line in git(dir, &["diff", "--numstat", base])?.lines() {
        let mut parts = line.split('\t');
        stat.files += 1;
        stat.added += parts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        stat.removed += parts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
    }
    Ok(stat)
}

/// The patch for everything the agent changed in `dir` since `base`.
pub fn diff(dir: &Path, base: &str) -> anyhow::Result<String> {
    track_new_files(dir)?;
    git(dir, &["diff", "--binary", base])
}

/// Mark new files intent-to-add so diffs include them; nothing else in the worktree changes.
fn track_new_files(dir: &Path) -> anyhow::Result<()> {
    git(dir, &["add", "--all", "--intent-to-add"]).map(drop)
}

/// Bring the agent's changes from worktree `dir` into the user's checkout `repo`, uncommitted,
/// for them to review and commit.
pub fn apply(dir: &Path, base: &str, repo: &Path) -> anyhow::Result<DiffStat> {
    let (stat, patch) = (stat(dir, base)?, diff(dir, base)?);
    if patch.is_empty() {
        return Ok(stat);
    }
    // Plain first, so the user's index stays as it was; three-way when their checkout moved on.
    if git_in(repo, &["apply", "--whitespace=nowarn", "-"], Some(patch.as_bytes())).is_err() {
        git_in(repo, &["apply", "--3way", "--whitespace=nowarn", "-"], Some(patch.as_bytes()))?;
    }
    Ok(stat)
}

/// Remove a worktree and its branch, discarding whatever is in it.
pub fn remove(repo: &Path, dir: &Path, branch: &str) {
    let _ = git(repo, &["worktree", "remove", "--force", &dir.to_string_lossy()]);
    let _ = git(repo, &["branch", "-D", branch]);
    // The group's folder, once its last worktree is gone.
    if let Some(parent) = dir.parent() {
        let _ = std::fs::remove_dir(parent);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fan_out_round_trip() {
        let tmp = std::env::temp_dir().join(format!("dino-wt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let repo = tmp.as_path();
        git(repo, &["init", "-q", "-b", "main"]).unwrap();
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        git(repo, &["add", "."]).unwrap();
        git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"]).unwrap();
        // An uncommitted edit the agents should start from.
        std::fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();

        let base = snapshot(repo).unwrap();
        let wt = add(repo, "g/claude", "dino/g/claude", &base).unwrap();
        assert_eq!(std::fs::read_to_string(wt.join("a.txt")).unwrap(), "one\ntwo\n");
        assert!(git(repo, &["status", "--porcelain"]).unwrap().lines().all(|l| !l.contains(".dino")));
        let all = list(&wt).unwrap();
        let real = |p: &Path| p.canonicalize().unwrap().to_string_lossy().into_owned();
        assert_eq!(all, vec![
            Worktree { path: real(repo), branch: Some("main".into()) },
            Worktree { path: real(&wt), branch: Some("dino/g/claude".into()) },
        ]);

        std::fs::write(wt.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(wt.join("new.txt"), "hi\n").unwrap();
        assert_eq!(stat(&wt, &base).unwrap(), DiffStat { files: 2, added: 2, removed: 0 });
        let text = diff(&wt, &base).unwrap();
        assert!(text.contains("+three") && text.contains("new.txt"));

        apply(&wt, &base, repo).unwrap();
        assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap(), "one\ntwo\nthree\n");
        assert_eq!(std::fs::read_to_string(repo.join("new.txt")).unwrap(), "hi\n");

        remove(repo, &wt, "dino/g/claude");
        assert!(!wt.exists());
        assert!(git(repo, &["branch", "--list", "dino/*"]).unwrap().trim().is_empty());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
