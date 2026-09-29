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

pub(crate) fn git(dir: &Path, args: &[&str]) -> anyhow::Result<String> {
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
    /// dino made it for a session: closing it can apply its changes and remove it.
    #[serde(default)]
    pub dino: bool,
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
            Some(Worktree { path, branch, dino: false })
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

/// A new worktree at `worktrees_dir(repo)/<name>` on a new `branch` from `base`, with the
/// `.worktreeinclude` files of `repo` copied in.
pub fn add(repo: &Path, name: &str, branch: &str, base: &str) -> anyhow::Result<PathBuf> {
    let dir = add_bare(repo, name, branch, base)?;
    copy_included(repo, &dir);
    Ok(dir)
}

/// Past these, the rest of the `.worktreeinclude` files are left out (say a pattern caught
/// `node_modules`): a worktree should take seconds to make.
const INCLUDE_MAX_FILES: usize = 10_000;
const INCLUDE_MAX_BYTES: u64 = 512 << 20;

/// Copy the files `.worktreeinclude` (gitignore syntax, at the top of `from`) names into worktree
/// `to`, as Claude Code does: only files that are also gitignored, since tracked ones are already
/// there. For `.env` and the like. A file that can't be copied is logged and skipped.
/// Returns the paths copied, relative to the top.
pub fn copy_included(from: &Path, to: &Path) -> Vec<String> {
    let include = from.join(".worktreeinclude");
    if !include.is_file() {
        return vec![];
    }
    use std::collections::HashSet;
    // Untracked files matching `exclude`; with `dirs`, a folder matched whole is one `dir/` entry.
    let untracked = |exclude: &str, dirs: bool| -> HashSet<String> {
        let mut args = vec!["ls-files", "-z", "--others", "--ignored", exclude];
        if dirs {
            args.push("--directory");
        }
        match git(from, &args) {
            Ok(out) => out.split('\0').filter(|p| !p.is_empty()).map(String::from).collect(),
            Err(e) => {
                eprintln!("dinod: .worktreeinclude in {}: {e}", from.display());
                HashSet::new()
            }
        }
    };
    let ignored = untracked("--exclude-standard", false);
    // Folders ignored whole: git lists a folder whose files are all ignored as one entry too.
    let collapsed: Vec<String> = untracked("--exclude-standard", true).into_iter().filter(|p| p.ends_with('/')).collect();
    let ignored_dirs: Vec<String> = if collapsed.is_empty() {
        vec![]
    } else {
        let input = collapsed.iter().map(|d| d.trim_end_matches('/')).collect::<Vec<_>>().join("\0");
        git_in(from, &["check-ignore", "--no-index", "--stdin", "-z"], Some(input.as_bytes()))
            .unwrap_or_default()
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(|p| format!("{p}/"))
            .collect()
    };
    let patterns: Vec<String> = std::fs::read_to_string(&include)
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim_end().to_string())
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('!'))
        .collect();
    // In a folder ignored whole (node_modules, target), a pattern for any depth (`**/x`, or `x`
    // with no slash) takes a file only when it matches the folder itself or its first name is
    // one of the folder's; `vendor/**/config.json` and the like go anywhere. As Claude Code does.
    let mut per_pattern: std::collections::HashMap<&str, HashSet<String>> = Default::default();
    let mut reaches = |dir: &str, rel: &str| -> bool {
        patterns.iter().any(|p| {
            if !per_pattern.entry(p).or_insert_with(|| untracked(&format!("--exclude={p}"), false)).contains(rel) {
                return false;
            }
            let body = p.trim_end_matches('/');
            let any_depth = body.strip_prefix("**/").or_else(|| (!body.contains('/')).then_some(body));
            let Some(rest) = any_depth else { return true };
            let first = rest.split('/').next().unwrap_or("");
            dir.trim_end_matches('/').split('/').any(|name| glob(first.as_bytes(), name.as_bytes()))
        })
    };
    let (Ok(src_top), Ok(dst_top)) = (from.canonicalize(), to.canonicalize()) else { return vec![] };
    let (mut copied, mut bytes) = (vec![], 0u64);
    let mut wanted: Vec<String> = untracked(&format!("--exclude-from={}", include.display()), false).into_iter().collect();
    wanted.sort();
    for rel in wanted {
        if !ignored.contains(&rel) || rel.starts_with(".dino/") {
            continue;
        }
        if ignored_dirs.iter().find(|d| rel.starts_with(d.as_str())).is_some_and(|dir| !reaches(dir, &rel)) {
            continue;
        }
        if copied.len() >= INCLUDE_MAX_FILES || bytes > INCLUDE_MAX_BYTES {
            eprintln!("dinod: .worktreeinclude in {}: stopped after {} files ({} MB)", from.display(), copied.len(), bytes >> 20);
            break;
        }
        match copy_one(&src_top, &dst_top, &rel) {
            Ok(n) => {
                bytes += n;
                copied.push(rel);
            }
            Err(e) => eprintln!("dinod: .worktreeinclude: couldn't copy {rel}: {e}"),
        }
    }
    copied
}

