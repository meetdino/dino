//! Your other Claude accounts in dinod: their tokens kept in the Keychain (the key file on Linux,
//! see `dino_core::account_store`), added by signing in to them in the browser (see
//! `claude_login`) or from a pasted `claude setup-token` token, removed and reordered; listed with
//! which one answers Claude Code now, its usage windows, and when a spent one resets, as the proxy
//! found them; named as the user names them (see `names`), account 1 by its email until then.
//! Never their tokens.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dino_core::account_store as store;
use dino_core::claude_token as token;
use dino_core::ipc::{ClaudeAccountInfo, ClaudeAccountsInfo, WindowInfo};
use dino_proxy::Quota;
use dino_proxy::fallback::Limited;

use crate::Daemon;
use crate::claude_login::{self, Checked, OwnLogin};

/// The proxy session the check's call goes through: no session of the user's.
const CHECK_SESSION: &str = "claude-account-check";

pub(crate) fn serve(d: &Arc<Daemon>, action: &str, value: Option<String>, account: Option<u32>, order: Vec<u32>) -> anyhow::Result<ClaudeAccountsInfo> {
    let mut added = None;
    match action {
        "status" => {}
        "add" => {
            let t =
                token::find(value.as_deref().unwrap_or_default()).ok_or_else(|| anyhow::anyhow!("that isn't a Claude token: paste the one claude setup-token printed (it starts with sk-ant-oat)"))?;
            added = Some(add(d, &t)?);
        }
        "login" | "create" => {
            let d2 = d.clone();
            claude_login::start(move |id, t| claude_login::finish(id, add_checked(&d2, &t)))?;
        }
        "login_code" => claude_login::code(value.as_deref().unwrap_or_default())?,
        "login_cancel" => claude_login::cancel(),
        "remove" => {
            let n = account.ok_or_else(|| anyhow::anyhow!("which account?"))?;
            anyhow::ensure!(store::load().taken(n), "there's no Account {n}");
            store::set(&[(n, None)])?;
            crate::keys_changed(d);
            forget_orgs();
            forget_names();
        }
        "rename" => {
            let n = account.ok_or_else(|| anyhow::anyhow!("which account?"))?;
            rename(n, value.as_deref().unwrap_or_default())?;
            d.proxy.set_account_names(proxy_names());
        }
        "order" => {
            let have = accounts();
            let mut numbers: Vec<u32> = have.iter().map(|(n, _)| *n).collect();
            let mut asked = order.clone();
            asked.sort();
            anyhow::ensure!(asked == numbers, "your accounts changed in the meantime; try again");
            // The same numbers, the tokens moved: account 2 is always the first tried.
            numbers.sort();
            let tokens: Vec<&str> = order.iter().filter_map(|n| have.iter().find(|(h, _)| h == n).map(|(_, t)| t.as_str())).collect();
            let changes: Vec<(u32, Option<&str>)> = numbers.iter().copied().zip(tokens.into_iter().map(Some)).collect();
            store::set(&changes)?;
            crate::keys_changed(d);
        }
        _ => anyhow::bail!("unknown action {action}"),
    }
    let mut info = info(d);
    info.added = added;
    Ok(info)
}

fn accounts() -> Vec<(u32, String)> {
    store::load().kept
}

/// Keep `t` as the first number free, unless it's one already.
fn add(d: &Daemon, t: &str) -> anyhow::Result<u32> {
    let have = accounts();
    if let Some((n, kept)) = have.iter().find(|(_, kept)| kept == t) {
        anyhow::bail!("that's {} already", label(*n, Some(kept)));
    }
    let all = store::load();
    let n = (2..).find(|n| !all.taken(*n)).unwrap_or(2);
    store::set(&[(n, Some(t))])?;
    crate::keys_changed(d);
    crate::subtoken::recheck();
    Ok(n)
}

/// Keep `t`, a token a browser sign-in just made, once Claude Code has checked it: whose account
/// it is (one already here is turned away, the user's own sign-in included), and how much is left.
/// Nothing of it is kept, listed or used for anything else until then, so an account signed in to
/// again leaves no trace. `Ok((n, note))`: added as account `n`; `note` says what couldn't be checked.
fn add_checked(d: &Daemon, t: &str) -> Result<(u32, Option<String>), claude_login::Failed> {
    add_checked_with(d, t, |t| {
        // The proxy knows the call it signs with `t` as `t`'s own while the check runs.
        let _checking = d.proxy.checking(t);
        let checked = claude_login::check(&d.proxy.base_url(CHECK_SESSION, "anthropic"), t);
        (checked, d.proxy.claude_account_seen(Some(t)).0)
    })
}

