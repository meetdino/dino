//! Folder trust for the agents dino starts in its own worktrees.
//!
//! Claude asks "Do you trust the files in this folder?" once per project, keyed by path in
//! `~/.claude.json` (`projects.<path>.hasTrustDialogAccepted`), and a fan-out worktree is a new
//! path. When the repo it came from is trusted, dino marks the worktree trusted too, and forgets it
//! when the worktree goes. Codex already carries a repo's trust over to its worktrees.

use serde_json::{Map, Value, json};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

fn claude_config() -> PathBuf {
    match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(dir) => PathBuf::from(dir).join(".claude.json"),
        None => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".claude.json"),
    }
}

/// `p` as given and with symlinks resolved (`/tmp` is `/private/tmp`): Claude may use either.
fn spellings(p: &Path) -> Vec<String> {
    let mut out = vec![p.to_string_lossy().into_owned()];
    if let Ok(real) = std::fs::canonicalize(p) {
        let real = real.to_string_lossy().into_owned();
        if real != out[0] {
            out.push(real);
        }
    }
    out
}

fn read(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn accepted(config: &Value, p: &Path) -> bool {
    spellings(p).iter().any(|s| config["projects"][s.as_str()]["hasTrustDialogAccepted"] == json!(true))
}

/// Which folder from `dir` up to the repo `root` Claude trusts, relative to `root`. Like Claude, look
/// no higher than the repo: a trusted home folder doesn't make the repos in it trusted.
pub fn claude_trusted_in(dir: &Path, root: &Path) -> Option<PathBuf> {
    let config = read(&claude_config())?;
    let rel = dir.strip_prefix(root).unwrap_or(Path::new(""));
    rel.ancestors().find(|r| accepted(&config, &join(root, r))).map(Path::to_path_buf)
}

/// Whether Claude trusts `dir`, looking up to its repo's top, or at `dir` alone outside a repo.
pub fn claude_trusts(dir: &Path) -> bool {
    // Git answers with the real path; match it so a trusted subfolder is found.
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let root = crate::worktree::repo_root(&dir).unwrap_or_else(|_| dir.clone());
    claude_trusted_in(&dir, &root).is_some()
}

/// Extra args for a headless `claude -p`, which skips the trust prompt but still loads the
/// project's settings and runs their hooks: in a folder the user hasn't trusted, only theirs.
pub fn claude_headless_args(trusted: bool) -> Vec<String> {
    if trusted { vec![] } else { vec!["--setting-sources".into(), "user".into()] }
}

/// `root/rel` without a trailing slash when `rel` is empty: config keys have none.
pub fn join(root: &Path, rel: &Path) -> PathBuf {
    if rel.as_os_str().is_empty() { root.to_path_buf() } else { root.join(rel) }
}

/// Mark `dir` trusted for Claude.
pub fn claude_trust(dir: &Path) -> anyhow::Result<()> {
    edit(|projects| {
        for d in spellings(dir) {
            let entry = projects.entry(d).or_insert_with(|| Value::Object(Map::new()));
            if let Some(e) = entry.as_object_mut() {
                e.insert("hasTrustDialogAccepted".into(), json!(true));
            }
        }
    })
}

/// Drop what Claude keeps for `dir` and the folders in it: for a worktree that is gone.
pub fn claude_forget(dir: &Path) -> anyhow::Result<()> {
    let dirs = spellings(dir);
    edit(|projects| projects.retain(|k, _| !dirs.iter().any(|d| k == d || k.starts_with(&format!("{d}/")))))
}

fn edit(change: impl FnOnce(&mut Map<String, Value>)) -> anyhow::Result<()> {
    let path = claude_config();
    // No config means Claude never ran here; it will ask, as it would anyway.
    let Some(mut config) = read(&path) else { return Ok(()) };
    let Some(projects) = config.get_mut("projects").and_then(Value::as_object_mut) else { return Ok(()) };
    let before = projects.clone();
    change(projects);
    if *projects == before {
        return Ok(());
    }
    // Claude rewrites this file often: write it whole, then rename, so it never reads half of it.
    let tmp = path.with_extension(format!("json.dino-{}", std::process::id()));
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    f.write_all(serde_json::to_string_pretty(&config)?.as_bytes())?;
    drop(f);
    std::fs::rename(tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_trust() {
        let dir = std::env::temp_dir().join(format!("dino-trust-{}", std::process::id()));
        let repo = dir.join("repo");
        let wt = repo.join(".dino/worktrees/x/claude");
        std::fs::create_dir_all(wt.join("sub")).unwrap();
        std::fs::create_dir_all(repo.join("sub/deep")).unwrap();
        unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", &dir) };
        let config = dir.join(".claude.json");

        assert_eq!(claude_trusted_in(&repo, &repo), None);
        claude_trust(&wt).unwrap();
        assert!(!config.exists(), "no config, nothing written");

        // Claude keys by the real path; dino may hold the /tmp spelling.
        let real = |p: &Path| std::fs::canonicalize(p).unwrap().to_string_lossy().into_owned();
        std::fs::write(&config, json!({"zeta": 1, "projects": {real(&repo.join("sub")): {"hasTrustDialogAccepted": true, "history": [1]}, real(&dir): {"hasTrustDialogAccepted": true}}, "alpha": 2}).to_string()).unwrap();
        assert_eq!(claude_trusted_in(&repo, &repo), None, "a trusted folder above the repo doesn't count");
        assert_eq!(claude_trusted_in(&repo.join("sub/deep"), &repo), Some(PathBuf::from("sub")));

        claude_trust(&wt.join("sub")).unwrap();
        assert_eq!(claude_trusted_in(&wt.join("sub"), &wt), Some(PathBuf::from("sub")));
        let text = std::fs::read_to_string(&config).unwrap();
        assert!(text.find("zeta").unwrap() < text.find("alpha").unwrap(), "keeps Claude's key order");
        claude_forget(&wt).unwrap();
        assert_eq!(claude_trusted_in(&wt.join("sub"), &wt), None);
        let v = read(&config).unwrap();
        assert_eq!(v["projects"][real(&repo.join("sub"))]["history"], json!([1]), "other projects untouched");
        assert_eq!(v["projects"].as_object().unwrap().len(), 2);
        assert_eq!(std::fs::metadata(&config).unwrap().permissions().mode() & 0o777, 0o600);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn headless_args() {
        assert!(claude_headless_args(true).is_empty(), "trusted: the project's settings, as the user accepted");
        assert_eq!(claude_headless_args(false), ["--setting-sources", "user"]);
    }

    use std::os::unix::fs::PermissionsExt;
}
