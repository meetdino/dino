//! The Claude subscription token in dinod: made by `claude setup-token` in a shell the user
//! watches, kept in the key store, and noting whether Claude Code on this Mac is signed in on its
//! own (which decides whether sessions here get the token, see `dino_core::claude_token`).

use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use dino_core::claude_token::{self as token, CREATED_KEY, KEY, LIFETIME_SECS};
use dino_core::ipc::ClaudeTokenInfo;
use dino_core::settings;

use crate::{Daemon, Launch};

/// How long a `claude setup-token` shell has to show its token.
const CREATE_WITHIN: Duration = Duration::from_secs(10 * 60);
/// How often to ask whether Claude Code on this Mac is signed in, while there's a token.
const RECHECK: Duration = Duration::from_secs(15 * 60);

/// A `claude setup-token` shell dinod waits on, and what went wrong with the last one.
#[derive(Default)]
pub(crate) struct State {
    pub(crate) creating: Option<String>,
    pub(crate) error: Option<String>,
}

static STATE: Mutex<State> = Mutex::new(State { creating: None, error: None });

/// Sign-in checks under way: a Claude Code session starting meanwhile waits for their answer (see
/// [`settled`]), or it would start without the token a moment before dinod knew it needed it (#179).
static CHECKS: (Mutex<usize>, Condvar) = (Mutex::new(0), Condvar::new());
/// The most a session waits for one: a check gives up after 5 s.
const SETTLE_WITHIN: Duration = Duration::from_secs(6);

/// One check under way, from before its thread starts until it has noted its answer.
struct Check;

impl Check {
    fn begin() -> Self {
        *CHECKS.0.lock().unwrap() += 1;
        Check
    }
}

impl Drop for Check {
    fn drop(&mut self) {
        let mut n = CHECKS.0.lock().unwrap();
        *n = n.saturating_sub(1);
        CHECKS.1.notify_all();
    }
}

/// Once no sign-in check is under way (or [`SETTLE_WITHIN`] has passed): what
/// `dino_core::claude_token::signed_in` says then is the answer for the token kept now.
pub(crate) fn settled() {
    let n = CHECKS.0.lock().unwrap();
    let _ = CHECKS.1.wait_timeout_while(n, SETTLE_WITHIN, |n| *n > 0);
}

/// Keep the "signed in on its own" note fresh while a token is kept.
pub(crate) fn start() {
    let first = Check::begin();
    std::thread::Builder::new()
        .name("claude-token".into())
        .spawn(move || {
            check_signed_in();
            drop(first);
            loop {
                std::thread::sleep(RECHECK);
                let _check = Check::begin();
                check_signed_in();
            }
        })
        .ok();
}

/// Ask Claude Code (without any token in its environment) whether it's signed in, and note it for
/// dinod and `dino` commands. Only while a token is kept: otherwise nothing reads the answer.
fn check_signed_in() {
    let kept = dino_core::load_keys().get(KEY).is_some_and(|t| token::valid(t));
    if !kept {
        let _ = std::fs::remove_file(token::signed_in_file());
        return;
    }
    let Some(claude) = dino_core::which("claude") else { return };
    let mut child = match Command::new(claude)
        .args(["auth", "status", "--json"])
        .env_remove(KEY)
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return,
    };
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        if let Ok(Some(_)) = child.try_wait() {
            let out = child.wait_with_output().map(|o| o.stdout).unwrap_or_default();
            let signed_in = serde_json::from_slice::<serde_json::Value>(&out).ok().and_then(|v| v["loggedIn"].as_bool());
            if let Some(s) = signed_in {
                let _ = std::fs::write(token::signed_in_file(), if s { "yes\n" } else { "no\n" });
            }
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
}

pub(crate) fn info() -> ClaudeTokenInfo {
    let keys = dino_core::load_keys();
    let kept = keys.get(KEY).filter(|t| token::valid(t));
    let created = kept.and(keys.get(CREATED_KEY)).and_then(|c| c.parse::<u64>().ok());
    let s = STATE.lock().unwrap();
    ClaudeTokenInfo {
        set: kept.is_some(),
        masked: kept.map(|t| token::masked(t)),
        created,
        expires: created.map(|c| c + LIFETIME_SECS),
        signed_in: kept.and(token::signed_in()),
        creating: s.creating.clone(),
        error: s.error.clone(),
    }
}

