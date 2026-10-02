//! Keeping a `dino` installed with install.sh up to date. Once a day dinod reads the release feed
//! (the appcast the app's Sparkle reads too), and when it names a newer dino it downloads this
//! Mac's tarball, checks it against the SHA-256 and the release key's Ed25519 signature in the
//! feed, and puts the new binary in place of its own. dinod keeps running the old one until no
//! agent is working and no shell is running a command, then restarts into the new one; sessions
//! resume, as after `dino stop`.
//!
//! The `dino` inside Dino.app updates with the app, and Homebrew's with `brew upgrade`: neither is
//! touched here. Nor is a build without the release key, so development builds never update.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use sha2::Digest;

use crate::Daemon;

/// Where the feed lives: the newest release's appcast.xml, or what scripts/release.sh was told.
/// `DINO_UPDATE_FEED` at run time points elsewhere (a local server, to try an update).
const FEED: &str = match option_env!("DINO_UPDATE_FEED_URL") {
    Some(f) => f,
    None => "https://github.com/asdf9384/dino-releases/releases/latest/download/appcast.xml",
};
/// The release key's public half (Ed25519, base64), compiled in by scripts/release.sh.
const PUBLIC_KEY: Option<&str> = option_env!("DINO_UPDATE_PUBLIC_KEY");
const EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// The version this dinod put in place of its own binary, waiting to be started.
static INSTALLED: Mutex<Option<String>> = Mutex::new(None);

pub(crate) fn installed() -> Option<String> {
    INSTALLED.lock().unwrap().clone()
}

/// Look now and then; restart into what was installed once nothing is busy.
pub(crate) fn start(d: Arc<Daemon>) {
    let Some(exe) = std::env::current_exe().ok().filter(|e| updates_itself(e)) else { return };
    let key = PUBLIC_KEY.and_then(|k| base64::engine::general_purpose::STANDARD.decode(k.trim()).ok());
    let Some(key) = key.filter(|k| k.len() == 32) else { return };
    let feed = std::env::var("DINO_UPDATE_FEED").unwrap_or_else(|_| FEED.into());
    std::thread::spawn(move || {
        // Not in the way of starting up.
        std::thread::sleep(Duration::from_secs(if std::env::var_os("DINO_UPDATE_FEED").is_some() { 2 } else { 60 }));
        loop {
            if installed().is_none() && due() && dino_core::settings::Settings::load().machine.check_updates {
                stamp();
                match check(&feed, &key, &exe) {
                    Ok(Some(v)) => {
                        eprintln!("dinod: installed dino {v} in {}; it starts once nothing is busy", exe.display());
                        *INSTALLED.lock().unwrap() = Some(v);
                    }
                    Ok(None) => {}
                    Err(e) => eprintln!("dinod: couldn't update dino: {e:#}"),
                }
            }
            if installed().is_some() && idle(&d) {
                crate::restart_into(&d, &exe);
            }
            std::thread::sleep(Duration::from_secs(15));
        }
    });
}

/// A `dino` this process may replace: not inside an app bundle (Sparkle's) or Homebrew's tree
/// (`brew upgrade`'s), and in a folder this user can write.
fn updates_itself(exe: &Path) -> bool {
    let p = exe.to_string_lossy();
    if p.contains(".app/Contents/") || p.contains("/Cellar/") || p.contains("/homebrew/") || p.contains("/Homebrew/") {
        return false;
    }
    let Some(dir) = exe.parent() else { return false };
    let Ok(c) = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()) else { return false };
    // SAFETY: a valid C string.
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

/// No agent working, waiting on its background work or asking for permission, and no shell
/// running a command: restarting dinod now cuts nothing off.
pub(crate) fn idle(d: &Daemon) -> bool {
    let sessions = d.sessions.lock().unwrap().clone();
    sessions.iter().filter(|s| !s.pane.is_exited()).all(|s| {
        if s.agent_id == "shell" {
            return s.inside.lock().unwrap().fg.is_none();
        }
        let st = crate::stats(d, s);
        let (agents, commands) = st.waiting();
        !matches!(st.activity, Some(dino_proxy::Activity::Working | dino_proxy::Activity::NeedsPermission(_)))
            && agents + commands == 0
            && st.in_flight == 0
    })
}

fn stamp_path() -> PathBuf {
    dino_core::config_dir().join("update-checked")
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn due() -> bool {
    let last = std::fs::read_to_string(stamp_path()).ok().and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(0);
    now().saturating_sub(last) >= EVERY.as_secs()
}

fn stamp() {
    let _ = std::fs::write(stamp_path(), now().to_string());
}

/// One build of the CLI the feed offers.
#[derive(Debug, PartialEq)]
struct Cli {
    arch: String,
    url: String,
    sha256: String,
    signature: String,
}

/// The newest item's version and CLI builds.
fn parse(feed: &str) -> Option<(String, Vec<Cli>)> {
    let item = &feed[feed.find("<item>")?..];
    let item = &item[..item.find("</item>")?];
    let version = between(item, "<sparkle:shortVersionString>", "</sparkle:shortVersionString>")?.trim().to_string();
    let mut clis = Vec::new();
    let mut rest = item;
    while let Some(at) = rest.find("<dino:cli ") {
        rest = &rest[at..];
        let end = rest.find("/>")?;
        let tag = &rest[..end];
        let attr = |name: &str| between(tag, &format!("{name}=\""), "\"").map(|v| v.replace("&amp;", "&"));
        if let (Some(arch), Some(url), Some(sha256), Some(signature)) = (attr("arch"), attr("url"), attr("sha256"), attr("signature")) {
            clis.push(Cli { arch, url, sha256, signature });
        }
        rest = &rest[end..];
    }
    Some((version, clis))
}

fn between<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let from = s.find(open)? + open.len();
    let to = s[from..].find(close)? + from;
    Some(&s[from..to])
}

