//! Settings sync, as this Mac's side of it, like VS Code's: sign in, and your settings are on every
//! Mac you sign in on. Values travel as plain JSON over TLS; secrets (the key store's API keys and
//! tokens) never sync. Every change made here, by the app, the CLI or an editor on `settings.toml`,
//! is noticed within a second and pushed; changes from the account's other Macs are pulled through
//! the same save as the app's own. A server that offers push (a self-hosted or local one) nudges
//! over a WebSocket; otherwise this Mac looks every minute, right away on wake, a network change or
//! the app coming to the front, and every few seconds while the Account pane is open.
//!
//! State lives in `DINO_HOME/sync/state.json` (the records, what was last in step, the clock,
//! what's waiting to go), 0600. The last 20 `settings.toml`s a sync replaced are in
//! `sync/snapshots/`.

use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dino_core::ipc::SyncStatus;
use dino_core::settings::Settings;
use dino_sync::record::{PullResponse, PushRequest, PushResponse};
use dino_sync::settings::{Entries, SCHEMA};
use dino_sync::{Clock, Hlc, Nudge, Record, RecordId, Store};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cloud;

/// How many replaced `settings.toml`s to keep.
const SNAPSHOTS: usize = 20;
/// A look at the server while its push socket is up, in case a nudge was missed.
const PULL_EVERY_PUSHED: Duration = Duration::from_secs(10 * 60);
/// A look at the server without push.
const PULL_EVERY: Duration = Duration::from_secs(60);
/// While the Account pane is open.
const PULL_EVERY_FAST: Duration = Duration::from_secs(3);
/// The Account pane asked for the status this recently: it's open.
const PANE_OPEN: Duration = Duration::from_secs(5);

#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)]
struct State {
    server: String,
    phase: String,
    /// The sync protocol the records are in.
    protocol: u32,
    account: String,
    email: Option<String>,
    device: String,
    seq: u64,
    clock: Option<Hlc>,
    /// Every record as last stored here or received.
    records: Vec<Record>,
    /// The settings as they were when this Mac and the account were last in step.
    baseline: Vec<(RecordId, Value)>,
    /// Written here, not yet taken by the server.
    pending: Vec<RecordId>,
    /// This Mac's and the account's settings, while the person chooses between them.
    conflict_local: Vec<(RecordId, Value)>,
    last_sync: Option<u64>,
    message: Option<String>,
    /// Repo variables for remotes not checked out here yet.
    pending_repos: Vec<(String, String, String)>,
}

/// What only this run of dinod knows.
#[derive(Default)]
struct Live {
    device_code: Option<(String, String)>,
    /// The address a sign-in link went to, while it waits to be opened.
    email_sent_to: Option<String>,
    signing_in: bool,
}

fn dir() -> PathBuf {
    dino_core::config_dir().join("sync")
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn write_private(path: &PathBuf, bytes: &[u8]) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir())?;
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    f.write_all(bytes)?;
    drop(f);
    std::fs::rename(tmp, path)?;
    Ok(())
}

/// Told when the phase becomes "ready" or "conflict": signed out, the loops wait on it.
static PHASE: Condvar = Condvar::new();

/// Wait until the phase is one of `phases`; its server then.
fn until_phase(phases: &[&str]) -> String {
    let mut s = state().lock().unwrap();
    while !phases.contains(&s.phase.as_str()) {
        s = PHASE.wait(s).unwrap();
    }
    s.server.clone()
}

fn state() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(|| {
        let mut s: State = std::fs::read(dir().join("state.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        // From the end-to-end encrypted sync before this one (or stopped midway through signing
        // in): still signed in, so the account is set up again from what the server holds now.
        let signed_in = !matches!(s.phase.as_str(), "" | "signed_out");
        if signed_in && (s.protocol != dino_sync::record::PROTOCOL || !matches!(s.phase.as_str(), "ready" | "conflict")) {
            s = State { server: s.server, phase: if cloud::signed_in() { "upgrade".into() } else { "signed_out".into() }, ..State::default() };
        }
        for old in ["account-key", "device-key"] {
            let _ = std::fs::remove_file(dir().join(old));
        }
        Mutex::new(s)
    })
}

fn live() -> &'static Mutex<Live> {
    static L: OnceLock<Mutex<Live>> = OnceLock::new();
    L.get_or_init(Mutex::default)
}