pub(crate) fn serve(d: &Arc<Daemon>, action: &str, value: Option<String>) -> anyhow::Result<ClaudeTokenInfo> {
    match action {
        "status" => {}
        "set" => {
            let t = value.map(|v| v.trim().to_string()).unwrap_or_default();
            anyhow::ensure!(token::valid(&t), "that isn't a token from claude setup-token (they start with sk-ant-oat)");
            keep(&t, None)?;
        }
        "remove" => {
            settings::set_key(KEY, None)?;
            settings::set_key(CREATED_KEY, None)?;
            let _ = std::fs::remove_file(token::signed_in_file());
        }
        "create" => create(d)?,
        _ => anyhow::bail!("unknown action {action}"),
    }
    Ok(info())
}

/// Keep `t` (made now, if `created`), then look again at whether Claude Code here is signed in.
fn keep(t: &str, created: Option<u64>) -> anyhow::Result<()> {
    settings::set_key(KEY, Some(t))?;
    match created {
        Some(c) => settings::set_key(CREATED_KEY, Some(&c.to_string()))?,
        None => settings::set_key(CREATED_KEY, None)?,
    }
    STATE.lock().unwrap().error = None;
    // Counted before this returns, so a session asked for right after waits for its answer.
    let check = Check::begin();
    std::thread::spawn(move || {
        check_signed_in();
        drop(check);
    });
    Ok(())
}

fn create(d: &Arc<Daemon>) -> anyhow::Result<()> {
    setup_token_shell(d, "Claude subscription token", &STATE, |t| keep(t, Some(crate::now_secs())))
}

/// A shell that runs `claude setup-token`, in front of the user: they sign in in the browser, and
/// when the token shows up dinod hands it to `keep` and closes the shell, so it doesn't stay on
/// screen. `state` says meanwhile which shell it is, and after, what went wrong.
pub(crate) fn setup_token_shell(d: &Arc<Daemon>, label: &str, state: &'static Mutex<State>, keep: impl FnOnce(&str) -> anyhow::Result<()> + Send + 'static) -> anyhow::Result<()> {
    anyhow::ensure!(dino_core::which("claude").is_some(), "Claude Code isn't installed: install it first (Settings → Agents)");
    if let Some(open) = state.lock().unwrap().creating.clone() {
        if d.sessions.lock().unwrap().iter().any(|s| s.id == open && !s.pane.is_exited()) {
            return Ok(());
        }
    }
    let id = crate::spawn(d, Launch::new("shell", vec![], Some(crate::home().display().to_string())))?;
    let s = d.sessions.lock().unwrap().iter().find(|s| s.id == id).cloned().ok_or_else(|| anyhow::anyhow!("the shell has closed"))?;
    *s.label.lock().unwrap() = Some(label.into());
    // No token from anywhere else in the shell that makes one.
    crate::type_at_prompt(s.clone(), format!("unset {KEY}; claude setup-token"));
    {
        let mut st = state.lock().unwrap();
        st.creating = Some(id.clone());
        st.error = None;
    }
    let d = d.clone();
    std::thread::spawn(move || {
        let started = Instant::now();
        let outcome = loop {
            std::thread::sleep(Duration::from_millis(500));
            if s.pane.is_exited() || !d.sessions.lock().unwrap().iter().any(|x| x.id == id) {
                break Err("the shell closed before claude setup-token printed a token".to_string());
            }
            if let Some(t) = token::find(&s.pane.text(200)) {
                break keep(&t).map_err(|e| e.to_string());
            }
            if started.elapsed() > CREATE_WITHIN {
                break Err("claude setup-token didn't print a token within 10 minutes".to_string());
            }
        };
        let ok = outcome.is_ok();
        {
            let mut st = state.lock().unwrap();
            st.creating = None;
            st.error = outcome.err();
        }
        if ok {
            // Long enough to see it worked, then the token goes off screen with the shell.
            std::thread::sleep(Duration::from_secs(2));
            crate::kill(&d, &id);
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A session started while a check is under way waits for it, and only that long.
    #[test]
    fn a_session_waits_for_the_check_under_way() {
        let check = Check::begin();
        let started = Instant::now();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            drop(check);
        });
        settled();
        let waited = started.elapsed();
        t.join().unwrap();
        assert!(waited >= Duration::from_millis(250) && waited < SETTLE_WITHIN, "{waited:?}");
        // Nothing under way: no wait.
        let started = Instant::now();
        settled();
        assert!(started.elapsed() < Duration::from_millis(100));
    }
}