/// `add_checked`, with `check` saying what the check found and the organization Anthropic named.
fn add_checked_with(d: &Daemon, t: &str, check: impl FnOnce(&str) -> (Checked, Option<String>)) -> Result<(u32, Option<String>), claude_login::Failed> {
    let t = t.trim();
    if !token::valid(t) {
        return Err("Claude Code didn't print a token dino can use".to_string().into());
    }
    if let Some((n, k)) = accounts().into_iter().find(|(_, k)| k == t) {
        return Err(claude_login::Failed { said: format!("that's {} already", label(n, Some(&k))), duplicate: Some(n) });
    }
    let (checked, org) = check(t);
    if let Some(org) = &org
        && let Some(same) = same_account(d, 0, org)
    {
        let name = if same == 1 { format!("{}, the one Claude Code is signed in with", label(1, None)) } else { label(same, accounts().iter().find(|(m, _)| *m == same).map(|(_, t)| t.as_str())) };
        return Err(claude_login::Failed { said: format!("That's {name}, already added. Switch to another Claude account in your browser, then try again."), duplicate: Some(same) });
    }
    let note = match checked {
        // Anthropic said whose it is: it took the token, whatever Claude Code made of the answer.
        _ if org.is_some() => None,
        Checked::Signed => None,
        Checked::Refused(why) => return Err(format!("Anthropic didn't accept the sign-in: {why}").into()),
        Checked::Unknown(why) => Some(format!("Added, but dino couldn't check it yet ({why}). It's checked again the first time Claude Code uses it.")),
    };
    let n = add(d, t).map_err(|e| claude_login::Failed::from(e.to_string()))?;
    if let Some(org) = &org {
        remember_org(t, org);
    }
    Ok((n, note))
}

/// Which account already here is organization `org`, but account `n`: the one Claude Code signed
/// in with (1), or another added before.
fn same_account(d: &Daemon, n: u32, org: &str) -> Option<u32> {
    let known = orgs();
    let others: Vec<(u32, Option<String>)> =
        accounts().into_iter().filter(|(m, _)| *m != n).map(|(m, t)| (m, d.proxy.claude_account_seen(Some(&t)).0.or_else(|| known.get(&fingerprint(&t)).cloned()))).collect();
    same(org, others.into_iter(), || d.proxy.claude_account_seen(None).0.or_else(claude_login::own_org))
}

/// Of `others` (each account's number and organization, where known), and else Claude Code's own
/// sign-in (1, by `own`, asked last: `claude auth status` takes a moment), the one that is `org`.
fn same(org: &str, mut others: impl Iterator<Item = (u32, Option<String>)>, own: impl FnOnce() -> Option<String>) -> Option<u32> {
    others.find(|(_, o)| o.as_deref() == Some(org)).map(|(m, _)| m).or_else(|| (own().as_deref() == Some(org)).then_some(1))
}

/// Each added account's organization, as its check found it, by a fingerprint of its token (not
/// the token): kept across restarts, to tell an account added again from a new one.
fn orgs_file() -> std::path::PathBuf {
    dino_core::config_dir().join("claude-account-orgs.json")
}

fn fingerprint(t: &str) -> String {
    use sha2::Digest;
    let h = sha2::Sha256::digest(t.trim().as_bytes());
    h.iter().take(12).map(|b| format!("{b:02x}")).collect()
}