/// Something changed that the loop should act on now, not at its next second.
static KICK: AtomicBool = AtomicBool::new(false);
/// A nudge said there's more on the server.
static PULL: AtomicBool = AtomicBool::new(false);
/// The push socket is connected: nudges arrive, so the looks can be slow.
static SOCKET_UP: AtomicBool = AtomicBool::new(false);
/// Whether the server offers push: 0 not known yet, 1 no, 2 yes.
static PUSH: AtomicU8 = AtomicU8::new(0);
/// When the Account pane (or the CLI) last asked for the status, in ms since the epoch.
static LAST_STATUS: AtomicU64 = AtomicU64::new(0);

/// How long to wait between looks, by what's going on.
fn every(socket_up: bool, fast: bool) -> Duration {
    match (socket_up, fast) {
        (true, _) => PULL_EVERY_PUSHED,
        (false, true) => PULL_EVERY_FAST,
        (false, false) => PULL_EVERY,
    }
}

/// Someone is looking at the Account pane.
fn fast() -> bool {
    now_ms().saturating_sub(LAST_STATUS.load(Ordering::Relaxed)) < PANE_OPEN.as_millis() as u64
}

fn save_state(s: &State) {
    if let Ok(b) = serde_json::to_vec(s) {
        let _ = write_private(&dir().join("state.json"), &b);
    }
}

/// Checkouts dinod knows of (sessions' repos, dino's worktrees' main checkouts), to find where a
/// synced repo's variables go on this Mac.
pub type Places = Box<dyn Fn() -> Vec<String> + Send + Sync>;
/// Run after a sync changed this Mac's settings.
pub type Applied = Box<dyn Fn() + Send + Sync>;

fn hooks() -> &'static Mutex<(Option<Places>, Option<Applied>)> {
    static P: OnceLock<Mutex<(Option<Places>, Option<Applied>)>> = OnceLock::new();
    P.get_or_init(|| Mutex::new((None, None)))
}

fn remote_of(path: &str) -> Option<String> {
    static CACHE: OnceLock<Mutex<BTreeMap<String, Option<String>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Mutex::default);
    if let Some(r) = cache.lock().unwrap().get(path) {
        return r.clone();
    }
    let r = std::process::Command::new("git")
        .args(["-C", path, "remote", "get-url", "origin"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|r| !r.is_empty())
        .map(|r| normalize_remote(&r));
    cache.lock().unwrap().insert(path.to_string(), r.clone());
    r
}

/// The same repository however it was cloned: `git@github.com:a/b.git` and
/// `https://github.com/a/b` both become `github.com/a/b`.
fn normalize_remote(r: &str) -> String {
    let r = r.trim().trim_end_matches('/').trim_end_matches(".git");
    let r = r.split_once("://").map_or(r, |(_, rest)| rest);
    let r = r.rsplit_once('@').map_or(r, |(_, rest)| rest);
    // scp-like `host:path`, not a port.
    match r.split_once(':') {
        Some((host, path)) if !path.starts_with(|c: char| c.is_ascii_digit()) => format!("{host}/{path}"),
        _ => r.to_string(),
    }
}

fn path_of(remote: &str) -> Option<String> {
    let candidates = hooks().lock().unwrap().0.as_ref().map(|f| f()).unwrap_or_default();
    let mut seen = Settings::load_user().repos.into_keys().collect::<Vec<_>>();
    seen.extend(candidates);
    seen.into_iter().find(|p| remote_of(p).as_deref() == Some(remote))
}

/// This Mac's settings as sync entries.
fn local_entries() -> Entries {
    dino_sync::settings::flatten(&Settings::load_user(), &remote_of)
}

/// Starts the loop and the nudge socket.
pub fn start(repos: Places, applied: Applied) {
    *hooks().lock().unwrap() = (Some(repos), Some(applied));
    std::thread::Builder::new().name("sync".into()).spawn(run).expect("sync thread");
    std::thread::Builder::new().name("sync-ws".into()).spawn(nudges).expect("sync socket thread");
    if state().lock().unwrap().phase == "upgrade" {
        std::thread::Builder::new().name("sync-upgrade".into()).spawn(upgrade).expect("sync upgrade thread");
    }
}