/// `a` is a later version than `b` (dotted numbers; anything else counts as 0).
fn newer(a: &str, b: &str) -> bool {
    let parts = |v: &str| v.split('.').map(|p| p.trim().parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    let (a, b) = (parts(a), parts(b));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    false
}

/// Download, check and put in place the feed's newer dino, if it names one. Returns its version.
fn check(feed: &str, key: &[u8], exe: &Path) -> anyhow::Result<Option<String>> {
    let client = reqwest::blocking::Client::builder().timeout(Duration::from_secs(120)).build()?;
    let text = client.get(feed).send()?.error_for_status()?.text()?;
    let (version, clis) = parse(&text).ok_or_else(|| anyhow::anyhow!("the feed has no release in it"))?;
    if !newer(&version, env!("CARGO_PKG_VERSION")) {
        return Ok(None);
    }
    let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "x86_64" };
    let cli = clis
        .iter()
        .find(|c| c.arch == arch)
        .or_else(|| clis.iter().find(|c| c.arch == "universal"))
        .ok_or_else(|| anyhow::anyhow!("dino {version} has no build for {arch}"))?;
    let bytes = client.get(&cli.url).send()?.error_for_status()?.bytes()?;
    let sum = hex::encode(sha2::Sha256::digest(&bytes));
    anyhow::ensure!(sum.eq_ignore_ascii_case(&cli.sha256), "dino {version}'s download doesn't match its SHA-256");
    let signature = base64::engine::general_purpose::STANDARD.decode(cli.signature.trim())?;
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key)
        .verify(&bytes, &signature)
        .map_err(|_| anyhow::anyhow!("dino {version}'s download isn't signed with the release key"))?;
    replace(exe, &bytes)?;
    Ok(Some(version))
}

/// Unpack the tarball next to `exe` (same file system) and rename the new binary over it.
fn replace(exe: &Path, tarball: &[u8]) -> anyhow::Result<()> {
    let dir = exe.parent().ok_or_else(|| anyhow::anyhow!("{} has no folder", exe.display()))?;
    let tmp = dir.join(format!(".dino-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir(&tmp)?;
    let result = (|| {
        std::fs::write(tmp.join("dino.tar.gz"), tarball)?;
        let ok = std::process::Command::new("/usr/bin/tar").arg("-xzf").arg(tmp.join("dino.tar.gz")).arg("-C").arg(&tmp).arg("dino").status()?.success();
        anyhow::ensure!(ok, "couldn't unpack the download");
        let new = tmp.join("dino");
        std::fs::set_permissions(&new, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
        // The new binary has to run before it replaces the one that works.
        let out = std::process::Command::new(&new).arg("--version").output()?;
        anyhow::ensure!(out.status.success(), "the downloaded dino doesn't run here");
        std::fs::rename(&new, exe)?;
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_newest_item_and_its_builds() {
        let feed = r#"<rss><channel><item><title>dino 0.2.0</title>
            <sparkle:version>120</sparkle:version><sparkle:shortVersionString>0.2.0</sparkle:shortVersionString>
            <enclosure url="https://x/Dino.zip" sparkle:edSignature="AA==" length="3" type="application/octet-stream"/>
            <dino:cli arch="arm64" url="https://x/a.tar.gz?a=1&amp;b=2" sha256="ab" signature="cd"/>
            <dino:cli arch="universal" url="https://x/u.tar.gz" sha256="ef" signature="01"/>
            </item><item><sparkle:shortVersionString>0.1.0</sparkle:shortVersionString></item></channel></rss>"#;
        let (v, clis) = parse(feed).unwrap();
        assert_eq!(v, "0.2.0");
        assert_eq!(clis.len(), 2);
        assert_eq!(clis[0], Cli { arch: "arm64".into(), url: "https://x/a.tar.gz?a=1&b=2".into(), sha256: "ab".into(), signature: "cd".into() });
        assert!(parse("<rss></rss>").is_none());
    }

    #[test]
    fn compares_versions_by_number() {
        assert!(newer("0.0.2", "0.0.1"));
        assert!(newer("0.10.0", "0.9.9"));
        assert!(newer("1.0", "0.99.99"));
        assert!(!newer("0.0.1", "0.0.1"));
        assert!(!newer("0.0.1", "0.0.2"));
        assert!(!newer("0.1", "0.1.0"));
    }

    #[test]
    fn leaves_app_and_homebrew_copies_alone() {
        assert!(!updates_itself(Path::new("/Applications/Dino.app/Contents/Helpers/dino")));
        assert!(!updates_itself(Path::new("/opt/homebrew/Cellar/dino-cli/0.0.1/bin/dino")));
        assert!(!updates_itself(Path::new("/opt/homebrew/bin/dino")));
        assert!(updates_itself(&std::env::temp_dir().join("dino")));
    }
}
