//! Signing in to a Claude account in the browser, for a token: dinod runs Claude Code's own
//! `claude setup-token`, unmodified, on a terminal nobody sees. Claude Code opens the browser on
//! claude.ai, where the user signs in (to any of their accounts), and its own sign-in finishes on
//! its own (a local callback) or with the code claude.ai shows pasted back (`code`). The token it
//! prints goes to whoever started the sign-in, and the terminal is gone at once, token and all.
//!
//! It runs with a `CLAUDE_CONFIG_DIR` of its own, so the user's own Claude Code login, settings
//! and Keychain entry are never read or written; setup-token saves nothing anyway
//! (https://code.claude.com/docs/en/authentication#generate-a-long-lived-token).
//!
//! [`check`] then has Claude Code itself (`claude -p`, signed in with the token) make one small call
//! through dino's proxy, which notes what Anthropic answers for it: whether it took the token, the
//! account's organization, its usage windows.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dino_core::claude_token as token;
use dino_core::ipc::ClaudeLoginInfo;
use dino_term::{Pane, SpawnSpec};

/// How long the user has to sign in.
const SIGN_IN_WITHIN: Duration = Duration::from_secs(10 * 60);
/// How long the check's one call may take.
const CHECK_WITHIN: Duration = Duration::from_secs(90);
/// Wide enough that the sign-in link and the token each fit on one line.
const COLS: u16 = 1000;

/// The sign-in shown to whoever asks: under way, or how the last one ended.
static SHOWN: Mutex<Option<ClaudeLoginInfo>> = Mutex::new(None);
/// The setup-token under way.
static RUNNING: Mutex<Option<Running>> = Mutex::new(None);
static NEXT: AtomicU64 = AtomicU64::new(1);

struct Running {
    id: u64,
    pane: Arc<Pane>,
}

/// What the check found.
pub(crate) enum Checked {
    /// Anthropic answered the call signed with it (an answer, or its limit's refusal).
    Signed,
    /// Anthropic turned the token down.
    Refused(String),
    /// No answer to tell either way (offline, Claude Code missing, timed out).
    Unknown(String),
}

/// The sign-in as it stands, for `status`.
pub(crate) fn shown() -> Option<ClaudeLoginInfo> {
    SHOWN.lock().unwrap().clone()
}

fn show(id: u64, f: impl FnOnce(&mut ClaudeLoginInfo)) {
    let mut s = SHOWN.lock().unwrap();
    if let Some(i) = s.as_mut().filter(|i| i.id == id) {
        f(i);
    }
}

/// Why a sign-in didn't add an account: what to say, and the account it is already, if that's why.
pub(crate) struct Failed {
    pub(crate) said: String,
    pub(crate) duplicate: Option<u32>,
}

impl From<String> for Failed {
    fn from(said: String) -> Self {
        Failed { said, duplicate: None }
    }
}

/// Note how sign-in `id` ended: `Ok(account)` added, maybe with what couldn't be checked; or why not.
pub(crate) fn finish(id: u64, outcome: Result<(u32, Option<String>), Failed>) {
    show(id, |i| {
        i.url = None;
        match outcome {
            Ok((n, note)) => {
                i.stage = "added".into();
                i.account = Some(n);
                i.error = note;
            }
            Err(f) => {
                i.stage = "failed".into();
                i.error = Some(f.said);
                i.duplicate = f.duplicate;
            }
        }
    });
}