/// Set the account up again after the sync protocol changed, until it works or the server says
/// this Mac is signed out.
fn upgrade() {
    let server = state().lock().unwrap().server.clone();
    let n = next_attempt();
    loop {
        match set_up_account(&server) {
            Ok(()) => return,
            Err(e) if e.is::<cloud::SignedOut>() || n != ATTEMPT.load(Ordering::SeqCst) => {
                if n == ATTEMPT.load(Ordering::SeqCst) {
                    signed_out_elsewhere(&e.to_string());
                }
                return;
            }
            Err(e) => {
                let mut s = state().lock().unwrap();
                s.phase = "upgrade".into();
                s.message = Some(format!("Can't reach the account server: {e}"));
                drop(s);
                std::thread::sleep(Duration::from_secs(30));
            }
        }
    }
}

/// A change was made here: look now.
pub fn kick() {
    KICK.store(true, Ordering::Relaxed);
}

fn run() {
    let mut last_pull = None::<Instant>;
    let mut stamp = None;
    let mut last_wall = now_ms();
    let mut failures: u32 = 0;
    let network = NetworkChanges::new();
    PULL.store(true, Ordering::Relaxed);
    loop {
        until_phase(&["ready"]);
        std::thread::sleep(Duration::from_millis(250));
        let wall = now_ms();
        // Woke from sleep, or the network changed: the other Macs may have moved on.
        if wall.saturating_sub(last_wall) > 30_000 || network.changed() {
            PULL.store(true, Ordering::Relaxed);
            failures = 0;
        }
        last_wall = wall;
        if state().lock().unwrap().phase != "ready" {
            continue;
        }
        let socket_up = SOCKET_UP.load(Ordering::Relaxed);
        let fast = !socket_up && fast();
        let files = files_stamp();
        let changed = KICK.swap(false, Ordering::Relaxed) || stamp.as_ref() != Some(&files);
        // After failures, wait longer before trying again (up to the normal look), not every pass.
        let backoff = Duration::from_secs(2u64.saturating_pow(failures.min(5))).min(PULL_EVERY);
        let retry_ok = failures == 0 || last_pull.is_none_or(|t: Instant| t.elapsed() >= backoff);
        let due = (PULL.load(Ordering::Relaxed) && retry_ok && PULL.swap(false, Ordering::Relaxed)) || last_pull.is_none_or(|t: Instant| t.elapsed() >= every(socket_up, fast));
        if !changed && !due && !has_pending() {
            continue;
        }
        if changed {
            record_local();
        }
        if has_pending() {
            if let Err(e) = push() {
                note_error(e);
                std::thread::sleep(Duration::from_secs(2));
            }
        }
        if due {
            last_pull = Some(Instant::now());
            match pull() {
                Ok(()) => failures = 0,
                Err(e) => {
                    note_error(e);
                    failures += 1;
                    PULL.store(true, Ordering::Relaxed);
                }
            }
        }
        stamp = Some(files_stamp());
    }
}

fn has_pending() -> bool {
    !state().lock().unwrap().pending.is_empty()
}

/// When `settings.toml` last changed.
fn files_stamp() -> Option<SystemTime> {
    Settings::path().metadata().and_then(|m| m.modified()).ok()
}

fn note_error(e: anyhow::Error) {
    if e.is::<cloud::SignedOut>() {
        signed_out_elsewhere(&e.to_string());
        return;
    }
    state().lock().unwrap().message = Some(format!("Can't reach the account server: {e}"));
}

/// The server no longer takes this Mac: its sign-in ended elsewhere (or the account was deleted).
fn signed_out_elsewhere(why: &str) {
    cloud::forget_tokens();
    let mut s = state().lock().unwrap();
    let server = s.server.clone();
    *s = State { server, phase: "signed_out".into(), message: Some(format!("This Mac was signed out of your dino account ({why}). Its settings haven't changed.")), ..State::default() };
    save_state(&s);
}

/// Record what changed here since the last time this Mac and the account were in step.
fn record_local() {
    record_local_in(&mut state().lock().unwrap());
}

