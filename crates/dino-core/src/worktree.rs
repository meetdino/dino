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

/// A file's changes, parsed for review.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    /// Relative to the repo root; the new name when renamed.
    pub path: String,
    pub old_path: Option<String>,
    /// "added", "deleted", "renamed" or "modified".
    pub status: String,
    pub added: u32,
    pub removed: u32,
    pub binary: bool,
    pub lines: Vec<DiffLine>,
    /// Lines past `MAX_LINES` were left out.
    pub truncated: bool,
}

/// One line of a hunk. `kind` is "hunk" (the `@@` header), "add", "del" or "ctx".
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: String,
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub text: String,
}

/// Per file, so a lockfile rewrite doesn't drown the rest.
const MAX_LINES: usize = 4000;

/// git's empty tree: the base in a repo with no commits yet.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// The last commit of the checkout containing `dir`.
pub fn head(dir: &Path) -> String {
    git(dir, &["rev-parse", "--verify", "--quiet", "HEAD"]).map(|s| s.trim().to_string()).unwrap_or_else(|_| EMPTY_TREE.into())
}

/// Everything changed in the checkout containing `dir` since `base`, new files included, per file.
/// Reads through a copy of the index, so the user's staging area stays exactly as it was.
pub fn changes(dir: &Path, base: &str) -> anyhow::Result<Vec<FileDiff>> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let index = PathBuf::from(git(dir, &["rev-parse", "--path-format=absolute", "--git-path", "index"])?.trim());
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = std::env::temp_dir().join(format!("dino-index-{}-{n}", std::process::id()));
    if index.exists() {
        std::fs::copy(&index, &tmp)?;
    }
    let run = |args: &[&str]| -> anyhow::Result<String> {
        let out = Command::new("git").arg("-C").arg(dir).args(args).env("GIT_INDEX_FILE", &tmp).stdin(Stdio::null()).output()?;
        anyhow::ensure!(out.status.success(), "git {}: {}", args[0], String::from_utf8_lossy(&out.stderr).trim());
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let text = run(&["add", "--all", "--intent-to-add"])
        .and_then(|_| run(&["-c", "core.quotePath=false", "diff", "--no-color", "--no-ext-diff", "--find-renames", base]));
    let _ = std::fs::remove_file(&tmp);
    Ok(parse_diff(&text?))
}

fn parse_diff(text: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let (mut old, mut new, mut in_hunk) = (0u32, 0u32, false);
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            // `a/x b/x`; the `+++` line names it exactly, unless the file is gone or binary.
            let path = rest.rsplit_once(" b/").map_or(rest, |(_, b)| b).to_string();
            files.push(FileDiff { path, old_path: None, status: "modified".into(), added: 0, removed: 0, binary: false, lines: vec![], truncated: false });
            in_hunk = false;
            continue;
        }
        let Some(f) = files.last_mut() else { continue };
        if let Some(h) = line.strip_prefix("@@ ") {
            // `@@ -old[,n] +new[,n] @@ context`
            let mut nums = h.split(' ').take(2).map(|r| r.get(1..).and_then(|r| r.split(',').next()?.parse().ok()).unwrap_or(0));
            (old, new, in_hunk) = (nums.next().unwrap_or(0), nums.next().unwrap_or(0), true);
            push(f, DiffLine { kind: "hunk".into(), old: None, new: None, text: line.into() });
            continue;
        }
        if !in_hunk {
            if line.starts_with("new file mode") {
                f.status = "added".into();
            } else if line.starts_with("deleted file mode") {
                f.status = "deleted".into();
            } else if let Some(p) = line.strip_prefix("rename from ") {
                f.status = "renamed".into();
                f.old_path = Some(p.into());
            } else if let Some(p) = line.strip_prefix("rename to ").or_else(|| line.strip_prefix("+++ b/")) {
                // git ends a name with spaces in it with a tab here.
                f.path = p.trim_end_matches('\t').into();
            } else if line.starts_with("Binary files ") {
                f.binary = true;
            }
            continue;
        }
        let (kind, o, n) = match line.chars().next() {
            Some('+') => ("add", None, Some(new)),
            Some('-') => ("del", Some(old), None),
            Some(' ') | None => ("ctx", Some(old), Some(new)),
            // "\ No newline at end of file"
            _ => continue,
        };
        match kind {
            "add" => (f.added, new) = (f.added + 1, new + 1),
            "del" => (f.removed, old) = (f.removed + 1, old + 1),
            _ => (old, new) = (old + 1, new + 1),
        }
        push(f, DiffLine { kind: kind.into(), old: o, new: n, text: line.get(1..).unwrap_or("").into() });
    }
    files
}

fn push(f: &mut FileDiff, line: DiffLine) {
    if f.lines.len() < MAX_LINES {
        f.lines.push(line);
    } else {
        f.truncated = true;
    }
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
    fn changes_leave_the_index_alone() {
        let tmp = std::env::temp_dir().join(format!("dino-changes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let repo = tmp.as_path();
        git(repo, &["init", "-q", "-b", "main"]).unwrap();
        assert_eq!(head(repo), EMPTY_TREE);
        std::fs::write(repo.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(repo.join("gone.txt"), "bye\n").unwrap();
        git(repo, &["add", "."]).unwrap();
        git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"]).unwrap();
        std::fs::write(repo.join("a.txt"), "one\n2\nthree\nfour\n").unwrap();
        std::fs::remove_file(repo.join("gone.txt")).unwrap();
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        std::fs::write(repo.join("sub/new file.txt"), "hi\n").unwrap();
        let status = git(repo, &["status", "--porcelain"]).unwrap();

        let files = changes(&repo.join("sub"), &head(repo)).unwrap();
        let by = |p: &str| files.iter().find(|f| f.path == p).unwrap_or_else(|| panic!("{p} in {files:?}"));
        let a = by("a.txt");
        assert_eq!((a.status.as_str(), a.added, a.removed), ("modified", 2, 1));
        let lines: Vec<_> = a.lines.iter().map(|l| (l.kind.as_str(), l.old, l.new, l.text.as_str())).collect();
        assert_eq!(lines, vec![
            ("hunk", None, None, "@@ -1,3 +1,4 @@"),
            ("ctx", Some(1), Some(1), "one"),
            ("del", Some(2), None, "two"),
            ("add", None, Some(2), "2"),
            ("ctx", Some(3), Some(3), "three"),
            ("add", None, Some(4), "four"),
        ]);
        assert_eq!(by("gone.txt").status, "deleted");
        let new = by("sub/new file.txt");
        assert_eq!((new.status.as_str(), new.added), ("added", 1));
        assert_eq!(git(repo, &["status", "--porcelain"]).unwrap(), status, "the user's index is untouched");
        let _ = std::fs::remove_dir_all(&tmp);
    }

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