fn orgs() -> HashMap<String, String> {
    std::fs::read(orgs_file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn write_orgs(m: &HashMap<String, String>) {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let path = orgs_file();
    let tmp = path.with_extension("tmp");
    let ok = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp).and_then(|mut f| f.write_all(&serde_json::to_vec(m).unwrap_or_default()));
    if ok.is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

fn remember_org(t: &str, org: &str) {
    let mut m = orgs();
    m.insert(fingerprint(t), org.to_string());
    let have: Vec<String> = accounts().iter().map(|(_, t)| fingerprint(t)).collect();
    m.retain(|k, _| have.contains(k));
    write_orgs(&m);
}

/// Only the accounts still here.
fn forget_orgs() {
    let mut m = orgs();
    let have: Vec<String> = accounts().iter().map(|(_, t)| fingerprint(t)).collect();
    let before = m.len();
    m.retain(|k, _| have.contains(k));
    if m.len() != before {
        write_orgs(&m);
    }
}

pub(crate) fn info(d: &Daemon) -> ClaudeAccountsInfo {
    own_soon();
    let have = accounts();
    let tokens: Vec<&str> = have.iter().map(|(_, t)| t.as_str()).collect();
    let (own, others) = d.proxy.claude_accounts(&tokens);
    let numbers = std::iter::once(1).chain(have.iter().map(|(n, _)| *n));
    let windows = std::iter::once(None).chain(tokens.iter().map(|t| Some(*t))).map(|t| d.proxy.claude_account_seen(t).1);
    let names = std::iter::once(None).chain(have.iter().map(|(n, t)| name(*n, Some(t))));
    let accounts = rows(numbers.zip(std::iter::once(own).chain(others)).zip(windows).zip(names).map(|(((n, l), q), name)| (n, l, q, name)));
    let kept = store::load();
    let error = store::problem().map(|p| format!("dino can't read your other Claude accounts from the Keychain now: {p}.")).or_else(|| {
        let n: Vec<String> = kept.unreadable.iter().map(|n| format!("Account {n}")).collect();
        (!n.is_empty()).then(|| format!("{} is in your Keychain, but this dino can't read it (a dino signed differently saved it). Remove it and sign in again.", n.join(", ")))
    });
    ClaudeAccountsInfo { accounts, added: None, creating: None, error, login: claude_login::shown() }
}

/// For the state: every account with its windows, once there's more than one (see
/// `Proxy::claude_accounts_now`).
pub(crate) fn now(d: &Daemon) -> Option<Vec<ClaudeAccountInfo>> {
    Some(rows(d.proxy.claude_accounts_now()?.into_iter()))
}

/// Each account as it stands, in the order they're tried: the first that isn't spent answers.
/// Named as the user named it; account 1 (`name` `None`) by its own, or else its email.
fn rows(accounts: impl Iterator<Item = (u32, Option<Limited>, Option<Quota>, Option<String>)>) -> Vec<ClaudeAccountInfo> {
    let mut answering = false;
    let me = own();
    accounts
        .map(|(number, spent, quota, name)| {
            let first = spent.is_none() && !answering;
            answering |= first;
            let me = me.as_ref().filter(|_| number == 1);
            ClaudeAccountInfo {
                name: if number == 1 { self::name(1, None) } else { name },
                email: me.and_then(|m| m.email.clone()),
                plan: me.and_then(|m| m.plan.clone()),
                number,
                answering: first,
                spent: spent.is_some(),
                resets_at: spent.as_ref().and_then(|l| l.resets_at),
                retry_at: spent.as_ref().map(|l| l.retry_at),
                windows: quota.map(|q| q.windows.into_iter().map(|(name, w)| WindowInfo { name, utilization: w.utilization, resets_at: w.resets_at }).collect()).unwrap_or_default(),
            }
        })
        .collect()
}

/// After dinod's start (which moved what the key file held into the Keychain): while the Keychain
/// can't be read (locked) or the file still holds some, trying again every minute, and the proxy
/// told once what it can read changed.
pub(crate) fn start(d: &Arc<Daemon>) {
    // Account 1's email, for the list and the state, asked once now rather than as they're shown.
    refresh_own();
    let d = Arc::downgrade(d);
    std::thread::Builder::new()
        .name("claude-accounts".into())
        .spawn(move || {
            let mut seen = store::load();
            loop {
                std::thread::sleep(Duration::from_secs(60));
                let Some(d) = d.upgrade() else { return };
                let _ = store::migrate();
                let now = store::load();
                if now != seen {
                    crate::keys_changed(&d);
                    seen = now;
                }
            }
        })
        .ok();
}

// Names.

/// The names the user gave their Claude accounts (Settings → Accounts → Rename…), in
/// `claude-account-names.json` beside their organizations (mode 600), never in a repo or synced.
/// An added account's under a fingerprint of its token, as there, so the name follows the account
/// when the list is reordered and goes with it when it's removed. Account 1's under the email
/// Claude Code is signed in with ("own:<email>"), or "own" while that isn't known, so signing
/// Claude Code in as someone else doesn't hand them the name. Read once and kept in memory: the
/// state names the accounts many times a second.
fn names_file() -> std::path::PathBuf {
    dino_core::config_dir().join("claude-account-names.json")
}

/// The names as kept, by the file they were read from (tests change folders).
static NAMES: Mutex<Option<(std::path::PathBuf, HashMap<String, String>)>> = Mutex::new(None);

fn names() -> HashMap<String, String> {
    let path = names_file();
    let mut kept = NAMES.lock().unwrap();
    if let Some((p, m)) = kept.as_ref()
        && *p == path
    {
        return m.clone();
    }
    let m: HashMap<String, String> = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    *kept = Some((path, m.clone()));
    m
}

fn write_names(m: &HashMap<String, String>) -> anyhow::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let path = names_file();
    let tmp = path.with_extension("tmp");
    std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp).and_then(|mut f| f.write_all(&serde_json::to_vec_pretty(m)?))?;
    std::fs::rename(&tmp, &path)?;
    *NAMES.lock().unwrap() = Some((path, m.clone()));
    Ok(())
}

