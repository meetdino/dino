//! GitHub's API, for automations: through the user's own `gh` login (its token, read from
//! `gh auth token` and kept in memory only). Cheap to poll: every GET is conditional on the ETag of
//! the answer before (GitHub doesn't count a 304 against the rate limit), an answer under
//! `FRESH` old is reused without asking, and when the rate limit runs low nothing is asked until
//! it resets.

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

const API: &str = "https://api.github.com";

/// An answer this fresh is reused as it is: several automations on one repo share one look.
const FRESH: Duration = Duration::from_secs(20);

/// Calls kept back for the user's own `gh` and other tools once the rate limit gets this low.
const RESERVE: u64 = 200;

/// How long the token from `gh auth token` is used before it's read again (`gh auth refresh`,
/// another account).
const TOKEN_FOR: Duration = Duration::from_secs(30 * 60);

struct Cached {
    etag: Option<String>,
    body: Value,
    at: Instant,
}

pub(crate) struct GitHub {
    client: reqwest::blocking::Client,
    token: Mutex<Option<(String, Instant)>>,
    cache: Mutex<HashMap<String, Cached>>,
    /// Nothing is asked before this: rate limited, or no login.
    until: Mutex<Option<(Instant, String)>>,
    login: Mutex<Option<String>>,
}

impl Default for GitHub {
    fn default() -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent(concat!("dino/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default();
        Self { client, token: Mutex::default(), cache: Mutex::default(), until: Mutex::default(), login: Mutex::default() }
    }
}

impl GitHub {
    /// The user's token: `GH_TOKEN` or `GITHUB_TOKEN` as gh reads them, else gh's own login.
    fn token(&self) -> anyhow::Result<String> {
        if let Some((t, at)) = self.token.lock().unwrap().clone()
            && at.elapsed() < TOKEN_FOR
        {
            return Ok(t);
        }
        let from_env = ["GH_TOKEN", "GITHUB_TOKEN"].iter().find_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()));
        let token = match from_env {
            Some(t) => t,
            None => {
                let gh = dino_core::which("gh").ok_or_else(|| anyhow::anyhow!("GitHub triggers need the GitHub CLI: brew install gh, then gh auth login"))?;
                let out = Command::new(gh).args(["auth", "token", "--hostname", "github.com"]).stdin(Stdio::null()).stderr(Stdio::null()).output()?;
                let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
                anyhow::ensure!(out.status.success() && !t.is_empty(), "gh isn't signed in to GitHub: run gh auth login");
                t
            }
        };
        *self.token.lock().unwrap() = Some((token.clone(), Instant::now()));
        Ok(token)
    }

    fn held_back(&self) -> anyhow::Result<()> {
        let mut until = self.until.lock().unwrap();
        match until.as_ref() {
            Some((t, why)) if Instant::now() < *t => anyhow::bail!("{why}"),
            Some(_) => *until = None,
            None => {}
        }
        Ok(())
    }

    /// Note what the rate limit says, and hold back when it's low.
    fn note_limit(&self, h: &reqwest::header::HeaderMap) {
        let num = |k: &str| h.get(k).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok());
        let (Some(left), Some(reset)) = (num("x-ratelimit-remaining"), num("x-ratelimit-reset")) else { return };
        let resource = h.get("x-ratelimit-resource").and_then(|v| v.to_str().ok()).unwrap_or("core");
        // The search API has its own small limit; only the main one is shared with everything else.
        let low = if resource == "core" { left < RESERVE } else { left == 0 };
        if low {
            let wait = reset.saturating_sub(crate::now_secs()).clamp(30, 3600);
            *self.until.lock().unwrap() = Some((Instant::now() + Duration::from_secs(wait), format!("GitHub's rate limit is nearly used up; looking again in {} min", wait.div_ceil(60))));
        }
    }

    /// GET `path` (from `/repos/…`), as JSON. Unchanged since the last look: the answer from then.
    pub(crate) fn get(&self, path: &str) -> anyhow::Result<Value> {
        if let Some(c) = self.cache.lock().unwrap().get(path)
            && c.at.elapsed() < FRESH
        {
            return Ok(c.body.clone());
        }
        self.held_back()?;
        let token = self.token()?;
        let etag = self.cache.lock().unwrap().get(path).and_then(|c| c.etag.clone());
        let mut req = self.client.get(format!("{API}{path}")).bearer_auth(&token).header("Accept", "application/vnd.github+json").header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(e) = &etag {
            req = req.header("If-None-Match", e);
        }
        let resp = req.send()?;
        self.note_limit(resp.headers());
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_MODIFIED {
            let mut cache = self.cache.lock().unwrap();
            let c = cache.get_mut(path).ok_or_else(|| anyhow::anyhow!("GitHub said nothing changed about something dino never saw"))?;
            c.at = Instant::now();
            return Ok(c.body.clone());
        }
        if status == reqwest::StatusCode::UNAUTHORIZED {
            // Signed out or refreshed since: read the token again next time.
            *self.token.lock().unwrap() = None;
            anyhow::bail!("GitHub turned gh's login down: run gh auth login");
        }
        if matches!(status.as_u16(), 403 | 429) && let Some(secs) = resp.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok()) {
            *self.until.lock().unwrap() = Some((Instant::now() + Duration::from_secs(secs.clamp(30, 3600)), "GitHub asked dino to slow down".into()));
        }
        let new_etag = resp.headers().get("etag").and_then(|v| v.to_str().ok()).map(String::from);
        let text = resp.text()?;
        if !status.is_success() {
            let msg = serde_json::from_str::<Value>(&text).ok().and_then(|v| v["message"].as_str().map(String::from)).unwrap_or_else(|| status.to_string());
            anyhow::bail!("GitHub: {msg} ({path})");
        }
        let body: Value = serde_json::from_str(&text)?;
        let mut cache = self.cache.lock().unwrap();
        // Bounded: only the paths automations look at, and those that went away drop off here.
        if cache.len() > 256 {
            cache.retain(|_, c| c.at.elapsed() < Duration::from_secs(3600));
        }
        cache.insert(path.to_string(), Cached { etag: new_etag, body: body.clone(), at: Instant::now() });
        Ok(body)
    }

    pub(crate) fn post(&self, path: &str, body: &Value) -> anyhow::Result<Value> {
        self.held_back()?;
        let token = self.token()?;
        let resp = self
            .client
            .post(format!("{API}{path}"))
            .bearer_auth(&token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .json(body)
            .send()?;
        self.note_limit(resp.headers());
        let status = resp.status();
        let v: Value = resp.json().unwrap_or(Value::Null);
        anyhow::ensure!(status.is_success(), "GitHub: {} ({path})", v["message"].as_str().unwrap_or(status.as_str()));
        Ok(v)
    }

    /// Who gh is signed in as.
    pub(crate) fn login(&self) -> anyhow::Result<String> {
        if let Some(l) = self.login.lock().unwrap().clone() {
            return Ok(l);
        }
        let me = self.get("/user")?["login"].as_str().map(String::from).ok_or_else(|| anyhow::anyhow!("GitHub didn't say who you are"))?;
        *self.login.lock().unwrap() = Some(me.clone());
        Ok(me)
    }
}