/// Whether one path name matches one gitignore glob name: `*`, `?`, `[a-z]`, `[!x]`, `\` escapes.
fn glob(p: &[u8], s: &[u8]) -> bool {
    match (p.first(), s.first()) {
        (None, _) => s.is_empty(),
        (Some(b'*'), _) => glob(&p[1..], s) || (!s.is_empty() && glob(p, &s[1..])),
        (_, None) => false,
        (Some(b'?'), _) => glob(&p[1..], &s[1..]),
        (Some(b'['), Some(&c)) => {
            let Some(end) = p.iter().skip(2).position(|&b| b == b']').map(|i| i + 2) else { return p[0] == c && glob(&p[1..], &s[1..]) };
            let (negate, set) = match p[1] {
                b'!' | b'^' => (true, &p[2..end]),
                _ => (false, &p[1..end]),
            };
            let mut hit = false;
            let mut i = 0;
            while i < set.len() {
                if i + 2 < set.len() && set[i + 1] == b'-' {
                    hit |= (set[i]..=set[i + 2]).contains(&c);
                    i += 3;
                } else {
                    hit |= set[i] == c;
                    i += 1;
                }
            }
            hit != negate && glob(&p[end + 1..], &s[1..])
        }
        (Some(b'\\'), Some(&c)) if p.len() > 1 => p[1] == c && glob(&p[2..], &s[1..]),
        (Some(&a), Some(&c)) => a == c && glob(&p[1..], &s[1..]),
    }
}

fn copy_one(src_top: &Path, dst_top: &Path, rel: &str) -> anyhow::Result<u64> {
    anyhow::ensure!(!rel.split('/').any(|c| c == ".." || c.is_empty()), "odd path");
    let (src, dst) = (src_top.join(rel), dst_top.join(rel));
    // A symlink, or a file in a symlinked folder, is copied only when it leads somewhere inside
    // the checkout, and then as the file it leads to.
    anyhow::ensure!(src.canonicalize()?.starts_with(src_top), "it leads outside the checkout");
    anyhow::ensure!(std::fs::metadata(&src)?.is_file(), "not a file");
    anyhow::ensure!(std::fs::symlink_metadata(&dst).is_err(), "it's already in the worktree");
    let parent = dst.parent().unwrap();
    std::fs::create_dir_all(parent)?;
    anyhow::ensure!(parent.canonicalize()?.starts_with(dst_top), "its folder leads outside the worktree");
    // Keeps the permissions: scripts stay executable, secrets stay private.
    Ok(std::fs::copy(&src, &dst)?)
}

fn add_bare(repo: &Path, name: &str, branch: &str, base: &str) -> anyhow::Result<PathBuf> {
    exclude_dino_dir(repo)?;
    let dir = worktrees_dir(repo).join(name);
    std::fs::create_dir_all(dir.parent().unwrap())?;
    git(repo, &["worktree", "add", "--quiet", "-b", branch, &dir.to_string_lossy(), base])?;
    Ok(dir)
}