/// Where account `n`'s name is kept: by its token's fingerprint, or account 1's by `email`.
fn name_key(n: u32, token: Option<&str>, email: Option<&str>) -> Option<String> {
    if n == 1 {
        return Some(email.map_or_else(|| "own".to_string(), |e| format!("own:{}", e.to_lowercase())));
    }
    token.map(fingerprint)
}

/// The name account `n` (signing with `token`) was given, if any.
fn given(m: &HashMap<String, String>, n: u32, token: Option<&str>, email: Option<&str>) -> Option<String> {
    let mine = name_key(n, token, email).and_then(|k| m.get(&k).cloned());
    // Named before Claude Code's email was known.
    mine.or_else(|| (n == 1).then(|| m.get("own").cloned()).flatten())
}

/// What account `n` is called: the name given, or account 1's email; `None` for "Account <n>".
pub(crate) fn name(n: u32, token: Option<&str>) -> Option<String> {
    let email = (n == 1).then(own).flatten().and_then(|o| o.email);
    given(&names(), n, token, email.as_deref()).or(email)
}

/// As dino says it: "Work", "you@example.com", "Account 3".
pub(crate) fn label(n: u32, token: Option<&str>) -> String {
    dino_core::ipc::account_label(n, name(n, token).as_deref())
}

/// The name typed, as kept: on one line, trimmed, at most 60 characters. `None` (no name of its
/// own) for nothing, or for what it'd be called anyway: "Account 3", which would go wrong once
/// the list is reordered, or account 1's email, which follows Claude Code's sign-in.
fn tidy(typed: &str, email: Option<&str>) -> anyhow::Result<Option<String>> {
    let one_line: String = typed.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let name = one_line.split_whitespace().collect::<Vec<_>>().join(" ");
    anyhow::ensure!(name.chars().count() <= 60, "keep the name to 60 characters");
    let numbered = name.strip_prefix("Account ").or_else(|| name.strip_prefix("account ")).is_some_and(|n| n.parse::<u32>().is_ok());
    let email = email.is_some_and(|e| e.eq_ignore_ascii_case(&name));
    Ok((!name.is_empty() && !numbered && !email).then_some(name))
}

/// Call account `n` `typed`; nothing clears its name.
fn rename(n: u32, typed: &str) -> anyhow::Result<()> {
    let email = (n == 1).then(own).flatten().and_then(|o| o.email);
    let token = if n == 1 { None } else { Some(accounts().into_iter().find(|(m, _)| *m == n).map(|(_, t)| t).ok_or_else(|| anyhow::anyhow!("there's no Account {n} to rename"))?) };
    let name = tidy(typed, email.as_deref())?;
    let key = name_key(n, token.as_deref(), email.as_deref()).ok_or_else(|| anyhow::anyhow!("there's no Account {n} to rename"))?;
    let mut m = names();
    if n == 1 {
        // One name for Claude Code's sign-in: this one's.
        m.remove("own");
    }
    match name {
        Some(name) => m.insert(key, name),
        None => m.remove(&key),
    };
    write_names(&m)
}

