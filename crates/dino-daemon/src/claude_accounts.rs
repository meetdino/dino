//! Your other Claude accounts in dinod: their tokens kept in the Keychain (the key file on Linux,
//! see `dino_core::account_store`), added by signing in to them in the browser (see
//! `claude_login`) or from a pasted `claude setup-token` token, removed and reordered; listed with
//! which one answers Claude Code now, its usage windows, and when a spent one resets, as the proxy
//! found them. Never their tokens.

use std::collections::HashMap;
use std::sync::Arc;

use dino_core::account_store as store;
use dino_core::claude_token as token;
use dino_core::ipc::{ClaudeAccountInfo, ClaudeAccountsInfo, WindowInfo};
use dino_proxy::Quota;
use dino_proxy::fallback::Limited;

use crate::Daemon;
use crate::claude_login::{self, Checked};

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
    if let Some((n, _)) = have.iter().find(|(_, kept)| kept == t) {
        anyhow::bail!("that's Account {n} already");
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
    if let Some((n, _)) = accounts().into_iter().find(|(_, k)| k == t) {
        return Err(claude_login::Failed { said: format!("that's Account {n} already"), duplicate: Some(n) });
    }
    let (checked, org) = check(t);
    if let Some(org) = &org
        && let Some(same) = same_account(d, 0, org)
    {
        let name = if same == 1 { "Account 1, the one Claude Code is signed in with".to_string() } else { format!("Account {same}") };
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
    let have = accounts();
    let tokens: Vec<&str> = have.iter().map(|(_, t)| t.as_str()).collect();
    let (own, others) = d.proxy.claude_accounts(&tokens);
    let numbers = std::iter::once(1).chain(have.iter().map(|(n, _)| *n));
    let windows = std::iter::once(None).chain(tokens.iter().map(|t| Some(*t))).map(|t| d.proxy.claude_account_seen(t).1);
    let accounts = rows(numbers.zip(std::iter::once(own).chain(others)).zip(windows).map(|((n, l), q)| (n, l, q)));
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
fn rows(accounts: impl Iterator<Item = (u32, Option<Limited>, Option<Quota>)>) -> Vec<ClaudeAccountInfo> {
    let mut answering = false;
    accounts
        .map(|(number, spent, quota)| {
            let first = spent.is_none() && !answering;
            answering |= first;
            ClaudeAccountInfo {
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
    let d = Arc::downgrade(d);
    std::thread::Builder::new()
        .name("claude-accounts".into())
        .spawn(move || {
            let mut seen = store::load();
            loop {
                std::thread::sleep(std::time::Duration::from_secs(60));
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
