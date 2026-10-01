//! Git worktrees for fan-out: one checkout per agent, compared, then one kept and the rest removed.
//!
//! Worktrees live in `~/.dino/worktrees/<repo>/<name>` by default, outside the repo so they don't nest
//! copies of it in its own file tree; dino carries the repo's trust over (see `trust`). Settings →
//! Worktrees can move them (`worktrees_dir`), inside the repo too, hidden via `.git/info/exclude`.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

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
    /// What's in it compared with the main checkout's branch; None for the main checkout.
    #[serde(default)]
    pub git: Option<Summary>,
    /// The agent that made it, when its agent says so (Claude's subagent hooks).
    #[serde(default)]
    pub owner: Option<Owner>,
}

/// Who made a worktree: a subagent of a session.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Owner {
    pub session: String,
    /// The task it was given ("Review code button"), when known.
    pub description: Option<String>,
    pub agent_type: Option<String>,
    pub running: bool,
}

/// A worktree at a glance, for the sidebar.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    /// The latest commit subject not on the base branch, else a readable branch name.
    pub label: String,
    /// Everything changed since it branched off, uncommitted work and new files included.
    pub added: u32,
    pub removed: u32,
    /// Uncommitted or new files.
    pub dirty: bool,
    /// Commits the base branch doesn't have.
    pub ahead: u32,
    /// "in_progress" (uncommitted work), "ready" (committed, not merged), "merged" (its changes
    /// are on the base branch) or "empty" (nothing done yet).
    pub state: String,
}

/// A branch name for people: `worktree-agent-a367461d…` reads "Subagent a367461", `dino/fix-x` "fix-x".
pub fn readable_branch(branch: &str) -> String {
    let b = branch.strip_prefix("worktree-").or_else(|| branch.strip_prefix("dino/")).unwrap_or(branch);
    match b.strip_prefix("agent-") {
        Some(id) if !id.is_empty() && id.chars().all(|c| c.is_ascii_hexdigit()) => {
            format!("Subagent {}", &id[..id.len().min(7)])
        }
        _ => b.to_string(),
    }
}

/// `had_commits`: the branch moved since it was made. `same_as_base`: the files it changed read
/// the same on the base branch, so it landed even if squashed.
pub fn state(dirty: bool, ahead: u32, had_commits: bool, same_as_base: bool) -> &'static str {
    if dirty {
        "in_progress"
    } else if had_commits && (ahead == 0 || same_as_base) {
        "merged"
    } else if ahead > 0 {
        "ready"
    } else {
        "empty"
    }
}

/// What a summary reads from history alone, the same while HEAD and the base stay put.
#[derive(Clone)]
struct History {
    tips: String,
    mb: String,
    ahead: u32,
    had_commits: bool,
    same_as_base: bool,
    subject: Option<String>,
    /// Lines added and removed since the merge base, when nothing was uncommitted.
    clean: Option<(u32, u32)>,
}

/// By worktree, branch and base. Most looks find nothing committed since the last one, and every
/// git call is a process.
static HISTORY: Mutex<Option<HashMap<(PathBuf, Option<String>, String), History>>> = Mutex::new(None);

/// `dir` at a glance next to `base` (a branch of its repo). Only reads: it may be another agent's
/// worktree, so its index is left alone.
pub fn summary(dir: &Path, branch: Option<&str>, base: &str) -> anyhow::Result<Summary> {
    let tips = git(dir, &["rev-parse", "HEAD", base])?;
    let key = (dir.to_path_buf(), branch.map(String::from), base.to_string());
    let known = HISTORY.lock().unwrap().get_or_insert_default().get(&key).filter(|h| h.tips == tips).cloned();
    let mut h = match known {
        Some(h) => h,
        None => history(dir, branch, base, tips)?,
    };
    let status = git(dir, &["status", "--porcelain", "-uall"])?;
    let dirty = !status.trim().is_empty();
    // With nothing uncommitted, the diff from the merge base is history's too.
    let (mut added, removed) = match h.clean {
        Some(n) if !dirty => n,
        _ => {
            let (mut added, mut removed) = (0, 0);
            for line in git(dir, &["diff", "--numstat", &h.mb])?.lines() {
                let mut parts = line.split('\t');
                added += parts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
                removed += parts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
            }
            (added, removed)
        }
    };
    if !dirty {
        h.clean = Some((added, removed));
    }
    // New files aren't in the diff without touching the index: count their lines.
    for path in status.lines().filter_map(|l| l.strip_prefix("?? ")) {
        added += untracked_lines(&dir.join(path.trim_matches('"')));
    }
    let label = h.subject.clone().filter(|s| !s.is_empty()).or_else(|| branch.map(readable_branch)).unwrap_or_else(|| base_name(dir));
    let summary = Summary { label, added, removed, dirty, ahead: h.ahead, state: state(dirty, h.ahead, h.had_commits, h.same_as_base).into() };
    HISTORY.lock().unwrap().get_or_insert_default().insert(key, h);
    Ok(summary)
}