/// Only the names of accounts still here (and account 1's).
fn forget_names() {
    let mut m = names();
    let have: Vec<String> = accounts().iter().map(|(_, t)| fingerprint(t)).collect();
    let before = m.len();
    m.retain(|k, _| k == "own" || k.starts_with("own:") || have.contains(k));
    if m.len() != before {
        let _ = write_names(&m);
    }
}

/// Each added account's name, by its token, for the proxy: what a session on it says ("On Work").
pub(crate) fn proxy_names() -> HashMap<String, String> {
    let m = names();
    accounts().into_iter().filter_map(|(n, t)| given(&m, n, Some(&t), None).map(|name| (t, name))).collect()
}

/// Account 1 as `claude auth status` last said, and when.
static OWN: Mutex<Option<(Instant, Option<OwnLogin>)>> = Mutex::new(None);
static ASKING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// What `claude auth status` just said of Claude Code's sign-in.
pub(crate) fn own_seen(login: Option<OwnLogin>) {
    *OWN.lock().unwrap() = Some((Instant::now(), login));
}

/// Account 1 as last seen. Never asked from here: the state names it many times a second, and
/// `claude auth status` is a process of its own.
fn own() -> Option<OwnLogin> {
    OWN.lock().unwrap().clone().and_then(|(_, o)| o)
}

/// Ask again, out of the way, once what was seen is a minute old: as the list is looked at
/// (Settings → Accounts, `dino claude-token`), so a new sign-in shows soon after.
fn own_soon() {
    if OWN.lock().unwrap().as_ref().is_none_or(|(at, _)| at.elapsed() > Duration::from_secs(60)) {
        refresh_own();
    }
}