fn record_local_in(s: &mut State) {
    // A settings.toml that doesn't parse reads as earlier settings or the defaults: no change made here.
    if Settings::error().is_some() {
        return;
    }
    let current = local_entries();
    let baseline: Entries = s.baseline.iter().cloned().collect();
    let changes = dino_sync::settings::diff(&baseline, &current);
    if changes.is_empty() {
        return;
    }
    let mut clock = Clock::resume(s.device.clone(), s.clock.clone());
    let mut store = store_of(s);
    for (id, value) in changes {
        let hlc = clock.now(now_ms());
        let record = Record { id: id.clone(), hlc, schema: SCHEMA, deleted: value.is_none(), value: value.unwrap_or(Value::Null), seq: None, extra: Default::default() };
        if store.write_local(record, SCHEMA).is_ok() && !s.pending.contains(&id) {
            s.pending.push(id);
        }
    }
    s.clock = clock.last().cloned();
    s.records = store.iter().cloned().collect();
    s.baseline = current.into_iter().collect();
    save_state(s);
}

/// The records this Mac holds.
fn store_of(s: &State) -> Store {
    let mut store = Store::new();
    store.apply_all(s.records.iter().cloned());
    store
}

/// Takes the records from the server that are later than what's here, and keeps the clock ahead
/// of them. Records stamped too far in the future are left out (and said so).
fn take(s: &mut State, store: &mut Store, records: Vec<Record>) -> Vec<RecordId> {
    let mut clock = Clock::resume(s.device.clone(), s.clock.clone());
    let now = now_ms();
    let (changed, refused) = store.apply_all_remote(records, now);
    for id in &changed {
        if let Some(r) = store.get(id) {
            let _ = clock.observe(&r.hlc, now);
        }
    }
    if !refused.is_empty() {
        s.message = Some(format!("{} setting(s) from another Mac were stamped too far in the future and were ignored: check that Mac's clock.", refused.len()));
    }
    s.clock = clock.last().cloned();
    changed
}

/// Send what's waiting, a batch at a time.
fn push() -> anyhow::Result<()> {
    let (server, device, batch) = {
        let s = state().lock().unwrap();
        let store = store_of(&s);
        let batch: Vec<Record> = s.pending.iter().filter_map(|id| store.get(id).cloned()).take(dino_sync::record::MAX_BATCH).collect();
        (s.server.clone(), s.device.clone(), batch)
    };
    if batch.is_empty() {
        state().lock().unwrap().pending.clear();
        return Ok(());
    }
    let req = PushRequest { device_id: device, records: batch, extra: Default::default() };
    let v = cloud::send(&server, reqwest::Method::POST, "/v1/sync", &serde_json::to_value(&req)?)?;
    let resp: PushResponse = serde_json::from_value(v)?;
    let mut s = state().lock().unwrap();
    // Taken, or beaten by a newer one the next pull brings: either way no longer waiting.
    let done: Vec<RecordId> = resp.accepted.iter().chain(resp.superseded.iter()).cloned().collect();
    s.pending.retain(|id| !done.contains(id) && !resp.rejected.iter().any(|r| &r.id == id));
    s.message = resp.rejected.first().map(|r| format!("A setting wasn't synced: {}", r.error));
    if !resp.superseded.is_empty() {
        PULL.store(true, Ordering::Relaxed);
    }
    s.last_sync = Some(now_ms() / 1000);
    save_state(&s);
    Ok(())
}

/// Everything the server took since the last pull, applied here.
fn pull() -> anyhow::Result<()> {
    loop {
        let (server, since) = {
            let s = state().lock().unwrap();
            (s.server.clone(), s.seq)
        };
        let page = get_page(&server, since)?;
        let more = page.more;
        apply_remote(page.records)?;
        let mut s = state().lock().unwrap();
        s.seq = s.seq.max(page.seq);
        s.last_sync = Some(now_ms() / 1000);
        if s.message.as_deref().is_some_and(|m| m.starts_with("Can't reach")) {
            s.message = None;
        }
        save_state(&s);
        if !more {
            return Ok(());
        }
    }
}

fn get_page(server: &str, since: u64) -> anyhow::Result<PullResponse> {
    let v = cloud::get(server, &format!("/v1/sync?since={since}"))?.ok_or_else(|| anyhow::anyhow!("no sync on this server"))?;
    let page: PullResponse = serde_json::from_value(v)?;
    dino_sync::record::check_pull(&page)?;
    Ok(page)
}