/// Lines in the untracked file at `file`, when it's a plain file under 1 MiB. Not through a
/// symlink, a FIFO or a device: an agent's worktree may hold one to `/dev/zero`, which never ends,
/// or a pipe, which waits forever.
fn untracked_lines(file: &Path) -> u32 {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    const MAX: u64 = 1 << 20;
    let plain = |m: std::fs::Metadata| m.file_type().is_file() && m.len() < MAX;
    if !std::fs::symlink_metadata(file).is_ok_and(plain) {
        return 0;
    }
    // Swapped since? Opened without following a link or waiting on a pipe, and checked again.
    let open = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(file);
    let Ok(f) = open else { return 0 };
    if !f.metadata().is_ok_and(plain) {
        return 0;
    }
    let mut b = Vec::new();
    if f.take(MAX).read_to_end(&mut b).is_err() {
        return 0;
    }
    b.iter().filter(|&&c| c == b'\n').count() as u32
}

fn history(dir: &Path, branch: Option<&str>, base: &str, tips: String) -> anyhow::Result<History> {
    let tip = tips.lines().next().unwrap_or_default().to_string();
    let mb = git(dir, &["merge-base", "HEAD", base])?.trim().to_string();
    let ahead: u32 = git(dir, &["rev-list", "--count", &format!("{base}..HEAD")])?.trim().parse().unwrap_or(0);
    // The oldest reflog entry is where the branch started; no reflog, then judge by the base.
    let start = branch
        .and_then(|b| git(dir, &["reflog", "show", "--format=%H", &format!("refs/heads/{b}")]).ok())
        .and_then(|log| log.lines().last().map(str::to_string));
    let had_commits = start.as_deref().map_or(ahead > 0, |s| s != tip);
    let same_as_base = ahead > 0 && {
        let changed = git(dir, &["diff", "--name-only", &mb, "HEAD"])?;
        let files: Vec<&str> = changed.lines().collect();
        !files.is_empty() && {
            let mut args = vec!["diff", "--quiet", base, "HEAD", "--"];
            args.extend(files);
            git(dir, &args).is_ok()
        }
    };
    let subject = if ahead > 0 { git(dir, &["log", "-1", "--format=%s"]).ok().map(|s| s.trim().to_string()) } else { None };
    Ok(History { tips, mb, ahead, had_commits, same_as_base, subject, clean: None })
}

