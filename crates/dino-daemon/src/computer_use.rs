//! Agents using the Mac's apps: open-computer-use for every agent here that takes MCP servers
//! (Claude Code, Codex, Kimi, Qwen, Pi, Hermes, CodeWhale, OpenCode, Copilot). On unless turned
//! off (Settings → Agents, or the Welcome card), and for one agent unless turned off for it. On,
//! dino installs a pinned release of open-computer-use (MIT, iFurySt/open-codex-computer-use) in
//! its own folder, checked against npm's published hash and the developer's signature, and adds it
//! to each agent with the agent's own MCP command. An agent's own computer use (Claude Code's with
//! a Pro or Max plan) is never touched. macOS's Accessibility and Screen Recording go to
//! open-computer-use's own notarized app, through its `doctor`; dino asks macOS for nothing. Off,
//! dino removes every server it added (only those: it records what it added, and leaves one the
//! user changed) and the install.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use base64::Engine;
use dino_core::agent_mcp::{self, Server};
use dino_core::ipc::{ComputerUseAgentInfo, ComputerUseInfo};
use dino_core::settings::Settings;
use serde::{Deserialize, Serialize};
use sha2::Digest;

/// The release dino installs, and what it must be: npm's published SHA-512 of the package, and
/// the Developer ID and bundle its macOS app is signed and notarized as.
pub(crate) const VERSION: &str = "0.3.6";
const TARBALL: &str = "https://registry.npmjs.org/open-computer-use/-/open-computer-use-0.3.6.tgz";
const SHA512: &str = "pGNfWBBefl5qzQMo6rkC/e28sOfeczTQ0hEKkYn2sXwti38uz+fC4Y0oBdtnNACemsNVR06Dzp6SozJjkpkbSg==";
const TEAM: &str = "J9P29FA5BX";
const BUNDLE: &str = "com.ifuryst.opencomputeruse";
/// The server's name in each agent: its tools are then `mcp__open-computer-use__…`.
pub(crate) const NAME: &str = "open-computer-use";
const APP: &str = "Open Computer Use.app";

/// One change at a time: installs and agents' own commands don't overlap.
static BUSY: Mutex<()> = Mutex::new(());
/// Agents adding it to failed for since dinod started: tried again at its next start, not at
/// every change of settings.
static FAILED: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// What its `doctor` last said: (Accessibility, Screen Recording).
static GRANTED: Mutex<Option<(bool, bool)>> = Mutex::new(None);

fn dir() -> PathBuf {
    dino_core::config_dir().join("tools").join(format!("open-computer-use-{VERSION}"))
}

fn program() -> PathBuf {
    dir().join(APP).join("Contents/MacOS/OpenComputerUse")
}

fn server() -> Server {
    Server { command: program().display().to_string(), args: vec!["mcp".into()] }
}

fn installed() -> bool {
    program().is_file() && dir().join(".checked").is_file()
}

fn on() -> bool {
    // open-computer-use drives macOS apps: on Linux it's off, and dino adds nothing to agents.
    cfg!(target_os = "macos") && Settings::load().computer_use()
}

/// What dino added, by agent: the server as it added it. This Mac's own (never synced).
#[derive(Serialize, Deserialize, Default)]
struct Record {
    added: std::collections::BTreeMap<String, Added>,
    /// Agents it was turned off for (in Settings, or by removing it in the agent): never added
    /// again by itself.
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    declined: std::collections::BTreeSet<String>,
}

#[derive(Serialize, Deserialize, Clone)]
struct Added {
    command: String,
    args: Vec<String>,
}

impl Added {
    fn server(&self) -> Server {
        Server { command: self.command.clone(), args: self.args.clone() }
    }
}

fn record_path() -> PathBuf {
    dino_core::config_dir().join("computer-use.json")
}