/// Every live, readable record as a setting's value.
fn entries_of(store: &Store) -> Entries {
    store.iter().filter(|r| r.schema <= SCHEMA && !r.deleted).map(|r| (r.id.clone(), r.value.clone())).collect()
}

fn apply_remote(records: Vec<Record>) -> anyhow::Result<()> {
    let mut s = state().lock().unwrap();
    // A change made here since the loop last looked (an editor saving `settings.toml` just now)
    // becomes a record first, so it's merged by its stamp rather than written over.
    record_local_in(&mut s);
    let mut store = store_of(&s);
    let changed = take(&mut s, &mut store, records);
    s.records = store.iter().cloned().collect();
    if changed.is_empty() {
        return Ok(());
    }
    // A local write that lost to a newer one elsewhere isn't waiting any more.
    s.pending.retain(|id| !changed.contains(id));
    let synced = entries_of(&store);
    apply_entries(&mut s, &synced)?;
    s.baseline = synced.into_iter().collect();
    save_state(&s);
    Ok(())
}

/// Make this Mac's settings what `entries` say.
fn apply_entries(s: &mut State, entries: &Entries) -> anyhow::Result<()> {
    let local = Settings::load_user();
    let applied = dino_sync::settings::unflatten(&local, entries, &path_of, &remote_of);
    if applied.settings != local {
        snapshot();
        applied.settings.save()?;
    }
    s.pending_repos = applied.pending;
    if let Some(f) = hooks().lock().unwrap().1.as_ref() {
        f();
    }
    Ok(())
}

/// Keep the current `settings.toml` before a sync replaces it.
fn snapshot() {
    let Ok(text) = std::fs::read(Settings::path()) else { return };
    let d = dir().join("snapshots");
    let _ = std::fs::create_dir_all(&d);
    let _ = write_private(&d.join(format!("{}.toml", now_ms())), &text);
    let mut all = snapshots();
    while all.len() > SNAPSHOTS {
        if let Some(old) = all.pop() {
            let _ = std::fs::remove_file(old);
        }
    }
}

/// Newest first.
fn snapshots() -> Vec<PathBuf> {
    let mut all: Vec<PathBuf> = std::fs::read_dir(dir().join("snapshots")).map(|r| r.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "toml")).collect()).unwrap_or_default();
    all.sort();
    all.reverse();
    all
}

/// The WebSocket that says when the account moved on, when the server offers one. Reconnects,
/// slower each time it fails; without it, the sync loop looks on its own.
fn nudges() {
    let mut wait = Duration::from_secs(1);
    let mut asked: Option<(String, Instant)> = None;
    loop {
        if !matches!(state().lock().unwrap().phase.as_str(), "ready" | "conflict") {
            PUSH.store(0, Ordering::Relaxed);
            asked = None;
        }
        let server = until_phase(&["ready", "conflict"]);
        // Whether this server pushes at all: asked once, and again now and then (it may change).
        let stale = asked.as_ref().is_none_or(|(s, at)| *s != server || at.elapsed() >= Duration::from_secs(10 * 60));
        if stale {
            match cloud::meta(&server) {
                Ok(m) => {
                    PUSH.store(if m.push { 2 } else { 1 }, Ordering::Relaxed);
                    asked = Some((server.clone(), Instant::now()));
                }
                Err(_) => {
                    std::thread::sleep(wait);
                    wait = (wait * 2).min(Duration::from_secs(60));
                    continue;
                }
            }
        }
        if PUSH.load(Ordering::Relaxed) != 2 {
            std::thread::sleep(Duration::from_secs(5));
            continue;
        }
        let r = listen(&server);
        SOCKET_UP.store(false, Ordering::Relaxed);
        match r {
            Ok(()) => wait = Duration::from_secs(1),
            Err(e) => {
                if e.is::<cloud::SignedOut>() {
                    signed_out_elsewhere(&e.to_string());
                }
                std::thread::sleep(wait);
                wait = (wait * 2).min(Duration::from_secs(60));
            }
        }
    }
}