fn base_name(dir: &Path) -> String {
    dir.file_name().map_or_else(|| dir.display().to_string(), |n| n.to_string_lossy().into_owned())
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
            Some(Worktree { path, branch, dino: false, git: None, owner: None })
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

/// Where dino makes `repo`'s worktrees: Settings → Worktrees → location.
pub fn worktrees_dir(repo: &Path) -> PathBuf {
    // Tests keep theirs inside their throwaway repos, whatever the settings say.
    if cfg!(test) {
        return repo.join(".dino/worktrees");
    }
    worktrees_dir_at(repo, &crate::settings::Settings::load().worktrees.location)
}

/// `location` relative to `repo` (blank: the default), or, absolute or under `~`, a folder per repo in it.
pub fn worktrees_dir_at(repo: &Path, location: &str) -> PathBuf {
    worktrees_dir_in(repo, location, std::env::var_os("DINO_HOME").map(PathBuf::from).filter(|h| !h.as_os_str().is_empty()))
}

/// As `worktrees_dir_at`, for a dinod whose home is `dino_home` (set: `DINO_HOME`). A dinod with
/// its own home (a test one, say) keeps the default worktrees in that home, not the user's
/// `~/.dino/worktrees`; a location set in Settings still wins.
fn worktrees_dir_in(repo: &Path, location: &str, dino_home: Option<PathBuf>) -> PathBuf {
    let location = location.trim().trim_end_matches('/');
    let name = || repo.file_name().map(PathBuf::from).unwrap_or_default();
    if let Some(home) = dino_home.filter(|_| location.is_empty() || location == crate::settings::DEFAULT_WORKTREE_LOCATION) {
        return home.join("worktrees").join(name());
    }
    if let Some(rest) = location.strip_prefix("~/").or((location == "~").then_some("")) {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        return home.join(rest).join(name());
    }
    match location {
        "" => worktrees_dir_in(repo, crate::settings::DEFAULT_WORKTREE_LOCATION, None),
        l if l.starts_with('/') => Path::new(l).join(name()),
        l => repo.join(l),
    }
}

/// The folder right inside `repo` that holds its worktrees, when they're inside it (to keep out of git status).
fn inner_dir(repo: &Path, dir: &Path) -> Option<String> {
    let rel = dir.strip_prefix(repo).ok()?;
    let first = rel.components().next()?.as_os_str().to_str()?;
    (first != "..").then(|| first.to_string())
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
    // Never the worktrees themselves, wherever they are.
    let inner = inner_dir(from, &worktrees_dir(from)).map(|d| format!("{d}/"));
    let (mut copied, mut bytes) = (vec![], 0u64);
    let mut wanted: Vec<String> = untracked(&format!("--exclude-from={}", include.display()), false).into_iter().collect();
    wanted.sort();
    for rel in wanted {
        if !ignored.contains(&rel) || rel.starts_with(".dino/") || inner.as_deref().is_some_and(|d| rel.starts_with(d)) {
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
    let dir = worktrees_dir(repo).join(name);
    if let Some(inner) = inner_dir(repo, &dir) {
        exclude_dir(repo, &inner)?;
    }
    std::fs::create_dir_all(dir.parent().unwrap())?;
    git(repo, &["worktree", "add", "--quiet", "-b", branch, &dir.to_string_lossy(), base])?;
    Ok(dir)
}

/// Make `dir` again as a worktree of `repo` on `branch`, for a session coming back from the
/// archive: the branch as it was; if it's gone (git deletes a pushed one as merged), from what
/// was pushed; else a new one off the repo's HEAD.
pub fn restore(repo: &Path, dir: &Path, branch: &str) -> anyhow::Result<()> {
    anyhow::ensure!(!dir.exists(), "{} is in the way", dir.display());
    if let Some(inner) = inner_dir(repo, dir) {
        exclude_dir(repo, &inner)?;
    }
    std::fs::create_dir_all(dir.parent().unwrap())?;
    let path = dir.to_string_lossy();
    let exists = git(repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).is_ok();
    // A worktree removed by hand leaves an entry that blocks the path.
    let _ = git(repo, &["worktree", "prune"]);
    let pushed = || {
        let remotes = git(repo, &["remote"]).unwrap_or_default();
        remotes.lines().map(|r| format!("refs/remotes/{}/{branch}", r.trim())).find(|r| git(repo, &["rev-parse", "--verify", "--quiet", r]).is_ok())
    };
    if exists {
        git(repo, &["worktree", "add", "--quiet", &path, branch])?;
    } else if let Some(remote) = pushed() {
        git(repo, &["worktree", "add", "--quiet", "--track", "-b", branch, &path, &remote])?;
    } else {
        git(repo, &["worktree", "add", "--quiet", "-b", branch, &path, "HEAD"])?;
    }
    Ok(())
}

/// `name` as a branch and folder name: lowercase ASCII letters and digits, runs of anything else
/// as one `-`, at most 40 characters. None when nothing usable is left.
pub fn slug(name: &str) -> Option<String> {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.truncate(40);
    let out = out.trim_end_matches('-');
    (!out.is_empty()).then(|| out.to_string())
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

/// Hide `repo/<inner>/` from git status via `.git/info/exclude`.
fn exclude_dir(repo: &Path, inner: &str) -> anyhow::Result<()> {
    let common = git(repo, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
    let exclude = PathBuf::from(common.trim()).join("info/exclude");
    let current = std::fs::read_to_string(&exclude).unwrap_or_default();
    let line = format!("/{inner}/");
    if !current.lines().any(|l| l.trim() == line) {
        std::fs::create_dir_all(exclude.parent().unwrap())?;
        let sep = if current.is_empty() || current.ends_with('\n') { "" } else { "\n" };
        std::fs::write(&exclude, format!("{current}{sep}{line}\n"))?;
    }
    Ok(())
}

/// Bytes `dir` takes on disk (like `du -sk`), not following symlinks. Slow for big trees: call off
/// the main thread.
pub fn disk_size(dir: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let mut total = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    let mut seen = std::collections::HashSet::new();
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            let Ok(m) = e.metadata() else { continue };
            // Hard links count once.
            if m.nlink() > 1 && !seen.insert((m.dev(), m.ino())) {
                continue;
            }
            total += m.blocks() * 512;
            if m.is_dir() {
                stack.push(e.path());
            }
        }
    }
    total
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

/// Remove a worktree whose work is done, and its branch if git agrees it's merged. Never forces:
/// a worktree with uncommitted work stays, and so does a branch git doesn't see merged (squashed).
/// Says whether the branch went too.
pub fn clean(dir: &Path) -> anyhow::Result<bool> {
    if !git(dir, &["status", "--porcelain", "-uall"])?.trim().is_empty() {
        anyhow::bail!("it has uncommitted changes");
    }
    let branch = git(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok().map(|b| b.trim().to_string());
    let main = list(dir)?.into_iter().next().map(|w| PathBuf::from(w.path)).ok_or_else(|| anyhow::anyhow!("no main checkout"))?;
    if main == dir || std::fs::canonicalize(&main).ok() == std::fs::canonicalize(dir).ok() {
        anyhow::bail!("that's the main checkout");
    }
    git(&main, &["worktree", "remove", &dir.to_string_lossy()])?;
    Ok(branch.is_some_and(|b| git(&main, &["branch", "-d", &b]).is_ok()))
}

/// Remove a worktree but keep its branch, to make it again later (`restore`). Never forces.
pub fn put_away(dir: &Path) -> anyhow::Result<()> {
    if !git(dir, &["status", "--porcelain", "-uall"])?.trim().is_empty() {
        anyhow::bail!("it has uncommitted changes");
    }
    let main = list(dir)?.into_iter().next().map(|w| PathBuf::from(w.path)).ok_or_else(|| anyhow::anyhow!("no main checkout"))?;
    if std::fs::canonicalize(&main).ok() == std::fs::canonicalize(dir).ok() {
        anyhow::bail!("that's the main checkout");
    }
    git(&main, &["worktree", "remove", &dir.to_string_lossy()])?;
    Ok(())
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
    fn slugs_are_branch_safe() {
        assert_eq!(slug("Nightly deps: bump & test!").as_deref(), Some("nightly-deps-bump-test"));
        assert_eq!(slug("  Übersicht  ").as_deref(), Some("bersicht"));
        assert_eq!(slug("…"), None);
        assert_eq!(slug(&"abcd ".repeat(20)).map(|s| s.len()), Some(39));
    }

    #[test]
    fn untracked_lines_only_read_plain_files() {
        let tmp = std::env::temp_dir().join(format!("dino-untracked-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("a.txt"), "one\ntwo\n").unwrap();
        assert_eq!(untracked_lines(&tmp.join("a.txt")), 2);
        std::fs::write(tmp.join("big.txt"), vec![b'\n'; 1 << 20]).unwrap();
        assert_eq!(untracked_lines(&tmp.join("big.txt")), 0);
        // Neither returns, read whole: they'd hang the daemon.
        std::os::unix::fs::symlink("/dev/zero", tmp.join("zero")).unwrap();
        assert_eq!(untracked_lines(&tmp.join("zero")), 0);
        assert!(Command::new("mkfifo").arg(tmp.join("pipe")).status().unwrap().success());
        assert_eq!(untracked_lines(&tmp.join("pipe")), 0);
        std::os::unix::fs::symlink(tmp.join("a.txt"), tmp.join("link.txt")).unwrap();
        assert_eq!(untracked_lines(&tmp.join("link.txt")), 0);
        assert_eq!(untracked_lines(&tmp.join("missing.txt")), 0);
        let _ = std::fs::remove_dir_all(&tmp);
    }

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
    fn readable_branch_names() {
        assert_eq!(readable_branch("worktree-agent-a61958b8bbd9bb8e1"), "Subagent a61958b");
        assert_eq!(readable_branch("agent-abc"), "Subagent abc");
        assert_eq!(readable_branch("dino/fix-login"), "fix-login");
        assert_eq!(readable_branch("worktree-review-code"), "review-code");
        assert_eq!(readable_branch("agent-not-hex"), "agent-not-hex");
        assert_eq!(readable_branch("main"), "main");
    }

    #[test]
    fn states() {
        assert_eq!(state(true, 3, true, true), "in_progress");
        assert_eq!(state(false, 0, false, false), "empty");
        assert_eq!(state(false, 2, true, false), "ready");
        assert_eq!(state(false, 0, true, false), "merged", "merged with a merge or fast-forward");
        assert_eq!(state(false, 2, true, true), "merged", "squashed onto the base");
    }

    #[test]
    fn summary_through_a_worktrees_life() {
        let tmp = std::env::temp_dir().join(format!("dino-summary-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let repo = tmp.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let commit = |dir: &Path, msg: &str| {
            git(dir, &["add", "-A"]).unwrap();
            git(dir, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", msg]).unwrap();
        };
        git(&repo, &["init", "-q", "-b", "main"]).unwrap();
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        commit(&repo, "init");

        let wt = tmp.join("agent-abc1234def");
        git(&repo, &["worktree", "add", "-q", "-b", "worktree-agent-abc1234def", &wt.to_string_lossy(), "main"]).unwrap();
        let s = |dir: &Path, b: &str| summary(dir, Some(b), "main").unwrap();
        let b = "worktree-agent-abc1234def";
        let fresh = s(&wt, b);
        assert_eq!((fresh.label.as_str(), fresh.state.as_str(), fresh.added, fresh.dirty), ("Subagent abc1234", "empty", 0, false));

        std::fs::write(wt.join("x.txt"), "x\ny\n").unwrap();
        std::fs::write(wt.join("a.txt"), "uno\n").unwrap();
        let busy = s(&wt, b);
        assert_eq!((busy.state.as_str(), busy.added, busy.removed, busy.dirty), ("in_progress", 3, 1, true));
        assert!(git(&wt, &["diff", "--cached", "--name-only"]).unwrap().trim().is_empty(), "the index is left alone");
        assert!(git(&wt, &["status", "--porcelain"]).unwrap().contains("?? x.txt"));
        assert!(clean(&wt).is_err(), "never removes uncommitted work");

        commit(&wt, "Add x");
        let ready = s(&wt, b);
        assert_eq!((ready.label.as_str(), ready.state.as_str(), ready.ahead, ready.added), ("Add x", "ready", 1, 3));

        // Squashed onto main: still ahead, but its changes are there.
        git(&repo, &["merge", "--squash", "-q", b]).unwrap();
        commit(&repo, "Squashed x");
        assert_eq!(s(&wt, b).state, "merged");
        assert!(!clean(&wt).unwrap(), "git doesn't see a squashed branch merged, so it stays");
        assert!(!wt.exists());
        assert!(!git(&repo, &["branch", "--list", b]).unwrap().trim().is_empty());

        // Merged the usual way: the branch goes too.
        let wt2 = tmp.join("two");
        git(&repo, &["worktree", "add", "-q", "-b", "two", &wt2.to_string_lossy(), "main"]).unwrap();
        std::fs::write(wt2.join("y.txt"), "y\n").unwrap();
        commit(&wt2, "Add y");
        git(&repo, &["merge", "--ff-only", "-q", "two"]).unwrap();
        let merged = s(&wt2, "two");
        assert_eq!((merged.state.as_str(), merged.ahead, merged.label.as_str()), ("merged", 0, "two"));
        assert!(clean(&wt2).unwrap());
        assert!(git(&repo, &["branch", "--list", "two"]).unwrap().trim().is_empty());
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
            Worktree { path: real(repo), branch: Some("main".into()), dino: false, git: None, owner: None },
            Worktree { path: real(&wt), branch: Some("dino/g/claude".into()), dino: false, git: None, owner: None },
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

    #[test]
    fn worktree_locations() {
        let repo = Path::new("/src/app");
        let at = |l: &str| worktrees_dir_in(repo, l, None);
        assert_eq!(at(".dino/worktrees"), PathBuf::from("/src/app/.dino/worktrees"));
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        // The user's own dinod (no DINO_HOME): the default stays ~/.dino/worktrees/<repo>.
        assert_eq!(at(""), home.join(".dino/worktrees/app"));
        assert_eq!(at(crate::settings::DEFAULT_WORKTREE_LOCATION), home.join(".dino/worktrees/app"));
        assert_eq!(at("/Volumes/big/wt/"), PathBuf::from("/Volumes/big/wt/app"));
        assert_eq!(at("~/worktrees"), home.join("worktrees/app"));
        // A dinod with its own home keeps default worktrees there; a chosen location still wins.
        let own = Some(PathBuf::from("/tmp/dino-test"));
        assert_eq!(worktrees_dir_in(repo, "", own.clone()), PathBuf::from("/tmp/dino-test/worktrees/app"));
        assert_eq!(worktrees_dir_in(repo, crate::settings::DEFAULT_WORKTREE_LOCATION, own.clone()), PathBuf::from("/tmp/dino-test/worktrees/app"));
        assert_eq!(worktrees_dir_in(repo, "/Volumes/big/wt", own.clone()), PathBuf::from("/Volumes/big/wt/app"));
        assert_eq!(worktrees_dir_in(repo, ".trees", own), PathBuf::from("/src/app/.trees"));
        assert_eq!(inner_dir(repo, &repo.join(".trees/x")).as_deref(), Some(".trees"));
        assert_eq!(inner_dir(repo, Path::new("/elsewhere/app")), None);
    }

    #[test]
    fn put_away_and_restore() {
        let tmp = std::env::temp_dir().join(format!("dino-putaway-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let repo = tmp.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let commit = |dir: &Path, msg: &str| {
            git(dir, &["add", "-A"]).unwrap();
            git(dir, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", msg]).unwrap();
        };
        git(&repo, &["init", "-q", "-b", "main"]).unwrap();
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        commit(&repo, "init");
        let wt = repo.join(".dino/worktrees/claude-ab12");
        restore(&repo, &wt, "dino/claude-ab12").unwrap();
        assert!(wt.join("a.txt").is_file(), "a missing branch starts from HEAD");
        assert!(disk_size(&wt) > 0);

        std::fs::write(wt.join("b.txt"), "two\n").unwrap();
        assert!(put_away(&wt).is_err(), "never removes uncommitted work");
        commit(&wt, "Add b");
        put_away(&wt).unwrap();
        assert!(!wt.exists());
        assert!(git(&repo, &["rev-parse", "--verify", "dino/claude-ab12"]).is_ok(), "the branch stays");
        assert!(put_away(&repo).is_err(), "never the main checkout");

        restore(&repo, &wt, "dino/claude-ab12").unwrap();
        assert!(wt.join("b.txt").is_file(), "back on its branch, commits and all");
        assert!(restore(&repo, &wt, "dino/claude-ab12").is_err(), "not over something in the way");

        // Pushed, then the branch deleted (git sees it merged into its upstream): back from the remote.
        let origin = tmp.join("origin.git");
        git(&tmp, &["init", "-q", "--bare", &origin.to_string_lossy()]).unwrap();
        git(&repo, &["remote", "add", "origin", &origin.to_string_lossy()]).unwrap();
        git(&wt, &["push", "-qu", "origin", "HEAD"]).unwrap();
        clean(&wt).unwrap();
        assert!(git(&repo, &["rev-parse", "--verify", "--quiet", "refs/heads/dino/claude-ab12"]).is_err(), "pushed counts as merged");
        restore(&repo, &wt, "dino/claude-ab12").unwrap();
        assert!(wt.join("b.txt").is_file(), "back from what was pushed");
        remove(&repo, &wt, "dino/claude-ab12");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