/// Ask `claude auth status` again, on a thread of its own; once at a time.
fn refresh_own() {
    // Tests say who account 1 is (`own_seen`), never a late answer of Claude Code's.
    if cfg!(test) || ASKING.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return;
    }
    let asked = std::thread::Builder::new().name("claude-auth-status".into()).spawn(|| {
        let started = Instant::now();
        let _ = claude_login::own_login();
        // No answer (it took too long): what was seen before stands, for another minute.
        let mut seen = OWN.lock().unwrap();
        if seen.as_ref().is_none_or(|(at, _)| *at < started) {
            let before = seen.take().and_then(|(_, o)| o);
            *seen = Some((Instant::now(), before));
        }
        drop(seen);
        ASKING.store(false, std::sync::atomic::Ordering::Release);
    });
    if asked.is_err() {
        ASKING.store(false, std::sync::atomic::Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An account signed in to again is the one already here, whether added before or Claude
    /// Code's own; a new one is none, and an unknown organization is no match.
    #[test]
    fn the_same_account_is_found_by_its_organization() {
        let others = || vec![(2, Some("org-b".to_string())), (3, None), (4, Some("org-d".to_string()))].into_iter();
        assert_eq!(same("org-d", others(), || Some("org-a".into())), Some(4));
        assert_eq!(same("org-a", others(), || Some("org-a".into())), Some(1));
        assert_eq!(same("org-new", others(), || Some("org-a".into())), None);
        assert_eq!(same("org-new", others(), || None), None);
        // Claude Code isn't asked when another account matched already.
        assert_eq!(same("org-b", others(), || panic!("asked")), Some(2));
    }

    /// Signing in to an account that's here already leaves no trace, not even for a moment: while
    /// the check runs, the new token isn't kept, listed or handed to the proxy; after, it's turned
    /// away as that account. A new account is kept once checked. (The founder saw the list grow by
    /// a row behind "Already added": the token was kept before the check and dropped after.)
    #[test]
    fn an_account_signed_in_to_again_leaves_no_trace() {
        let _one = crate::tests::claude_accounts_lock();
        let d = crate::tests::shell_daemon_for_accounts();
        let two = "sk-ant-oat01-TWOTWOTWOTWOTWOTWOTWOTWOTWOTWOTWOTWOTWOTWOTWOTWOTW";
        let two_again = "sk-ant-oat01-TWOAGAINTWOAGAINTWOAGAINTWOAGAINTWOAGAINTWOAGAIN";
        let shown = |d: &Daemon| info(d).accounts.iter().map(|a| a.number).collect::<Vec<_>>();
        let traces = |d: &Daemon, t: &str| {
            let kept = store::load().kept.iter().any(|(_, k)| k == t);
            let proxied = d.proxy.claude_accounts_now().is_some_and(|a| a.len() > shown(d).len()) || dino_core::account_store::with_accounts(dino_core::load_keys()).values().any(|v| v == t);
            kept || proxied
        };
        // A new account: not kept while it's checked, kept after.
        let added = add_checked_with(&d, two, |t| {
            assert!(!traces(&d, t), "kept before it was checked");
            assert_eq!(shown(&d), [1], "listed before it was checked");
            (Checked::Signed, Some("org-two".into()))
        });
        let n = added.ok().unwrap().0;
        assert_eq!(shown(&d), [1, n]);

        // Account 2 signed in to again (a new token, the same organization): no trace, ever.
        let again = add_checked_with(&d, two_again, |t| {
            assert!(!traces(&d, t), "kept while it was checked");
            assert_eq!(shown(&d), [1, n], "an extra row while it was checked");
            (Checked::Signed, Some("org-two".into()))
        });
        let f = again.err().unwrap();
        assert_eq!(f.duplicate, Some(n), "{}", f.said);
        assert!(!traces(&d, two_again));
        assert_eq!(shown(&d), [1, n]);
        // The very same token: turned away before any check.
        let same = add_checked_with(&d, two, |_| panic!("checked a token that's kept already"));
        assert_eq!(same.err().unwrap().duplicate, Some(n));

        serve(&d, "remove", None, Some(n), vec![]).unwrap();
        assert!(store::load().kept.is_empty());
    }

    /// The founder's accounts were "Account 1, 2, 3, 4": now each can be named, and the name
    /// follows its account. Account 1 is called by Claude Code's email until named. A name kept
    /// by the token's fingerprint goes with the account when the list is reordered, and away with
    /// it when it's removed; nothing in the file is a token. Accounts never named keep their numbers.
    #[test]
    fn accounts_are_named_and_the_name_follows_the_account() {
        let _one = crate::tests::claude_accounts_lock();
        let d = crate::tests::shell_daemon_for_accounts();
        let a = "sk-ant-oat01-NAMEDAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let b = "sk-ant-oat01-NAMEDBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";
        let _ = std::fs::remove_file(names_file());
        *NAMES.lock().unwrap() = None;
        own_seen(Some(OwnLogin { org: Some("org-own".into()), email: Some("me@example.com".into()), plan: Some("max".into()) }));
        let listed = |i: &dino_core::ipc::ClaudeAccountsInfo| i.accounts.iter().map(|a| (a.number, a.label())).collect::<Vec<_>>();
        let state = |d: &Daemon| now(d).unwrap().iter().map(|a| (a.number, a.label())).collect::<Vec<_>>();

        assert_eq!(serve(&d, "add", Some(a.into()), None, vec![]).unwrap().added, Some(2));
        let i = serve(&d, "add", Some(b.into()), None, vec![]).unwrap();
        // Unnamed: account 1 by its email (and its plan), the others by number, as before.
        assert_eq!(listed(&i), [(1, "me@example.com".into()), (2, "Account 2".into()), (3, "Account 3".into())]);
        assert_eq!((i.accounts[0].email.as_deref(), i.accounts[0].plan.as_deref()), (Some("me@example.com"), Some("max")));
        assert_eq!((i.accounts[1].email.as_ref(), i.accounts[1].name.as_ref()), (None, None), "nothing known of an added account");

        let i = serve(&d, "rename", Some("  Work\n laptop ".into()), Some(3), vec![]).unwrap();
        assert_eq!(listed(&i)[2], (3, "Work laptop".into()), "on one line, trimmed");
        serve(&d, "rename", Some("Personal".into()), Some(1), vec![]).unwrap();
        assert_eq!(state(&d), [(1, "Personal".into()), (2, "Account 2".into()), (3, "Work laptop".into())], "the state, as the sidebar shows them");
        assert_eq!(d.proxy.claude_accounts_now().unwrap()[2].3.as_deref(), Some("Work laptop"), "the proxy names a session on it");
        assert!(serve(&d, "add", Some(b.into()), None, vec![]).unwrap_err().to_string().contains("Work laptop already"));
        let file = std::fs::read_to_string(names_file()).unwrap();
        assert!(!file.contains("sk-ant") && file.contains(&fingerprint(b)), "kept by fingerprint: {file}");
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(names_file()).unwrap().permissions()) & 0o777, 0o600);

        // Reordered: the name goes with its account to its new number.
        let i = serve(&d, "order", None, None, vec![3, 2]).unwrap();
        assert_eq!(listed(&i), [(1, "Personal".into()), (2, "Work laptop".into()), (3, "Account 3".into())]);
        // Read again from the file, as by a dinod started again.
        *NAMES.lock().unwrap() = None;
        assert_eq!(label(2, Some(b)), "Work laptop");

        // Its number, or account 1's email, as a name is no name: those follow the list and the sign-in.
        serve(&d, "rename", Some("Account 3".into()), Some(3), vec![]).unwrap();
        serve(&d, "rename", Some("ME@example.com".into()), Some(1), vec![]).unwrap();
        assert_eq!(listed(&info(&d)), [(1, "me@example.com".into()), (2, "Work laptop".into()), (3, "Account 3".into())]);
        assert!(serve(&d, "rename", Some("x".repeat(61)), Some(2), vec![]).is_err(), "too long");
        assert!(serve(&d, "rename", Some("Nine".into()), Some(9), vec![]).is_err(), "no such account");

        // Account 1 named, then Claude Code signed in as someone else: theirs is their email.
        serve(&d, "rename", Some("Personal".into()), Some(1), vec![]).unwrap();
        own_seen(Some(OwnLogin { email: Some("other@example.com".into()), ..Default::default() }));
        assert_eq!(label(1, None), "other@example.com");
        // Signed out, or a token Claude Code can't say whose it is: "Account 1".
        own_seen(None);
        assert_eq!(label(1, None), "Account 1");

        // Removed: its name goes with it, the others' stay.
        serve(&d, "remove", None, Some(2), vec![]).unwrap();
        let file = std::fs::read_to_string(names_file()).unwrap();
        assert!(!file.contains("Work laptop") && file.contains("Personal"), "{file}");
        // Added again, it's a new account with no name.
        assert_eq!(serve(&d, "add", Some(b.into()), None, vec![]).unwrap().accounts.iter().find(|a| a.number == 2).unwrap().label(), "Account 2");
        for n in [2, 3] {
            serve(&d, "remove", None, Some(n), vec![]).unwrap();
        }
        let _ = std::fs::remove_file(names_file());
        *NAMES.lock().unwrap() = None;
        *OWN.lock().unwrap() = None;
    }

    /// What `claude auth status --json` (2.1.296) says: a claude.ai sign-in has an email,
    /// organization and plan; a `CLAUDE_CODE_OAUTH_TOKEN` (what `claude setup-token` makes) says
    /// nothing of whose it is, and nor does an API key or being signed out.
    #[test]
    fn account_1_is_who_claude_auth_status_says() {
        let signed_in = br#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","email":"me@example.com","orgId":"org-1","orgName":"Me","subscriptionType":"max"}"#;
        assert_eq!(claude_login::parse_auth_status(signed_in), Some(OwnLogin { org: Some("org-1".into()), email: Some("me@example.com".into()), plan: Some("max".into()) }));
        let token = br#"{"loggedIn":true,"authMethod":"oauth_token","apiProvider":"firstParty","analyticsDisabled":false,"configDirectory":"/tmp/x"}"#;
        assert_eq!(claude_login::parse_auth_status(token), None);
        assert_eq!(claude_login::parse_auth_status(br#"{"loggedIn":false,"authMethod":"none"}"#), None);
        assert_eq!(claude_login::parse_auth_status(b"not json"), None);
    }

    #[test]
    fn a_fingerprint_is_not_the_token() {
        let t = "sk-ant-oat01-AbCdEfGhIjKlMnOpQrStUvWxYz0123456789_abcdefghijklmnop";
        let f = fingerprint(t);
        assert_eq!(f.len(), 24);
        assert!(!t.contains(&f) && !f.contains("sk-ant"));
        assert_eq!(f, fingerprint(&format!(" {t}\n")), "the same token kept with whitespace");
        assert_ne!(f, fingerprint("sk-ant-oat01-another"));
    }
}
