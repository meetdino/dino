//! `dino login`, `dino logout` and `dino sync`: the dino account and settings sync, all through
//! dinod (which keeps the tokens and does the syncing).

use std::time::{Duration, Instant};

use dino_core::ipc::{Request, Response, SyncStatus};

use crate::client;

fn ask(action: &str, value: Option<String>) -> anyhow::Result<Response> {
    Ok(client::request(&Request::Sync { action: action.into(), value })?)
}

fn status() -> anyhow::Result<SyncStatus> {
    match ask("status", None)? {
        Response::Sync { status } => Ok(status),
        Response::Error { message } => anyhow::bail!("{message}"),
        _ => anyhow::bail!("unexpected reply"),
    }
}

fn expect_status(r: Response) -> anyhow::Result<SyncStatus> {
    match r {
        Response::Sync { status } => Ok(status),
        Response::Error { message } => anyhow::bail!("{message}"),
        _ => anyhow::bail!("unexpected reply"),
    }
}

const USAGE: &str = "usage: dino login [--device] [<server>]

Sign in to your dino account, so your settings follow you to your other Macs. They're encrypted
on this Mac first: the server can't read them. Sessions, terminal content and agent logins never
leave this Mac.

  --device   Show a code to enter on another device's browser, instead of opening one here.";

/// `dino login [--device] [<server>]`: sign in, then set up sync (a recovery key on the first Mac,
/// asking for it on the next).
pub fn login(args: &[String]) -> anyhow::Result<()> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return Ok(());
    }
    let device = args.iter().any(|a| a == "--device");
    let server = args.iter().find(|a| !a.starts_with("--")).cloned();
    let s = status()?;
    if s.phase != "signed_out" && s.phase != "signing_in" {
        println!("Signed in as {} ({}). `dino logout` signs out.", s.email.as_deref().unwrap_or("?"), s.server);
        return Ok(());
    }
    if device {
        let s = expect_status(ask("login_device", server)?)?;
        println!("On any device, go to\n\n  {}\n\nand enter the code  {}\n", s.device_url.as_deref().unwrap_or("?"), s.device_code.as_deref().unwrap_or("?"));
    } else {
        let Response::Connect { url } = ask("login", server)? else { anyhow::bail!("unexpected reply") };
        println!("Opening your browser to sign in. If it doesn't open, go to:\n\n  {url}\n");
        if std::env::var_os("DINO_NO_BROWSER").is_none() {
            let _ = std::process::Command::new("open").arg(&url).status();
        }
    }
    let until = Instant::now() + Duration::from_secs(11 * 60);
    let s = loop {
        std::thread::sleep(Duration::from_millis(500));
        let s = status()?;
        if s.phase != "signing_in" {
            break s;
        }
        anyhow::ensure!(Instant::now() < until, "gave up waiting for the sign-in");
    };
    after(&s)
}

fn after(s: &SyncStatus) -> anyhow::Result<()> {
    match s.phase.as_str() {
        "signed_out" => anyhow::bail!("{}", s.message.clone().unwrap_or_else(|| "not signed in".into())),
        "needs_key" => {
            println!("Signed in as {}. This account already syncs from another Mac.", s.email.as_deref().unwrap_or("?"));
            return wait_for_approval();
        }
        "conflict" => conflict(s),
        _ => {
            println!("Signed in as {}. Settings sync is on.", s.email.as_deref().unwrap_or("?"));
            if let Some(rk) = &s.recovery_key {
                println!("\nYour recovery key (shown once; keep it in your password manager):\n\n  {rk}\n");
                println!("Another Mac joining this account needs it. Lose it and every Mac, and sync starts over.");
                ask("ack_recovery", None)?;
            }
        }
    }
    Ok(())
}

/// The joining side: show the code once another Mac takes the request, and wait for it.
fn wait_for_approval() -> anyhow::Result<()> {
    println!("\nOn one of your other Macs, run  dino sync approve  (or open Settings → Dino Account).");
    println!("Or enter the recovery key instead:  dino sync join <recovery key>\n");
    let mut shown = None::<String>;
    loop {
        std::thread::sleep(Duration::from_millis(500));
        let s = status()?;
        match s.phase.as_str() {
            "needs_key" => {}
            "signed_out" => anyhow::bail!("{}", s.message.unwrap_or_else(|| "signed out".into())),
            _ => {
                println!("This Mac has the key.");
                return after(&s);
            }
        }
        // dinod asks within a moment of signing in; until then there's no request yet.
        let Some(join) = s.join else {
            if let Some(m) = s.message {
                anyhow::bail!("{m} (`dino sync ask` asks again)");
            }
            continue;
        };
        if join.code != shown {
            if let Some(code) = &join.code {
                println!("Another Mac took the request. Check it shows this code before approving:\n\n  {code}\n");
            } else {
                println!("Waiting for another Mac to approve this one…");
            }
            shown = join.code;
        }
    }
}