/// `owner/name` of the GitHub repo the checkout at `dir` was cloned from (its `origin`).
pub(crate) fn repo_of(dir: &std::path::Path) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(["remote", "get-url", "origin"]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    parse_remote(String::from_utf8_lossy(&out.stdout).trim())
}

fn parse_remote(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("git@github.com:")
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))
        .or_else(|| url.strip_prefix("https://github.com/"))
        .or_else(|| url.strip_prefix("http://github.com/"))?;
    let rest = rest.trim_end_matches('/').trim_end_matches(".git");
    let mut parts = rest.split('/');
    let (owner, name) = (parts.next()?, parts.next()?);
    (!owner.is_empty() && !name.is_empty() && parts.next().is_none()).then(|| format!("{owner}/{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remotes() {
        assert_eq!(parse_remote("git@github.com:asdf/dino.git").as_deref(), Some("asdf/dino"));
        assert_eq!(parse_remote("https://github.com/asdf/dino").as_deref(), Some("asdf/dino"));
        assert_eq!(parse_remote("https://github.com/asdf/dino.git/").as_deref(), Some("asdf/dino"));
        assert_eq!(parse_remote("ssh://git@github.com/a/b.git").as_deref(), Some("a/b"));
        assert_eq!(parse_remote("https://gitlab.com/a/b"), None);
        assert_eq!(parse_remote("https://github.com/a/b/c"), None);
    }
}