/// A worktree for one session, off the HEAD of `checkout` (a repo's main checkout or one of its
/// worktrees), with the checkout's uncommitted edits carried over uncommitted: the new branch holds
/// only what the agent commits. Returns the worktree and the commit its changes count from.
pub fn start(checkout: &Path, name: &str, branch: &str) -> anyhow::Result<(PathBuf, String)> {
    let base = snapshot(checkout)?;
    let head = git(checkout, &["rev-parse", "HEAD"])?.trim().to_string();
    // Worktrees all live in the main checkout, even when this one is a worktree itself.
    let main = list(checkout)?.into_iter().next().map_or_else(|| checkout.to_path_buf(), |w| PathBuf::from(w.path));
    let dir = add_bare(&main, name, branch, &head)?;
    if base != head {
        let patch = git(checkout, &["diff", "--binary", &head, &base])?;
        if let Err(e) = git_in(&dir, &["apply", "--whitespace=nowarn", "-"], Some(patch.as_bytes())) {
            remove(&main, &dir, branch);
            return Err(e);
        }
    }
    copy_included(checkout, &dir);
    Ok((dir, base))
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
    Ok(parse_diff(&changes_patch(dir, base)?))
}

/// `changes` as one unified diff, as git prints it.
pub fn changes_patch(dir: &Path, base: &str) -> anyhow::Result<String> {
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
    text
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
    fn worktreeinclude_copies_ignored_files_it_names() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = std::env::temp_dir().join(format!("dino-wtinc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let repo = tmp.join("repo");
        std::fs::create_dir_all(repo.join("config")).unwrap();
        std::fs::create_dir_all(repo.join("app/sub")).unwrap();
        std::fs::create_dir_all(repo.join("node_modules/pkg")).unwrap();
        let repo = repo.as_path();
        git(repo, &["init", "-q", "-b", "main"]).unwrap();
        let w = |p: &str, s: &str| std::fs::write(repo.join(p), s).unwrap();
        std::fs::create_dir_all(repo.join("vendor/a")).unwrap();
        std::fs::create_dir_all(repo.join("cache")).unwrap();
        std::fs::create_dir_all(repo.join("py/.venv/bin")).unwrap();
        w(".gitignore", ".env\n.env.*\n*.log\nconfig/secrets.json\nnode_modules/\nlocal.json\nlink.env\nout.env\nvendor/\ncache/\n.venv/\n");
        w(".worktreeinclude", "# the secrets\n.env\n.env.local\nconfig/secrets.json\n**/local.json\ntracked.txt\nuntracked.txt\nlink.env\nout.env\nvendor/**/config.json\n**/.ven[v]\n");
        w("tracked.txt", "tracked\n");
        git(repo, &["add", "."]).unwrap();
        git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"]).unwrap();
        w(".env", "KEY=1\n");
        std::fs::set_permissions(repo.join(".env"), std::fs::Permissions::from_mode(0o600)).unwrap();
        w(".env.local", "LOCAL=1\n");
        w(".env.test", "not listed\n");
        w("debug.log", "not listed\n");
        w("config/secrets.json", "{}\n");
        w("app/sub/local.json", "{\"nested\":1}\n");
        w("node_modules/pkg/local.json", "in an ignored folder the pattern doesn't name\n");
        w("node_modules/pkg/index.js", "\n");
        w("cache/local.json", "all this ignored folder holds\n");
        w("py/.venv/bin/activate", "venv\n");
        w("vendor/a/config.json", "vendored\n");
        w("vendor/a/other.json", "\n");
        w("untracked.txt", "listed, but not ignored: it's the user's new file, not a secret\n");
        w("tracked.txt", "edited\n");
        std::fs::write(tmp.join("outside"), "SECRET\n").unwrap();
        std::os::unix::fs::symlink(repo.join(".env.local"), repo.join("link.env")).unwrap();
        std::os::unix::fs::symlink(tmp.join("outside"), repo.join("out.env")).unwrap();

        let (wt, _) = start(repo, "claude-ab12", "dino/claude-ab12").unwrap();
        let read = |p: &str| std::fs::read_to_string(wt.join(p)).ok();
        assert_eq!(read(".env").as_deref(), Some("KEY=1\n"));
        assert_eq!(std::fs::metadata(wt.join(".env")).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(read(".env.local").as_deref(), Some("LOCAL=1\n"));
        assert_eq!(read("config/secrets.json").as_deref(), Some("{}\n"));
        assert_eq!(read("app/sub/local.json").as_deref(), Some("{\"nested\":1}\n"));
        assert_eq!(read("link.env").as_deref(), Some("LOCAL=1\n"), "a link inside the checkout comes as its file");
        assert!(!std::fs::symlink_metadata(wt.join("link.env")).unwrap().file_type().is_symlink());
        assert_eq!(read("out.env"), None, "a link out of the checkout stays behind");
        assert_eq!(read("node_modules/pkg/local.json"), None, "a **/ pattern doesn't reach into an ignored folder");
        assert_eq!(read("cache/local.json"), None);
        assert_eq!(read("vendor/a/config.json").as_deref(), Some("vendored\n"), "a pattern naming the folder reaches in");
        assert_eq!(read("vendor/a/other.json"), None);
        assert_eq!(read("py/.venv/bin/activate").as_deref(), Some("venv\n"), "a pattern matching the folder takes it whole");
        for (p, s, ok) in [("*.js", "a.js", true), ("*.js", "a.jsx", false), ("[!a]b", "cb", true), ("[!a]b", "ab", false), ("[a-c]?", "bz", true), ("\\*", "*", true), ("\\*", "x", false)] {
            assert_eq!(glob(p.as_bytes(), s.as_bytes()), ok, "{p} {s}");
        }
        assert_eq!(read(".env.test"), None);
        assert_eq!(read("debug.log"), None);
        assert_eq!(read("untracked.txt"), None);
        assert_eq!(read("tracked.txt").as_deref(), Some("edited\n"), "tracked files come from git, edits and all");
        assert_eq!(git(&wt, &["status", "--porcelain"]).unwrap(), " M tracked.txt\n", "copied files stay ignored");

        // Fan-out worktrees get them too; no .worktreeinclude, nothing copied.
        let base = snapshot(repo).unwrap();
        let fan = add(repo, "g/codex", "dino/g/codex", &base).unwrap();
        assert!(fan.join(".env").exists());
        std::fs::remove_file(repo.join(".worktreeinclude")).unwrap();
        assert!(copy_included(repo, &fan).is_empty());
        remove(repo, &fan, "dino/g/codex");
        remove(repo, &wt, "dino/claude-ab12");
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
            Worktree { path: real(repo), branch: Some("main".into()), dino: false },
            Worktree { path: real(&wt), branch: Some("dino/g/claude".into()), dino: false },
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

        // A session's worktree: the branch starts at HEAD, the uncommitted edits come along uncommitted.
        let (one, base) = start(repo, "claude-ab12", "dino/claude-ab12").unwrap();
        assert_eq!(git(&one, &["rev-parse", "HEAD"]).unwrap(), git(repo, &["rev-parse", "HEAD"]).unwrap());
        assert_eq!(std::fs::read_to_string(one.join("a.txt")).unwrap(), "one\ntwo\nthree\n");
        assert_eq!(stat(&one, &base).unwrap(), DiffStat::default(), "carried-over edits aren't the agent's");
        // Started from inside it, the next one still lands in the main checkout's worktrees folder.
        let (two, _) = start(&one, "codex-cd34", "dino/codex-cd34").unwrap();
        assert_eq!(real(two.parent().unwrap()), real(&worktrees_dir(repo)));
        remove(repo, &two, "dino/codex-cd34");
        remove(repo, &one, "dino/claude-ab12");
        assert!(git(repo, &["branch", "--list", "dino/*"]).unwrap().trim().is_empty());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