/// macOS posts `com.apple.system.config.network_change` when the network changes (Wi-Fi joined,
/// VPN up, cable in): a cheap check of a shared counter, no thread or callback.
struct NetworkChanges {
    #[cfg(target_os = "macos")]
    token: Option<i32>,
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn notify_register_check(name: *const std::ffi::c_char, out_token: *mut i32) -> u32;
    fn notify_check(token: i32, check: *mut i32) -> u32;
}

impl NetworkChanges {
    fn new() -> Self {
        #[cfg(target_os = "macos")]
        {
            let mut token = 0;
            let ok = unsafe { notify_register_check(c"com.apple.system.config.network_change".as_ptr(), &mut token) } == 0;
            let n = NetworkChanges { token: ok.then_some(token) };
            // The first check always reports a change.
            let _ = n.changed();
            n
        }
        #[cfg(not(target_os = "macos"))]
        NetworkChanges {}
    }

    fn changed(&self) -> bool {
        #[cfg(target_os = "macos")]
        if let Some(token) = self.token {
            let mut check = 0;
            return unsafe { notify_check(token, &mut check) } == 0 && check != 0;
        }
        false
    }
}

fn listen(server: &str) -> anyhow::Result<()> {
    use tungstenite::client::IntoClientRequest;
    let at = cloud::access(server)?;
    let mut req = cloud::ws_url(server).into_client_request()?;
    req.headers_mut().insert("authorization", format!("Bearer {at}").parse()?);
    let (mut ws, _) = tungstenite::connect(req)?;
    if let tungstenite::stream::MaybeTlsStream::Plain(s) = ws.get_ref() {
        s.set_read_timeout(Some(Duration::from_secs(30)))?;
    }
    SOCKET_UP.store(true, Ordering::Relaxed);
    // Something may have happened while the socket was down.
    PULL.store(true, Ordering::Relaxed);
    loop {
        if state().lock().unwrap().server != server {
            return Ok(());
        }
        match ws.read() {
            Ok(tungstenite::Message::Text(t)) => match serde_json::from_str::<Nudge>(&t) {
                Ok(Nudge::Advanced { seq }) if seq > state().lock().unwrap().seq => PULL.store(true, Ordering::Relaxed),
                Ok(Nudge::Reset) => {
                    state().lock().unwrap().seq = 0;
                    PULL.store(true, Ordering::Relaxed);
                }
                _ => {}
            },
            Ok(tungstenite::Message::Close(_)) => return Ok(()),
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                ws.send(tungstenite::Message::Ping(Default::default()))?;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

// ── What the app and the CLI ask for ──

/// The Account pane (or `dino sync status`) is looking: look at the server more often for a bit.
pub fn looking() {
    LAST_STATUS.store(now_ms(), Ordering::Relaxed);
}

pub fn status() -> SyncStatus {
    let s = state().lock().unwrap();
    let l = live().lock().unwrap();
    let phase = if l.signing_in || matches!(s.phase.as_str(), "joining" | "upgrade") {
        "signing_in".to_string()
    } else if s.phase.is_empty() {
        "signed_out".into()
    } else {
        s.phase.clone()
    };
    let server = if s.server.is_empty() { cloud::default_server() } else { s.server.clone() };
    let local: Entries = s.conflict_local.iter().cloned().collect();
    // Every setting the account holds, a newer dino's too, as the account page counts them.
    let live: Entries = s.records.iter().filter(|r| !r.is_tombstone()).map(|r| (r.id.clone(), r.value.clone())).collect();
    let conflict = (s.phase == "conflict").then(|| {
        let cloud_entries: Entries = s.baseline.iter().cloned().collect();
        let only_here = local.keys().filter(|k| !cloud_entries.contains_key(*k)).count();
        let only_cloud = cloud_entries.keys().filter(|k| !local.contains_key(*k)).count();
        let differ = local.iter().filter(|(k, v)| cloud_entries.get(*k).is_some_and(|c| c != *v)).count();
        (only_here, only_cloud, differ)
    });
    SyncStatus {
        account_url: matches!(phase.as_str(), "ready" | "conflict").then(|| format!("{server}/account")),
        phase,
        server,
        email: s.email.clone(),
        last_sync: s.last_sync,
        pending: s.pending.len(),
        synced: live.len(),
        synced_what: dino_sync::settings::describe(&live),
        email_sent_to: l.email_sent_to.clone(),
        device_code: l.device_code.as_ref().map(|d| d.0.clone()),
        device_url: l.device_code.as_ref().map(|d| d.1.clone()),
        conflict,
        snapshots: snapshots().len(),
        message: s.message.clone(),
    }
}

/// Which sign-in is the current one: an earlier one that finishes or gives up later is ignored.
static ATTEMPT: AtomicU64 = AtomicU64::new(0);

fn next_attempt() -> u64 {
    ATTEMPT.fetch_add(1, Ordering::SeqCst) + 1
}

/// `dino login`: sign in with GitHub. The page to open; the rest happens once the browser comes
/// back.
pub fn login(server: Option<String>) -> anyhow::Result<String> {
    let server = prepare_login(server)?;
    let (s2, n) = (server.clone(), next_attempt());
    cloud::login(server, Some("github"), move |r| after_login(&s2, n, r))
}

/// `dino login --email`: a sign-in link to `email`; this Mac is signed in once it's opened.
pub fn login_email(email: &str) -> anyhow::Result<()> {
    let email = email.trim().to_string();
    anyhow::ensure!(email.contains('@') && email.len() <= 254, "that doesn't look like an email address");
    let server = prepare_login(None)?;
    let (s2, n) = (server.clone(), next_attempt());
    if let Err(e) = cloud::login_email(server, &email, move |r| after_login(&s2, n, r)) {
        live().lock().unwrap().signing_in = false;
        return Err(e);
    }
    live().lock().unwrap().email_sent_to = Some(email);
    Ok(())
}

pub fn login_device(server: Option<String>) -> anyhow::Result<()> {
    let server = prepare_login(server)?;
    let (s2, n) = (server.clone(), next_attempt());
    let code = cloud::login_device(server, move |r| after_login(&s2, n, r))?;
    live().lock().unwrap().device_code = Some(code);
    Ok(())
}

/// Stop waiting for a sign-in (the link was never opened, the person wants another way).
pub fn cancel_login() {
    next_attempt();
    let mut l = live().lock().unwrap();
    l.signing_in = false;
    l.email_sent_to = None;
    l.device_code = None;
}

fn prepare_login(server: Option<String>) -> anyhow::Result<String> {
    // Signed out, a server remembered from an earlier sign-in (a local test server, say) doesn't
    // stick: a fresh sign-in goes to the default unless one is named.
    let server = server.filter(|s| !s.is_empty()).unwrap_or_else(cloud::default_server);
    let server = server.trim_end_matches('/').to_string();
    anyhow::ensure!(server.starts_with("https://") || server.starts_with("http://127.0.0.1") || server.starts_with("http://localhost"), "the account server must be https");
    anyhow::ensure!(!cloud::signed_in() || state().lock().unwrap().phase == "signed_out", "already signed in; run `dino logout` first");
    {
        let mut l = live().lock().unwrap();
        l.signing_in = true;
        l.email_sent_to = None;
        l.device_code = None;
    }
    let mut s = state().lock().unwrap();
    s.server = server.clone();
    s.message = None;
    save_state(&s);
    Ok(server)
}

fn after_login(server: &str, attempt: u64, r: anyhow::Result<()>) {
    // A sign-in that was started again since (the page left open, then a new try) is over: it
    // mustn't sign out the one that went through.
    if attempt != ATTEMPT.load(Ordering::SeqCst) {
        return;
    }
    let result = r.and_then(|()| set_up_account(server));
    {
        let mut l = live().lock().unwrap();
        l.signing_in = false;
        l.device_code = None;
        l.email_sent_to = None;
    }
    if let Err(e) = result {
        let mut s = state().lock().unwrap();
        s.phase = "signed_out".into();
        s.message = Some(format!("Signing in didn't finish: {e}"));
        save_state(&s);
        cloud::forget_tokens();
    }
}

/// Signed in: who we are, then everything the account holds, taken or merged with this Mac's.
fn set_up_account(server: &str) -> anyhow::Result<()> {
    let me = cloud::get(server, "/v1/me")?.ok_or_else(|| anyhow::anyhow!("no account"))?;
    let account = me["account_id"].as_str().ok_or_else(|| anyhow::anyhow!("no account id"))?.to_string();
    let device = me["device_id"].as_str().ok_or_else(|| anyhow::anyhow!("no device id"))?.to_string();
    {
        let mut s = state().lock().unwrap();
        *s = State { server: server.into(), account, device, email: me["email"].as_str().map(String::from), phase: "joining".into(), protocol: dino_sync::record::PROTOCOL, ..State::default() };
        save_state(&s);
    }
    let mut records = vec![];
    let mut since = 0;
    loop {
        let page = get_page(server, since)?;
        since = page.seq;
        records.extend(page.records);
        if !page.more {
            break;
        }
    }
    let mut s = state().lock().unwrap();
    s.seq = since;
    let mut store = Store::new();
    take(&mut s, &mut store, records);
    s.records = store.iter().cloned().collect();
    let cloud_entries = entries_of(&store);
    let local = local_entries();
    s.baseline = cloud_entries.clone().into_iter().collect();
    s.last_sync = Some(now_ms() / 1000);
    // No setting set differently on the two sides: keep both without asking (this Mac's own
    // ones go to the account as changes made here).
    if local.iter().all(|(k, v)| cloud_entries.get(k).is_none_or(|c| c == v)) {
        let mut both = cloud_entries.clone();
        both.extend(local);
        apply_entries(&mut s, &both)?;
        s.phase = "ready".into();
    } else {
        s.conflict_local = local.into_iter().collect();
        s.phase = "conflict".into();
    }
    PHASE.notify_all();
    save_state(&s);
    drop(s);
    kick();
    Ok(())
}

/// The first sign-in on a Mac with settings of its own: `cloud` takes the account's, `local`
/// makes the account this Mac's, `merge` keeps both, the newer one where they differ.
pub fn resolve(choice: &str) -> anyhow::Result<()> {
    let mut s = state().lock().unwrap();
    anyhow::ensure!(s.phase == "conflict", "nothing to choose");
    let cloud_entries: Entries = s.baseline.iter().cloned().collect();
    let local: Entries = s.conflict_local.iter().cloned().collect();
    match choice {
        "cloud" => apply_entries(&mut s, &cloud_entries)?,
        // The loop sends this Mac's settings as changes against the account's.
        "local" => {}
        "merge" => {
            let store = store_of(&s);
            let changed_here = Settings::path().metadata().and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as u64);
            let mut merged = cloud_entries.clone();
            for (id, v) in &local {
                let there = store.get(id).map_or(0, |r| r.hlc.wall_ms);
                if !merged.contains_key(id) || changed_here > there {
                    merged.insert(id.clone(), v.clone());
                }
            }
            apply_entries(&mut s, &merged)?;
        }
        _ => anyhow::bail!("choose cloud, local or merge"),
    }
    s.conflict_local.clear();
    s.phase = "ready".into();
    PHASE.notify_all();
    save_state(&s);
    drop(s);
    kick();
    Ok(())
}

pub fn now() {
    PULL.store(true, Ordering::Relaxed);
    kick();
}

/// Put back the `settings.toml` a sync last replaced; it syncs as a change made here.
pub fn undo() -> anyhow::Result<()> {
    let newest = snapshots().into_iter().next().ok_or_else(|| anyhow::anyhow!("no earlier settings kept"))?;
    let text = std::fs::read(&newest)?;
    snapshot();
    write_private(&Settings::path(), &text)?;
    let _ = std::fs::remove_file(newest);
    kick();
    Ok(())
}

pub fn logout() {
    let server = state().lock().unwrap().server.clone();
    if !server.is_empty() {
        cloud::logout(&server);
    }
    cloud::forget_tokens();
    next_attempt();
    let mut s = state().lock().unwrap();
    *s = State { server, phase: "signed_out".into(), ..State::default() };
    save_state(&s);
    *live().lock().unwrap() = Live::default();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remotes_match_however_they_were_cloned() {
        for r in ["git@github.com:acme/api.git", "https://github.com/acme/api", "https://token@github.com/acme/api.git/", "ssh://git@github.com/acme/api.git"] {
            assert_eq!(normalize_remote(r), "github.com/acme/api", "{r}");
        }
        assert_eq!(normalize_remote("https://git.example.com:8443/team/x.git"), "git.example.com:8443/team/x");
    }
}