fn record() -> Record {
    std::fs::read(record_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save(r: &Record) -> anyhow::Result<()> {
    std::fs::create_dir_all(dino_core::config_dir())?;
    let tmp = record_path().with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(r)?)?;
    std::fs::rename(tmp, record_path())?;
    Ok(())
}

/// The agents on this Mac, by id: where each one's program is.
fn agents_here() -> Vec<(String, String, PathBuf)> {
    let mut path = dino_core::discover::login_path().unwrap_or_default();
    if let Some(own) = std::env::var_os("PATH") {
        path.push(":");
        path.push(own);
    }
    dino_core::detect_agents_in(&path)
        .into_iter()
        .filter(|a| agent_mcp::AGENTS.contains(&a.kind.id))
        .map(|a| (a.kind.id.to_string(), a.kind.name.to_string(), a.path))
        .collect()
}

/// The agent has computer use of its own: how to turn it on. Claude Code's needs a Pro or Max plan.
fn native(id: &str, bin: &Path) -> Option<String> {
    match id {
        "claude" => {
            let (_, account) = dino_core::discover::sign_in_status("claude", bin)?;
            matches!(account.as_deref(), Some("Claude Pro" | "Claude Max")).then(|| "Also has its own: in Claude, /mcp → computer-use → Enable".into())
        }
        _ => None,
    }
}

pub(crate) fn info() -> ComputerUseInfo {
    let rec = record();
    let granted = *GRANTED.lock().unwrap();
    let ours = server();
    let agents = agents_here()
        .into_iter()
        .map(|(id, name, bin)| {
            let there = agent_mcp::find(&id, NAME);
            let added = rec.added.get(&id).map(Added::server);
            let program = bin.file_name().map_or(id.clone(), |n| n.to_string_lossy().into_owned());
            let command = agent_mcp::adding(&id, NAME, &ours).map(|c| agent_mcp::shown(&program, &c, Some((NAME, &ours)))).unwrap_or_default();
            ComputerUseAgentInfo {
                on: added.is_some() && there == added,
                theirs: there.is_some() && there != added,
                native: native(&id, &bin),
                id,
                name,
                command,
            }
        })
        .collect();
    ComputerUseInfo { version: VERSION.into(), installed: installed(), accessibility: granted.map(|g| g.0), screen_recording: granted.map(|g| g.1), agents }
}

/// Download the pinned release, check it, and put its macOS app in dino's folder.
pub(crate) fn install() -> anyhow::Result<()> {
    anyhow::ensure!(on(), "Turn on computer use first");
    let _one = BUSY.lock().unwrap();
    if installed() {
        return Ok(());
    }
    let client = reqwest::blocking::Client::builder().timeout(Duration::from_secs(120)).build()?;
    let body = client.get(TARBALL).send()?.error_for_status()?.bytes()?;
    let got = base64::engine::general_purpose::STANDARD.encode(sha2::Sha512::digest(&body));
    anyhow::ensure!(got == SHA512, "the download isn't open-computer-use {VERSION} as published (its SHA-512 differs); nothing was installed");
    let tmp = dir().with_file_name(format!("open-computer-use-{VERSION}.partial"));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    std::fs::write(tmp.join("package.tgz"), &body)?;
    // Only its macOS app and its license.
    let app = format!("package/dist/{APP}");
    let ok = std::process::Command::new("/usr/bin/tar").arg("-xzf").arg("package.tgz").arg(&app).arg("package/LICENSE").current_dir(&tmp).status()?.success();
    anyhow::ensure!(ok, "couldn't unpack open-computer-use");
    std::fs::rename(tmp.join(&app), tmp.join(APP))?;
    std::fs::rename(tmp.join("package/LICENSE"), tmp.join("LICENSE"))?;
    let _ = std::fs::remove_dir_all(tmp.join("package"));
    let _ = std::fs::remove_file(tmp.join("package.tgz"));
    signed(&tmp.join(APP))?;
    std::fs::write(tmp.join(".checked"), format!("{VERSION} {SHA512}\n"))?;
    let _ = std::fs::remove_dir_all(dir());
    std::fs::rename(&tmp, dir())?;
    Ok(())
}

/// Its app is intact and signed by its developer, as notarized.
fn signed(app: &Path) -> anyhow::Result<()> {
    let verify = std::process::Command::new("/usr/bin/codesign").args(["--verify", "--strict", "--deep"]).arg(app).output()?;
    anyhow::ensure!(verify.status.success(), "open-computer-use's app isn't intact: {}", String::from_utf8_lossy(&verify.stderr).trim());
    let about = std::process::Command::new("/usr/bin/codesign").arg("-dv").arg(app).output()?;
    let said = String::from_utf8_lossy(&about.stderr);
    let has = |k: &str, v: &str| said.lines().any(|l| l.trim() == format!("{k}={v}"));
    anyhow::ensure!(has("TeamIdentifier", TEAM) && has("Identifier", BUNDLE), "open-computer-use's app isn't signed by its developer");
    Ok(())
}

/// Ask its `doctor` what macOS granted its app. With something missing it also opens its own
/// setup window, which walks through granting it in System Settings.
pub(crate) fn permissions() -> anyhow::Result<()> {
    anyhow::ensure!(installed(), "open-computer-use isn't installed");
    let out = std::process::Command::new(program()).arg("doctor").stdin(std::process::Stdio::null()).output()?;
    let said = String::from_utf8_lossy(&out.stdout);
    let granted = parse_doctor(&said).ok_or_else(|| anyhow::anyhow!("open-computer-use's doctor said: {}", said.trim()))?;
    *GRANTED.lock().unwrap() = Some(granted);
    Ok(())
}

/// `Permissions: accessibility=granted, screenRecording=granted`.
fn parse_doctor(said: &str) -> Option<(bool, bool)> {
    let line = said.lines().find_map(|l| l.trim().strip_prefix("Permissions:"))?;
    let get = |k: &str| line.split(',').find_map(|p| p.trim().strip_prefix(k)?.strip_prefix('=')).map(|v| v.trim() == "granted");
    Some((get("accessibility")?, get("screenRecording")?))
}

/// Add open-computer-use to agent `id`, or remove what dino added.
pub(crate) fn set(id: &str, want: bool) -> anyhow::Result<()> {
    #[cfg(not(target_os = "macos"))]
    anyhow::ensure!(!want, "computer use is macOS-only for now");
    if want {
        anyhow::ensure!(on(), "Turn on computer use first");
        install()?;
    }
    let _one = BUSY.lock().unwrap();
    let (_, name, bin) = agents_here().into_iter().find(|a| a.0 == id).ok_or_else(|| anyhow::anyhow!("{id} isn't on this Mac"))?;
    let mut rec = record();
    if want {
        rec.declined.remove(id);
        add(&mut rec, id, &name, &bin)?;
    } else {
        rec.declined.insert(id.into());
        if let Some(added) = rec.added.get(id).cloned() {
            remove(id, &bin, &added)?;
            rec.added.remove(id);
        }
    }
    save(&rec)
}

/// Add it to agent `id`, unless the agent has a server by that name dino didn't add.
fn add(rec: &mut Record, id: &str, name: &str, bin: &Path) -> anyhow::Result<()> {
    let ours = server();
    match agent_mcp::find(id, NAME) {
        Some(s) if s == ours => {}
        Some(_) => anyhow::bail!("{name} already has an MCP server named {NAME} that dino didn't add, so dino left it alone"),
        None => {
            let change = agent_mcp::adding(id, NAME, &ours).ok_or_else(|| anyhow::anyhow!("dino can't add MCP servers to {name}"))?;
            agent_mcp::apply(id, bin, NAME, &change, Some(&ours))?;
            anyhow::ensure!(agent_mcp::find(id, NAME).as_ref() == Some(&ours), "{name} didn't keep the server dino added");
        }
    }
    rec.added.insert(id.into(), Added { command: ours.command, args: ours.args });
    Ok(())
}

/// Remove what dino added to agent `id`, when it's still as dino left it.
fn remove(id: &str, bin: &Path, added: &Added) -> anyhow::Result<()> {
    if agent_mcp::find(id, NAME) != Some(added.server()) {
        // Gone, or the user changed it: theirs now.
        return Ok(());
    }
    let change = agent_mcp::removing(id, NAME).ok_or_else(|| anyhow::anyhow!("dino can't remove MCP servers from {id}"))?;
    agent_mcp::apply(id, bin, NAME, &change, None)
}

/// Run when dinod starts and when settings change. On: installed, and added to every agent here
/// but those it was turned off for and those with their own server by that name. Off (turned off,
/// or by the organization): nothing dino added stays, nor its install.
pub(crate) fn reconcile() {
    if on() {
        add_everywhere();
        return;
    }
    let mut rec = record();
    if rec.added.is_empty() && !dir().exists() {
        return;
    }
    let _one = BUSY.lock().unwrap();
    let here = agents_here();
    for (id, added) in rec.added.clone() {
        let Some((_, _, bin)) = here.iter().find(|a| a.0 == id) else {
            // Uninstalled, config and all, or not found now: try again next time.
            continue;
        };
        match remove(&id, bin, &added) {
            Ok(()) => {
                rec.added.remove(&id);
            }
            Err(e) => eprintln!("dinod: couldn't remove {NAME} from {id}: {e:#}"),
        }
    }
    if let Err(e) = save(&rec) {
        eprintln!("dinod: couldn't save {}: {e:#}", record_path().display());
    }
    // Its helper (one per user, which any copy's clients share, the user's own included) may
    // still run from dino's install: never stopped from here, the install goes once it's gone.
    if rec.added.is_empty() && !runs_from_install() {
        let _ = std::fs::remove_dir_all(dir());
        *GRANTED.lock().unwrap() = None;
    }
}

fn add_everywhere() {
    let here = agents_here();
    let rec = record();
    let failed = FAILED.lock().unwrap().clone();
    let wanted = |rec: &Record, id: &str| !rec.declined.contains(id) && !rec.added.contains_key(id) && !failed.iter().any(|f| f == id);
    // Removed in the agent since dino added it (`claude mcp remove`): turned off for it.
    let removed: Vec<String> = rec.added.keys().filter(|id| here.iter().any(|a| &a.0 == *id) && agent_mcp::find(id, NAME).is_none()).cloned().collect();
    if removed.is_empty() && !here.iter().any(|a| wanted(&rec, &a.0) && agent_mcp::find(&a.0, NAME).is_none_or(|s| s == server())) {
        return;
    }
    if let Err(e) = install() {
        eprintln!("dinod: couldn't install {NAME}: {e:#}");
        return;
    }
    let _one = BUSY.lock().unwrap();
    let mut rec = record();
    for id in removed {
        rec.added.remove(&id);
        rec.declined.insert(id);
    }
    for (id, name, bin) in &here {
        if !wanted(&rec, id) {
            continue;
        }
        match agent_mcp::find(id, NAME) {
            // Their own: left alone.
            Some(s) if s != server() => {}
            _ => {
                if let Err(e) = add(&mut rec, id, name, bin) {
                    eprintln!("dinod: couldn't add {NAME} to {name}: {e:#}");
                    FAILED.lock().unwrap().push(id.clone());
                }
            }
        }
    }
    if let Err(e) = save(&rec) {
        eprintln!("dinod: couldn't save {}: {e:#}", record_path().display());
    }
}

/// Something runs from dino's install of open-computer-use: its helper, or a server an agent
/// started before it was removed.
fn runs_from_install() -> bool {
    let Ok(ours) = std::fs::canonicalize(dir()) else { return false };
    #[cfg(not(target_os = "macos"))]
    return dino_core::procinfo::pids_named("OpenComputerUse").into_iter().any(|pid| dino_core::procinfo::exe_of(pid).is_some_and(|e| Path::new(&e).starts_with(&ours)));
    #[cfg(target_os = "macos")]
    dino_core::procinfo::pids_named("OpenComputerUse").into_iter().any(|pid| {
        let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        let n = unsafe { libc::proc_pidpath(pid as i32, buf.as_mut_ptr() as *mut libc::c_void, buf.len() as u32) };
        n > 0 && Path::new(std::str::from_utf8(&buf[..n as usize]).unwrap_or_default()).starts_with(&ours)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn its_doctor_is_read() {
        assert_eq!(parse_doctor("Permissions: accessibility=granted, screenRecording=granted\n"), Some((true, true)));
        assert_eq!(parse_doctor("Permissions: accessibility=missing, screenRecording=granted"), Some((false, true)));
        assert_eq!(parse_doctor("something else"), None);
    }

    #[test]
    fn a_record_from_before_opting_out_reads() {
        let old: Record = serde_json::from_str(r#"{"added":{"claude":{"command":"/x/OpenComputerUse","args":["mcp"]}}}"#).unwrap();
        assert!(old.added.contains_key("claude") && old.declined.is_empty());
        let mut r = old;
        r.declined.insert("codex".into());
        let back: Record = serde_json::from_slice(&serde_json::to_vec(&r).unwrap()).unwrap();
        assert!(back.declined.contains("codex"));
        assert!(!String::from_utf8(serde_json::to_vec(&Record::default()).unwrap()).unwrap().contains("declined"));
    }
}
