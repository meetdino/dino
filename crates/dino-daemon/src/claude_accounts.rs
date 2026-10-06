//! Your other Claude accounts in dinod: kept in the key store as `CLAUDE_ACCOUNT_<n>` (see
//! `dino_core::claude_token::accounts`), added from a pasted `claude setup-token` token or from a
//! shell running it, removed and reordered; listed with which one answers Claude Code now and when
//! a spent one resets, as the proxy found them. Never their tokens.

use std::sync::{Arc, Mutex};

use dino_core::claude_token::{self as token, account_key};
use dino_core::ipc::{ClaudeAccountInfo, ClaudeAccountsInfo, WindowInfo};
use dino_core::settings;
use dino_proxy::Quota;
use dino_proxy::fallback::Limited;

use crate::Daemon;
use crate::subtoken::{self, State};

static STATE: Mutex<State> = Mutex::new(State { creating: None, error: None });

pub(crate) fn serve(d: &Arc<Daemon>, action: &str, value: Option<String>, account: Option<u32>, order: Vec<u32>) -> anyhow::Result<ClaudeAccountsInfo> {
    let mut added = None;
    match action {
        "status" => {}
        "add" => {
            let t = token::find(value.as_deref().unwrap_or_default()).ok_or_else(|| anyhow::anyhow!("that isn't a Claude token: paste the one claude setup-token printed (it starts with sk-ant-oat)"))?;
            added = Some(add(d, &t)?);
        }
        "create" => {
            let d2 = d.clone();
            subtoken::setup_token_shell(d, "Claude account", &STATE, move |t| add(&d2, t).map(|_| ()))?;
        }
        "remove" => {
            let n = account.ok_or_else(|| anyhow::anyhow!("which account?"))?;
            anyhow::ensure!(accounts().iter().any(|(have, _)| *have == n), "there's no Account {n}");
            settings::set_key(&account_key(n), None)?;
            crate::keys_changed(d);
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
            let names: Vec<String> = numbers.iter().map(|n| account_key(*n)).collect();
            let changes: Vec<(&str, Option<&str>)> = names.iter().map(String::as_str).zip(tokens.into_iter().map(Some)).collect();
            settings::set_keys(&changes)?;
            crate::keys_changed(d);
        }
        _ => anyhow::bail!("unknown action {action}"),
    }
    let mut info = info(d);
    info.added = added;
    Ok(info)
}

fn accounts() -> Vec<(u32, String)> {
    token::accounts(&dino_core::load_keys())
}

/// Keep `t` as the first number free, unless it's one already.
fn add(d: &Daemon, t: &str) -> anyhow::Result<u32> {
    let have = accounts();
    if let Some((n, _)) = have.iter().find(|(_, kept)| kept == t) {
        anyhow::bail!("that's Account {n} already");
    }
    let n = (2..).find(|n| !have.iter().any(|(h, _)| h == n)).unwrap_or(2);
    settings::set_key(&account_key(n), Some(t))?;
    crate::keys_changed(d);
    Ok(n)
}

pub(crate) fn info(d: &Daemon) -> ClaudeAccountsInfo {
    let have = accounts();
    let tokens: Vec<&str> = have.iter().map(|(_, t)| t.as_str()).collect();
    let (own, others) = d.proxy.claude_accounts(&tokens);
    let numbers = std::iter::once(1).chain(have.iter().map(|(n, _)| *n));
    let accounts = rows(numbers.zip(std::iter::once(own).chain(others)).map(|(n, l)| (n, l, None)));
    let st = STATE.lock().unwrap();
    ClaudeAccountsInfo { accounts, added: None, creating: st.creating.clone(), error: st.error.clone() }
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
                windows: quota
                    .map(|q| q.windows.into_iter().map(|(name, w)| WindowInfo { name, utilization: w.utilization, resets_at: w.resets_at }).collect())
                    .unwrap_or_default(),
            }
        })
        .collect()
}