/// Start a sign-in, unless one is under way (then it's that one). `got` is handed the token and
/// the sign-in's id, on a thread of its own, and calls [`finish`].
pub(crate) fn start(got: impl FnOnce(u64, String) + Send + 'static) -> anyhow::Result<ClaudeLoginInfo> {
    let claude = dino_core::which("claude").ok_or_else(|| anyhow::anyhow!("Claude Code isn't installed: install it first (Settings → Agents)"))?;
    let mut running = RUNNING.lock().unwrap();
    if let Some(r) = running.as_ref().filter(|r| !r.pane.is_exited())
        && let Some(i) = shown().filter(|i| i.id == r.id)
    {
        return Ok(i);
    }
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = scratch(id)?;
    // Claude Code's own command, as published: only where it keeps its config is its own, and no
    // credential from dinod's environment stands in for the sign-in (CLAUDE_CODE_OAUTH_TOKEN never
    // reaches a terminal dino starts).
    let mut args: Vec<String> = ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_BASE_URL"].iter().flat_map(|v| ["-u".to_string(), v.to_string()]).collect();
    args.extend([claude.display().to_string(), "setup-token".into()]);
    let spec = SpawnSpec { program: "/usr/bin/env".into(), args, cwd: Some(dir.clone()), env: [("CLAUDE_CONFIG_DIR".to_string(), dir.join("config").display().to_string())].into() };
    let pane = match Pane::spawn(spec, COLS, 50, |_| {}) {
        Ok(p) => p,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e);
        }
    };
    let info = ClaudeLoginInfo { id, stage: "starting".into(), ..Default::default() };
    *SHOWN.lock().unwrap() = Some(info.clone());
    // One at a time: an older one still around goes.
    if let Some(old) = running.replace(Running { id, pane: pane.clone() }) {
        old.pane.kill();
    }
    drop(running);
    std::thread::Builder::new().name("claude-login".into()).spawn(move || watch(id, pane, dir, got))?;
    Ok(info)
}

