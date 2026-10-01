//! Settings sync, as this Mac's side of it: dino's settings (and, when the person wants, the keys
//! in its key store) become records sealed with the account's key before they leave, so the
//! server holds nothing it can read. Every change made here, by the app, the CLI or an editor on
//! `settings.toml`, is noticed within a second and pushed; changes from the account's other Macs
//! arrive on a nudge (or a look every ten minutes) and go through the same save as the app's own.
//!
//! State lives in `DINO_HOME/sync/`: `state.json` (the records, what was last in step, the clock,
//! what's waiting to go) and `account-key`, both 0600. The last 20 `settings.toml`s a sync
//! replaced are in `sync/snapshots/`.

use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dino_core::ipc::SyncStatus;
use dino_core::settings::Settings;
use dino_sync::record::{PullResponse, PushRequest, PushResponse};
use dino_sync::settings::{Entries, SCHEMA};
use dino_sync::{AccountKey, Clock, Hlc, Nudge, Record, RecordId, RecoveryKey, Store};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cloud;

/// How many replaced `settings.toml`s to keep.
const SNAPSHOTS: usize = 20;
/// A look at the server when nothing nudged.
const PULL_EVERY: Duration = Duration::from_secs(10 * 60);

#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)]
struct State {
    server: String,
    phase: String,
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
    key_sync: bool,
    /// The wrapped account key, while waiting for the recovery key.
    wrapped: Option<String>,
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
    /// Shown once, until the person has it.
    recovery: Option<String>,
    device_code: Option<(String, String)>,
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

fn state() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(|| {
        let s: State = std::fs::read(dir().join("state.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
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

fn save_state(s: &State) {
    if let Ok(b) = serde_json::to_vec(s) {
        let _ = write_private(&dir().join("state.json"), &b);
    }
}

fn account_key() -> Option<AccountKey> {
    let text = std::fs::read_to_string(dir().join("account-key")).ok()?;
    let (id, b64) = text.trim().split_once(':')?;
    use base64::Engine;
    let bytes: [u8; 32] = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(b64).ok()?.try_into().ok()?;
    Some(AccountKey::from_bytes(id, bytes))
}

fn save_account_key(k: &AccountKey) -> anyhow::Result<()> {
    use base64::Engine;
    write_private(&dir().join("account-key"), format!("{}:{}", k.id, base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(k.bytes())).as_bytes())
}

fn forget_account_key() {
    let _ = std::fs::remove_file(dir().join("account-key"));
}

/// Where a setting lives, as the server sees it: its collection, and its name keyed with the
/// account's key, so SSH hosts, key names and git remotes aren't readable there either. The real
/// name travels inside the sealed value.
fn blind(key: &AccountKey, id: &RecordId) -> RecordId {
    use sha2::{Digest, Sha256};
    // HMAC-SHA256 (RFC 2104) under a key derived for this use alone.
    let k: [u8; 32] = Sha256::new().chain_update(b"dino-sync record id v1").chain_update(key.bytes()).finalize().into();
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..32 {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let inner = Sha256::new().chain_update(ipad).chain_update(id.collection.as_bytes()).chain_update([0]).chain_update(id.key.as_bytes()).finalize();
    let mac = Sha256::new().chain_update(opad).chain_update(inner).finalize();
    RecordId::new(id.collection.clone(), mac[..16].iter().map(|b| format!("{b:02x}")).collect::<String>())
}

/// What's sealed in a record: the setting's real name and its value.
#[derive(Serialize, Deserialize)]
struct Sealed {
    id: String,
    v: Value,
}

/// Checkouts dinod knows of (sessions' repos, dino's worktrees' main checkouts), to find where a
/// synced repo's variables go on this Mac.
pub type Places = Box<dyn Fn() -> Vec<String> + Send + Sync>;
/// Run after a sync changed this Mac's settings or keys.
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

/// The key store as a key would sync: this Mac's sign-ins left out.
fn syncable_keys() -> BTreeMap<String, String> {
    dino_core::settings::stored().into_iter().filter(|(k, v)| !v.is_empty() && !cloud::PRIVATE_PREFIXES.iter().any(|p| k.starts_with(p))).collect()
}

/// This Mac's settings as sync entries.
fn local_entries(key_sync: bool) -> Entries {
    let keys = key_sync.then(syncable_keys);
    dino_sync::settings::flatten(&Settings::load_user(), &remote_of, keys.as_ref())
}

fn without_keys(e: Entries, key_sync: bool) -> Entries {
    if key_sync { e } else { e.into_iter().filter(|(id, _)| id.collection != "keys").collect() }
}

/// Starts the loop and the nudge socket.
pub fn start(repos: Places, applied: Applied) {
    *hooks().lock().unwrap() = (Some(repos), Some(applied));
    std::thread::Builder::new().name("sync".into()).spawn(run).expect("sync thread");
    std::thread::Builder::new().name("sync-ws".into()).spawn(nudges).expect("sync socket thread");
}

/// A change was made here: look now.
pub fn kick() {
    KICK.store(true, Ordering::Relaxed);
}

fn run() {
    let mut last_pull = None::<Instant>;
    let mut stamp = None;
    let mut last_wall = now_ms();
    PULL.store(true, Ordering::Relaxed);
    loop {
        std::thread::sleep(Duration::from_millis(250));
        let wall = now_ms();
        // Woke from sleep: the other Macs may have moved on.
        if wall.saturating_sub(last_wall) > 30_000 {
            PULL.store(true, Ordering::Relaxed);
        }
        last_wall = wall;
        let ready = state().lock().unwrap().phase == "ready";
        if !ready {
            continue;
        }
        let files = files_stamp();
        let changed = KICK.swap(false, Ordering::Relaxed) || stamp.as_ref() != Some(&files);
        let due = PULL.swap(false, Ordering::Relaxed) || last_pull.is_none_or(|t: Instant| t.elapsed() >= PULL_EVERY);
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
            match pull() {
                Ok(()) => last_pull = Some(Instant::now()),
                Err(e) => {
                    note_error(e);
                    PULL.store(true, Ordering::Relaxed);
                    std::thread::sleep(Duration::from_secs(2));
                }
            }
        }
        stamp = Some(files_stamp());
    }
}

fn has_pending() -> bool {
    !state().lock().unwrap().pending.is_empty()
}

/// When `settings.toml` and the key store last changed.
fn files_stamp() -> (Option<SystemTime>, Option<SystemTime>) {
    let m = |p: PathBuf| p.metadata().and_then(|m| m.modified()).ok();
    (m(Settings::path()), m(dino_core::keys_file()))
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
    forget_account_key();
    let mut s = state().lock().unwrap();
    let server = s.server.clone();
    let key_sync = s.key_sync;
    *s = State { server, key_sync, phase: "signed_out".into(), message: Some(format!("This Mac was signed out of your dino account ({why}). Your settings here are as they were.")), ..State::default() };
    save_state(&s);
}

/// Seal what changed here since the last time this Mac and the account were in step.
fn record_local() {
    let Some(key) = account_key() else { return };
    let mut s = state().lock().unwrap();
    let current = local_entries(s.key_sync);
    let baseline: Entries = without_keys(s.baseline.iter().cloned().collect(), s.key_sync);
    let changes = dino_sync::settings::diff(&baseline, &current);
    if changes.is_empty() {
        return;
    }
    let mut clock = Clock::resume(s.device.clone(), s.clock.clone());
    let mut store = store_of(&s);
    for (real, value) in changes {
        let hlc = clock.now(now_ms());
        let id = blind(&key, &real);
        let sealed = match value {
            Some(v) => match serde_json::to_string(&Sealed { id: real.key.clone(), v }).map_err(anyhow::Error::from).and_then(|text| Ok(key.seal(&s.account, &id, SCHEMA, &text)?)) {
                Ok(t) => Some(t),
                Err(_) => continue,
            },
            None => None,
        };
        let record = Record { id: id.clone(), hlc, schema: SCHEMA, value: sealed, seq: None, extra: Default::default() };
        if store.write_local(record, SCHEMA).is_ok() && !s.pending.contains(&id) {
            s.pending.push(id);
        }
    }
    s.clock = clock.last().cloned();
    s.records = store.iter().cloned().collect();
    // Keys that stopped syncing keep their last synced values, unused, until it's turned back on.
    let mut next: Entries = if s.key_sync { Entries::new() } else { s.baseline.iter().filter(|(id, _)| id.collection == "keys").cloned().collect() };
    next.extend(current);
    s.baseline = next.into_iter().collect();
    save_state(&s);
}

fn store_of(s: &State) -> Store {
    let mut store = Store::new();
    store.apply_all(s.records.iter().cloned());
    store
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
    let req = PushRequest { device_id: device, records: batch.clone(), extra: Default::default() };
    let v = cloud::send(&server, reqwest::Method::POST, "/v1/sync", &serde_json::to_value(&req)?)?;
    let resp: PushResponse = serde_json::from_value(v)?;
    let mut s = state().lock().unwrap();
    // Taken, or beaten by a newer one the next pull brings: either way no longer waiting.
    let done: Vec<RecordId> = resp.accepted.iter().chain(resp.superseded.iter()).cloned().collect();
    s.pending.retain(|id| !done.contains(id) && !resp.rejected.iter().any(|r| &r.id == id));
    if let Some(r) = resp.rejected.first() {
        s.message = Some(format!("A setting wasn't synced: {}", r.error));
    } else {
        s.message = None;
    }
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
        let v = cloud::get(&server, &format!("/v1/sync?since={since}"))?.ok_or_else(|| anyhow::anyhow!("no sync on this server"))?;
        let page: PullResponse = serde_json::from_value(v)?;
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

/// Decrypted entries for every readable, live record in `store`.
fn entries_of(store: &Store, account: &str, key: &AccountKey) -> Entries {
    store
        .iter()
        .filter(|r| r.schema <= SCHEMA)
        .filter_map(|r| {
            let v = r.value.as_ref()?;
            let text = key.open(account, &r.id, r.schema, v).ok()?;
            let sealed: Sealed = serde_json::from_str(&text).ok()?;
            let real = RecordId::new(r.id.collection.clone(), sealed.id);
            // Only where it says it lives: a value can't be moved to another setting.
            (blind(key, &real) == r.id).then_some((real, sealed.v))
        })
        .collect()
}

fn apply_remote(records: Vec<Record>) -> anyhow::Result<()> {
    let Some(key) = account_key() else { return Ok(()) };
    let mut s = state().lock().unwrap();
    let mut store = store_of(&s);
    let mut clock = Clock::resume(s.device.clone(), s.clock.clone());
    for r in &records {
        clock.observe(&r.hlc, now_ms());
    }
    let changed = store.apply_all(records);
    s.clock = clock.last().cloned();
    s.records = store.iter().cloned().collect();
    if changed.is_empty() {
        return Ok(());
    }
    // A local write that lost to a newer one elsewhere isn't waiting any more.
    s.pending.retain(|id| !changed.contains(id));
    let synced = without_keys(entries_of(&store, &s.account, &key), s.key_sync);
    apply_entries(&mut s, &synced)?;
    s.baseline = synced.into_iter().collect();
    save_state(&s);
    Ok(())
}

/// Make this Mac's settings (and keys, when they sync) what `entries` say.
fn apply_entries(s: &mut State, entries: &Entries) -> anyhow::Result<()> {
    let local = Settings::load_user();
    let applied = dino_sync::settings::unflatten(&local, entries, &path_of, &remote_of);
    if applied.settings != local {
        snapshot();
        applied.settings.save()?;
    }
    s.pending_repos = applied.pending;
    if s.key_sync {
        let have = syncable_keys();
        for (name, v) in &applied.keys {
            if have.get(name) != Some(v) {
                dino_core::settings::set_key(name, Some(v))?;
            }
        }
        for name in have.keys().filter(|k| !applied.keys.contains_key(*k)) {
            dino_core::settings::set_key(name, None)?;
        }
    }
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

/// The WebSocket that says when the account moved on. Reconnects, slower each time it fails.
fn nudges() {
    let mut wait = Duration::from_secs(1);
    loop {
        let server = {
            let s = state().lock().unwrap();
            (s.phase == "ready" || s.phase == "conflict").then(|| s.server.clone())
        };
        let Some(server) = server else {
            std::thread::sleep(Duration::from_secs(1));
            continue;
        };
        match listen(&server) {
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

fn listen(server: &str) -> anyhow::Result<()> {
    use tungstenite::client::IntoClientRequest;
    let at = cloud::access(server)?;
    let mut req = cloud::ws_url(server).into_client_request()?;
    req.headers_mut().insert("authorization", format!("Bearer {at}").parse()?);
    let (mut ws, _) = tungstenite::connect(req)?;
    if let tungstenite::stream::MaybeTlsStream::Plain(s) = ws.get_ref() {
        s.set_read_timeout(Some(Duration::from_secs(30)))?;
    }
    // Something may have happened while the socket was down.
    PULL.store(true, Ordering::Relaxed);
    loop {
        if state().lock().unwrap().server != server {
            return Ok(());
        }
        match ws.read() {
            Ok(tungstenite::Message::Text(t)) => match serde_json::from_str::<Nudge>(&t) {
                Ok(Nudge::Advanced { seq }) if seq > state().lock().unwrap().seq => PULL.store(true, Ordering::Relaxed),
                Ok(Nudge::Reset) => reset_elsewhere(),
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

/// Another Mac reset sync: the old key reads nothing now. This Mac's settings stay; the new
/// recovery key brings sync back.
fn reset_elsewhere() {
    let mut s = state().lock().unwrap();
    if s.phase != "ready" {
        return;
    }
    forget_account_key();
    s.records.clear();
    s.pending.clear();
    s.baseline.clear();
    s.wrapped = None;
    s.phase = "needs_key".into();
    s.message = Some("Sync was reset on another Mac. Enter the new recovery key to sync this one again.".into());
    save_state(&s);
}

// ── What the app and the CLI ask for ──

pub fn status() -> SyncStatus {
    let s = state().lock().unwrap();
    let l = live().lock().unwrap();
    let phase = if l.signing_in { "signing_in".to_string() } else if s.phase.is_empty() { "signed_out".into() } else { s.phase.clone() };
    let server = if s.server.is_empty() { cloud::default_server() } else { s.server.clone() };
    let local: Entries = s.conflict_local.iter().cloned().collect();
    let conflict = (s.phase == "conflict").then(|| {
        let cloud_entries: Entries = s.baseline.iter().cloned().collect();
        let only_here = local.keys().filter(|k| !cloud_entries.contains_key(*k)).count();
        let only_cloud = cloud_entries.keys().filter(|k| !local.contains_key(*k)).count();
        let differ = local.iter().filter(|(k, v)| cloud_entries.get(*k).is_some_and(|c| c != *v)).count();
        (only_here, only_cloud, differ)
    });
    SyncStatus {
        account_url: (phase != "signed_out").then(|| format!("{server}/account")),
        phase,
        server,
        email: s.email.clone(),
        last_sync: s.last_sync,
        pending: s.pending.len(),
        synced: s.records.iter().filter(|r| !r.is_tombstone()).count(),
        key_sync: s.key_sync,
        recovery_key: l.recovery.clone(),
        device_code: l.device_code.as_ref().map(|d| d.0.clone()),
        device_url: l.device_code.as_ref().map(|d| d.1.clone()),
        conflict,
        snapshots: snapshots().len(),
        message: s.message.clone(),
    }
}

/// `dino login`: the page to open. The rest happens once the browser comes back.
pub fn login(server: Option<String>) -> anyhow::Result<String> {
    let server = prepare_login(server)?;
    let s2 = server.clone();
    cloud::login(server, move |r| after_login(&s2, r))
}

pub fn login_device(server: Option<String>) -> anyhow::Result<()> {
    let server = prepare_login(server)?;
    let s2 = server.clone();
    let code = cloud::login_device(server, move |r| after_login(&s2, r))?;
    live().lock().unwrap().device_code = Some(code);
    Ok(())
}

fn prepare_login(server: Option<String>) -> anyhow::Result<String> {
    let server = server.filter(|s| !s.is_empty()).unwrap_or_else(|| {
        let s = state().lock().unwrap();
        if s.server.is_empty() { cloud::default_server() } else { s.server.clone() }
    });
    let server = server.trim_end_matches('/').to_string();
    anyhow::ensure!(server.starts_with("https://") || server.starts_with("http://127.0.0.1") || server.starts_with("http://localhost"), "the account server must be https");
    anyhow::ensure!(!cloud::signed_in() || state().lock().unwrap().phase == "signed_out", "already signed in; `dino logout` first");
    live().lock().unwrap().signing_in = true;
    let mut s = state().lock().unwrap();
    s.server = server.clone();
    s.message = None;
    save_state(&s);
    Ok(server)
}

fn after_login(server: &str, r: anyhow::Result<()>) {
    let result = r.and_then(|()| set_up_account(server));
    let mut l = live().lock().unwrap();
    l.signing_in = false;
    l.device_code = None;
    drop(l);
    if let Err(e) = result {
        let mut s = state().lock().unwrap();
        s.phase = "signed_out".into();
        s.message = Some(format!("Signing in didn't finish: {e}"));
        save_state(&s);
        cloud::forget_tokens();
    }
}

/// Signed in: who we are, and whether this Mac starts the account's key or needs it.
fn set_up_account(server: &str) -> anyhow::Result<()> {
    let me = cloud::get(server, "/v1/me")?.ok_or_else(|| anyhow::anyhow!("no account"))?;
    let account = me["account_id"].as_str().ok_or_else(|| anyhow::anyhow!("no account id"))?.to_string();
    let device = me["device_id"].as_str().ok_or_else(|| anyhow::anyhow!("no device id"))?.to_string();
    let wrapped = cloud::get(server, "/v1/sync/recovery")?.and_then(|v| v["wrapped"].as_str().map(String::from));
    {
        let mut s = state().lock().unwrap();
        *s = State { server: server.into(), account, device, email: me["email"].as_str().map(String::from), key_sync: true, ..State::default() };
        save_state(&s);
    }
    forget_account_key();
    match wrapped {
        None => start_account(server),
        Some(w) => {
            let mut s = state().lock().unwrap();
            s.wrapped = Some(w);
            s.phase = "needs_key".into();
            s.message = Some("This account already syncs from another Mac. Enter its recovery key to read its settings.".into());
            save_state(&s);
            Ok(())
        }
    }
}

/// The first Mac: make the account's key, keep it wrapped by a new recovery key on the server,
/// and send everything.
fn start_account(server: &str) -> anyhow::Result<()> {
    let key = AccountKey::generate();
    let recovery = RecoveryKey::generate();
    let account = state().lock().unwrap().account.clone();
    let wrapped = recovery.wrap(&account, &key)?;
    cloud::send(server, reqwest::Method::PUT, "/v1/sync/recovery", &json!({"wrapped": wrapped}))?;
    save_account_key(&key)?;
    live().lock().unwrap().recovery = Some(recovery.display());
    let mut s = state().lock().unwrap();
    s.phase = "ready".into();
    s.baseline.clear();
    save_state(&s);
    drop(s);
    PULL.store(true, Ordering::Relaxed);
    kick();
    Ok(())
}

/// `dino sync join <recovery key>`: read the account's key, then take or merge its settings.
pub fn join(recovery: &str) -> anyhow::Result<()> {
    let rk = RecoveryKey::parse(recovery).map_err(|_| anyhow::anyhow!("that isn't a recovery key: check it and try again"))?;
    let (server, account, wrapped) = {
        let s = state().lock().unwrap();
        anyhow::ensure!(s.phase == "needs_key", "this Mac doesn't need a recovery key now");
        (s.server.clone(), s.account.clone(), s.wrapped.clone())
    };
    let wrapped = match wrapped {
        Some(w) => w,
        None => cloud::get(&server, "/v1/sync/recovery")?.and_then(|v| v["wrapped"].as_str().map(String::from)).ok_or_else(|| anyhow::anyhow!("the account has no recovery key yet"))?,
    };
    let key = rk.unwrap(&account, &wrapped).map_err(|_| anyhow::anyhow!("that recovery key doesn't open this account's settings"))?;
    save_account_key(&key)?;
    // Everything the account holds, before deciding anything.
    {
        let mut s = state().lock().unwrap();
        s.seq = 0;
        s.records.clear();
        s.pending.clear();
        s.wrapped = None;
        s.phase = "joining".into();
        save_state(&s);
    }
    let mut records = vec![];
    loop {
        let since = state().lock().unwrap().seq;
        let v = cloud::get(&server, &format!("/v1/sync?since={since}"))?.ok_or_else(|| anyhow::anyhow!("no sync on this server"))?;
        let page: PullResponse = serde_json::from_value(v)?;
        state().lock().unwrap().seq = page.seq;
        records.extend(page.records);
        if !page.more {
            break;
        }
    }
    let mut s = state().lock().unwrap();
    let mut store = Store::new();
    let mut clock = Clock::resume(s.device.clone(), s.clock.clone());
    for r in &records {
        clock.observe(&r.hlc, now_ms());
    }
    store.apply_all(records);
    s.clock = clock.last().cloned();
    s.records = store.iter().cloned().collect();
    let cloud_entries = without_keys(entries_of(&store, &s.account, &key), s.key_sync);
    let local = local_entries(s.key_sync);
    s.baseline = cloud_entries.clone().into_iter().collect();
    s.last_sync = Some(now_ms() / 1000);
    s.message = None;
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
            let key = account_key().ok_or_else(|| anyhow::anyhow!("no account key"))?;
            let store = store_of(&s);
            let settings_changed = Settings::path().metadata().and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as u64);
            let keys_changed = dino_core::keys_file().metadata().and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as u64);
            let mut merged = cloud_entries.clone();
            for (id, v) in &local {
                let here = if id.collection == "keys" { keys_changed } else { settings_changed };
                let there = store.get(&blind(&key, id)).map_or(0, |r| r.hlc.wall_ms);
                if !merged.contains_key(id) || here > there {
                    merged.insert(id.clone(), v.clone());
                }
            }
            apply_entries(&mut s, &merged)?;
        }
        _ => anyhow::bail!("choose cloud, local or merge"),
    }
    s.conflict_local.clear();
    s.phase = "ready".into();
    save_state(&s);
    drop(s);
    kick();
    Ok(())
}

pub fn ack_recovery() {
    live().lock().unwrap().recovery = None;
}

pub fn now() {
    PULL.store(true, Ordering::Relaxed);
    kick();
}

pub fn set_key_sync(on: bool) {
    let mut s = state().lock().unwrap();
    if s.key_sync == on {
        return;
    }
    s.key_sync = on;
    if !on {
        // Keys already in the account stay there, and stay on this Mac; new ones stay here.
        s.baseline.retain(|(id, _)| id.collection != "keys");
    } else {
        // Take the account's keys first, then send this Mac's.
        if let Some(key) = account_key() {
            let store = store_of(&s);
            let keys: Entries = entries_of(&store, &s.account, &key).into_iter().filter(|(id, _)| id.collection == "keys").collect();
            let here = syncable_keys();
            for (id, v) in &keys {
                if let (Some(v), false) = (v.as_str(), here.contains_key(&id.key)) {
                    let _ = dino_core::settings::set_key(&id.key, Some(v));
                }
            }
            s.baseline.extend(keys);
        }
    }
    save_state(&s);
    drop(s);
    kick();
}

/// Wipe the account's synced settings and start again with a new key and recovery key; this
/// Mac's settings are sent again. Other Macs need the new recovery key.
pub fn reset() -> anyhow::Result<()> {
    let server = {
        let s = state().lock().unwrap();
        anyhow::ensure!(matches!(s.phase.as_str(), "ready" | "needs_key" | "conflict"), "not signed in");
        s.server.clone()
    };
    cloud::send(&server, reqwest::Method::POST, "/v1/sync/reset", &json!({}))?;
    {
        let mut s = state().lock().unwrap();
        s.records.clear();
        s.pending.clear();
        s.baseline.clear();
        s.conflict_local.clear();
        s.wrapped = None;
        save_state(&s);
    }
    start_account(&server)
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
    forget_account_key();
    let mut s = state().lock().unwrap();
    let key_sync = s.key_sync;
    *s = State { server, key_sync, phase: "signed_out".into(), ..State::default() };
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

    #[test]
    fn names_are_blinded_per_account_key() {
        let (k1, k2) = (AccountKey::from_bytes("k_1", [1; 32]), AccountKey::from_bytes("k_2", [2; 32]));
        let id = RecordId::new("ssh", "build-box.example.com");
        let b = blind(&k1, &id);
        assert_eq!(b, blind(&k1, &id), "the same on every Mac with the key");
        assert_eq!(b.collection, "ssh");
        assert!(!b.key.contains("build") && b.key.len() == 32);
        assert_ne!(b, blind(&k2, &id));
        assert_ne!(b, blind(&k1, &RecordId::new("ssh", "other")));
        // collection and name can't be shifted into each other.
        assert_ne!(blind(&k1, &RecordId::new("ab", "c")), blind(&k1, &RecordId::new("a", "bc")));
    }
}
