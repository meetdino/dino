//! `dino login`, `dino logout` and `dino sync`: the dino account and settings sync, all through
//! dinod (which keeps the tokens and does the syncing).

use std::time::{Duration, Instant};

use dino_core::ipc::{Request, Response, SyncStatus};

use crate::client;

fn ask(action: &str, value: Option<String>) -> anyhow::Result<Response> {
    Ok(client::request(&Request::Sync { action: action.into(), value })?)
}

fn status() -> anyhow::Result<SyncStatus> {
    expect_status(ask("status", None)?)
}

fn expect_status(r: Response) -> anyhow::Result<SyncStatus> {
    match r {
        Response::Sync { status } => Ok(status),
        Response::Error { message } => anyhow::bail!("{message}"),
        _ => anyhow::bail!("unexpected reply"),
    }
}

const USAGE: &str = "usage: dino login [--email [<address>] | --device] [<server>]

Sign in to your dino account with GitHub, so your settings follow you to your other Macs. API
keys and tokens, sessions, terminal content and agent logins never leave this Mac.

  --email    Get a sign-in link by email instead.
  --device   Show a code to enter on another device's browser (for a Mac over SSH).";

/// `dino login [--email [<address>] | --device] [<server>]`: sign in, and settings sync is on.
pub fn login(args: &[String]) -> anyhow::Result<()> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return Ok(());
    }
    let device = args.iter().any(|a| a == "--device");
    let by_email = args.iter().any(|a| a == "--email");
    let mut rest = args.iter().filter(|a| !a.starts_with("--")).cloned();
    let s = status()?;
    if s.phase != "signed_out" && s.phase != "signing_in" {
        println!("Signed in as {} ({}). `dino logout` signs out.", s.email.as_deref().unwrap_or("?"), s.server);
        return Ok(());
    }
    if by_email {
        let email = match rest.find(|a| a.contains('@')) {
            Some(e) => e,
            None => {
                print!("Email: ");
                std::io::Write::flush(&mut std::io::stdout())?;
                let mut line = String::new();
                std::io::stdin().read_line(&mut line)?;
                line.trim().to_string()
            }
        };
        let s = expect_status(ask("login_email", Some(email))?)?;
        println!("Check your email: a sign-in link is on its way to {}. Open it on any device.\n", s.email_sent_to.as_deref().unwrap_or("you"));
    } else if device {
        let s = expect_status(ask("login_device", rest.next())?)?;
        println!("On any device, go to\n\n  {}\n\nand enter the code  {}\n", s.device_url.as_deref().unwrap_or("?"), s.device_code.as_deref().unwrap_or("?"));
    } else {
        let Response::Connect { url } = ask("login", rest.next())? else { anyhow::bail!("unexpected reply") };
        println!("Opening your browser to sign in with GitHub. If it doesn't open, go to:\n\n  {url}\n");
        if std::env::var_os("DINO_NO_BROWSER").is_none() {
            let _ = std::process::Command::new("open").arg(&url).status();
        }
    }
    let until = Instant::now() + Duration::from_secs(16 * 60);
    let s = loop {
        std::thread::sleep(Duration::from_millis(500));
        let s = status()?;
        if s.phase != "signing_in" && s.phase != "joining" {
            break s;
        }
        anyhow::ensure!(Instant::now() < until, "gave up waiting for the sign-in");
    };
    after(&s)
}

fn after(s: &SyncStatus) -> anyhow::Result<()> {
    match s.phase.as_str() {
        "signed_out" => anyhow::bail!("{}", s.message.clone().unwrap_or_else(|| "not signed in".into())),
        "conflict" => conflict(s),
        _ => println!("Signed in as {}. Settings sync is on.", s.email.as_deref().unwrap_or("?")),
    }
    Ok(())
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

/// `dino sync status|now|resolve|undo`.
pub fn sync(args: &[String]) -> anyhow::Result<()> {
    let value = args.get(1).cloned();
    let s = match args.first().map(String::as_str) {
        None | Some("status") => status()?,
        Some("now") => expect_status(ask("now", None)?)?,
        Some("resolve") => expect_status(ask("resolve", value)?)?,
        Some("undo") => {
            expect_status(ask("undo", None)?)?;
            println!("Put back the settings from before the last sync changed them.");
            return Ok(());
        }
        Some(other) => anyhow::bail!("dino sync {other}? status, now, resolve cloud|local|merge, undo"),
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
        "signing_in" => match &s.email_sent_to {
            Some(e) => println!("Waiting for the sign-in link sent to {e} to be opened."),
            None => println!("Signing in…"),
        },
        "joining" => println!("Signed in; fetching your settings…"),
        "conflict" => conflict(s),
        _ => {
            println!("Signed in as {} at {}", s.email.as_deref().unwrap_or("?"), s.server);
            println!("  {} settings synced, {} waiting to send, last sync {}", s.synced, s.pending, s.last_sync.map(ago).unwrap_or_else(|| "never".into()));
            if let Some(u) = &s.account_url {
                println!("  devices and data: {u}");
            }
        }
    }
    if let Some(m) = &s.message {
        println!("\n{m}");
    }
}