/// Follow sign-in `id`'s screen until its token shows, it fails, or it's cancelled.
fn watch(id: u64, pane: Arc<Pane>, dir: PathBuf, got: impl FnOnce(u64, String)) {
    let started = Instant::now();
    let outcome = loop {
        std::thread::sleep(Duration::from_millis(250));
        if RUNNING.lock().unwrap().as_ref().is_none_or(|r| r.id != id) {
            break None;
        }
        let text = pane.text(500);
        if let Some(t) = token::find(&text) {
            break Some(Ok(t));
        }
        if let Some(e) = token::setup_token_error(&text) {
            break Some(Err(format!("Claude Code couldn't finish the sign-in: {e}")));
        }
        if pane.is_exited() {
            // Claude Code's own last words (an account on hold, a policy that forbids it).
            let said = text.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or_default().to_string();
            break Some(Err(if said.is_empty() { "Claude Code stopped before the sign-in finished".into() } else { said }));
        }
        if started.elapsed() > SIGN_IN_WITHIN {
            break Some(Err("The sign-in didn't finish within 10 minutes. Try again when you're ready.".into()));
        }
        let url = token::find_sign_in_url(&text);
        show(id, |i| {
            if i.stage == "starting" && (url.is_some() || text.contains("Opening browser")) {
                i.stage = "browser".into();
            }
            if url.is_some() && i.url != url {
                i.url = url;
            }
        });
    };
    // Off the screen and out of memory as soon as it's read.
    pane.kill();
    {
        let mut running = RUNNING.lock().unwrap();
        if running.as_ref().is_some_and(|r| r.id == id) {
            *running = None;
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    match outcome {
        Some(Ok(t)) => {
            show(id, |i| {
                i.stage = "checking".into();
                i.url = None;
            });
            got(id, t);
        }
        Some(Err(e)) => finish(id, Err(e.into())),
        None => {}
    }
}

/// The code claude.ai showed, for a sign-in finished in another browser: typed into setup-token's
/// "Paste code here if prompted" for the user.
pub(crate) fn code(code: &str) -> anyhow::Result<()> {
    anyhow::ensure!(token::is_sign_in_code(code), "That isn't the code claude.ai shows after you sign in. Copy all of it (it has a # in the middle) and paste it again.");
    let pane = RUNNING.lock().unwrap().as_ref().map(|r| r.pane.clone()).ok_or_else(|| anyhow::anyhow!("The sign-in has ended. Start it again."))?;
    pane.write(code.trim().as_bytes().to_vec());
    // Its own keystroke, after the code, as a person's Return would be.
    std::thread::sleep(Duration::from_millis(150));
    pane.write(b"\r".to_vec());
    Ok(())
}

/// Stop the sign-in under way.
pub(crate) fn cancel() {
    let r = RUNNING.lock().unwrap().take();
    if let Some(r) = r {
        r.pane.kill();
        show(r.id, |i| {
            i.stage = "cancelled".into();
            i.url = None;
        });
    }
}

/// A folder only this sign-in uses, readable only by the user, gone when it ends.
fn scratch(id: u64) -> anyhow::Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let dir = std::env::temp_dir().join(format!("dino-claude-login-{}-{id}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::DirBuilder::new().mode(0o700).recursive(true).create(dir.join("config"))?;
    Ok(dir)
}

/// Have Claude Code, signed in with `token`, answer one small question through dino's proxy at
/// `base_url`, which notes what Anthropic says of the account (see `Proxy::claude_account_seen`).
/// Its own config folder, no tools, no MCP servers, nothing kept.
pub(crate) fn check(base_url: &str, token: &str) -> Checked {
    let Some(claude) = dino_core::which("claude") else { return Checked::Unknown("Claude Code isn't installed".into()) };
    let dir = match scratch(NEXT.fetch_add(1, Ordering::Relaxed)) {
        Ok(d) => d,
        Err(e) => return Checked::Unknown(e.to_string()),
    };
    let out = run_check(&claude, &dir, base_url, token);
    let _ = std::fs::remove_dir_all(&dir);
    out
}

fn run_check(claude: &Path, dir: &Path, base_url: &str, token: &str) -> Checked {
    let child = Command::new(claude)
        .args(["-p", "Reply with the word OK", "--model", "haiku", "--max-turns", "1", "--tools", "", "--strict-mcp-config", "--no-session-persistence", "--output-format", "json"])
        .current_dir(dir)
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env("CLAUDE_CONFIG_DIR", dir.join("config"))
        .env(token::KEY, token)
        .env("ANTHROPIC_BASE_URL", base_url)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return Checked::Unknown(e.to_string()),
    };
    let (mut stdout, mut stderr) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
    let out = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let err = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if started.elapsed() < CHECK_WITHIN => std::thread::sleep(Duration::from_millis(200)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let (out, err) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
    let Some(status) = status else { return Checked::Unknown("Claude Code didn't answer within 90 seconds".into()) };
    let reply: Option<serde_json::Value> = serde_json::from_str(out.trim()).ok();
    let failed = !status.success() || reply.as_ref().is_none_or(|r| r["is_error"].as_bool() != Some(false));
    if !failed {
        return Checked::Signed;
    }
    let said = reply.as_ref().and_then(|r| r["result"].as_str()).map(str::to_string).unwrap_or_else(|| err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or_default().to_string());
    // Never repeat a token, should Claude Code ever quote one.
    let said: String = said.split_whitespace().filter(|w| !w.contains("sk-ant-")).collect::<Vec<_>>().join(" ");
    let lower = said.to_lowercase();
    if ["401", "403", "authentication", "invalid bearer", "oauth token", "revoked", "/login"].iter().any(|w| lower.contains(w)) {
        Checked::Refused(said)
    } else {
        Checked::Unknown(if said.is_empty() { "Claude Code couldn't reach Anthropic".into() } else { said })
    }
}

/// The organization of the account Claude Code on this Mac is signed in with, as
/// `claude auth status` says (a claude.ai sign-in only): which account the user has already.
pub(crate) fn own_org() -> Option<String> {
    own_login().and_then(|o| o.org)
}

/// Who Claude Code on this Mac is signed in as, as `claude auth status --json` says for a
/// claude.ai sign-in (its email, organization and plan). `None` for anything else: signed out, an
/// API key, or a `CLAUDE_CODE_OAUTH_TOKEN`, which it reports with nothing of whose it is. Also
/// kept as account 1's (see `claude_accounts::own`).
pub(crate) fn own_login() -> Option<OwnLogin> {
    let claude = dino_core::which("claude")?;
    let mut child = Command::new(claude)
        .args(["auth", "status", "--json"])
        .env_remove(token::KEY)
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        if let Ok(Some(_)) = child.try_wait() {
            let out = child.wait_with_output().ok()?.stdout;
            let login = parse_auth_status(&out);
            crate::claude_accounts::own_seen(login.clone());
            return login;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

/// Account 1 as `claude auth status` reports a claude.ai sign-in.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct OwnLogin {
    pub org: Option<String>,
    pub email: Option<String>,
    /// "max", "pro", "team"…
    pub plan: Option<String>,
}

pub(crate) fn parse_auth_status(out: &[u8]) -> Option<OwnLogin> {
    let v: serde_json::Value = serde_json::from_slice(out).ok()?;
    if v["loggedIn"].as_bool() != Some(true) || v["authMethod"].as_str() != Some("claude.ai") {
        return None;
    }
    let text = |k: &str| v[k].as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    Some(OwnLogin { org: text("orgId"), email: text("email"), plan: text("subscriptionType") })
}