/// `dino sync approve [<n> | <id>]`: list the Macs asking for the key, or take one, show the code
/// to compare, and give it the key once the person says the codes match.
fn approve(which: Option<String>) -> anyhow::Result<()> {
    let s = status()?;
    anyhow::ensure!(s.phase == "ready", "this Mac isn't syncing, so it has no key to give");
    if s.approvals.is_empty() {
        println!("No Mac is asking for the key right now. On the new Mac, sign in with `dino login`.");
        return Ok(());
    }
    let Some(which) = which else {
        for (i, a) in s.approvals.iter().enumerate() {
            println!("  {}  {} ({})", i + 1, a.device, a.os);
        }
        println!("\n`dino sync approve <number>` to approve one, `dino sync deny <number>` to say no.");
        return Ok(());
    };
    let a = pick(&s, &which)?;
    let s = expect_status(ask("claim", Some(a.id.clone()))?)?;
    let code = s.approvals.iter().find(|x| x.id == a.id).and_then(|x| x.code.clone()).ok_or_else(|| anyhow::anyhow!("that request is gone"))?;
    println!("{} is asking for your settings' key. It should show:\n\n  {code}\n", a.device);
    print!("Is that the code on {}? [y/N] ", a.device);
    std::io::Write::flush(&mut std::io::stdout())?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    if !matches!(answer.trim(), "y" | "Y" | "yes") {
        expect_status(ask("deny", Some(a.id))?)?;
        println!("Not approved. If the codes differ, someone else may have asked: sign out of that device on your account page.");
        return Ok(());
    }
    expect_status(ask("grant", Some(a.id))?)?;
    println!("Approved. {} now syncs your settings.", a.device);
    Ok(())
}

fn pick(s: &SyncStatus, which: &str) -> anyhow::Result<dino_core::ipc::ApprovalRequest> {
    let by_number = which.parse::<usize>().ok().and_then(|n| n.checked_sub(1)).and_then(|i| s.approvals.get(i));
    by_number.or_else(|| s.approvals.iter().find(|a| a.id == which)).cloned().ok_or_else(|| anyhow::anyhow!("no such request: `dino sync approve` lists them"))
}

fn conflict(s: &SyncStatus) {
    let (here, there, differ) = s.conflict.unwrap_or_default();
    println!("This Mac has settings of its own: {here} only here, {there} only in your account, {differ} set differently.");
    println!("  dino sync resolve cloud   use your account's");
    println!("  dino sync resolve local   make your account this Mac's");
    println!("  dino sync resolve merge   keep both; where they differ, the newer one");
}

pub fn logout() -> anyhow::Result<()> {
    expect_status(ask("logout", None)?)?;
    println!("Signed out. This Mac's settings stay as they are.");
    Ok(())
}

/// `dino sync status|now|approve|deny|ask|join|resolve|keys|undo|reset`.
pub fn sync(args: &[String]) -> anyhow::Result<()> {
    let value = args.get(1).cloned();
    let s = match args.first().map(String::as_str) {
        None | Some("status") => status()?,
        Some("now") => expect_status(ask("now", None)?)?,
        Some("approve") => return approve(value),
        Some("deny") => {
            let s = status()?;
            let a = pick(&s, value.as_deref().ok_or_else(|| anyhow::anyhow!("usage: dino sync deny <number>"))?)?;
            expect_status(ask("deny", Some(a.id))?)?;
            println!("Said no to {}.", a.device);
            return Ok(());
        }
        Some("ask") => {
            expect_status(ask("ask", None)?)?;
            return wait_for_approval();
        }
        Some("join") => {
            let key = value.ok_or_else(|| anyhow::anyhow!("usage: dino sync join <recovery key>"))?;
            let s = expect_status(ask("join", Some(key))?)?;
            return after(&s);
        }
        Some("resolve") => expect_status(ask("resolve", value)?)?,
        Some("keys") => expect_status(ask("keys", value)?)?,
        Some("undo") => {
            expect_status(ask("undo", None)?)?;
            println!("Put back the settings from before the last sync changed them.");
            return Ok(());
        }
        Some("reset") => {
            let s = expect_status(ask("reset", None)?)?;
            println!("Sync was reset: your account holds this Mac's settings again, under a new key.");
            return after(&s);
        }
        Some(other) => anyhow::bail!("dino sync {other}? status, now, approve, deny, ask, join, resolve, keys on|off, undo, reset"),
    };
    print(&s);
    Ok(())
}

fn print(s: &SyncStatus) {
    let ago = |t: u64| {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let d = now.saturating_sub(t);
        if d < 60 { format!("{d}s ago") } else if d < 3600 { format!("{}m ago", d / 60) } else { format!("{}h ago", d / 3600) }
    };
    match s.phase.as_str() {
        "signed_out" => println!("Not signed in. `dino login` turns on settings sync."),
        "signing_in" => println!("Signing in…"),
        "needs_key" => {
            println!("Signed in as {}; this Mac needs your settings' key.", s.email.as_deref().unwrap_or("?"));
            match s.join.as_ref().and_then(|j| j.code.as_ref()) {
                Some(code) => println!("  Another Mac took the request; it should show  {code}"),
                None if s.join.is_some() => println!("  Waiting for another Mac to approve it (dino sync approve there)."),
                None => println!("  `dino sync ask` asks your other Macs again."),
            }
            println!("  Or: dino sync join <recovery key>");
        }
        "conflict" => conflict(s),
        _ => {
            println!("Signed in as {} at {}", s.email.as_deref().unwrap_or("?"), s.server);
            println!("  {} settings synced, {} waiting to send, last sync {}", s.synced, s.pending, s.last_sync.map(ago).unwrap_or_else(|| "never".into()));
            println!("  keys {}", if s.key_sync { "sync (encrypted)" } else { "stay on this Mac" });
            if let Some(u) = &s.account_url {
                println!("  devices and data: {u}");
            }
            for a in &s.approvals {
                println!("  {} is asking for your settings' key: dino sync approve", a.device);
            }
        }
    }
    if let Some(m) = &s.message {
        println!("\n{m}");
    }
}
