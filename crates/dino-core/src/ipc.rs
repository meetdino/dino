//! dinod ↔ client protocol over a Unix socket.
//!
//! Every message is a frame: `[kind: u8][len: u32 BE][payload]`. Control traffic is JSON
//! request/response frames. After a successful `Attach`, the connection also carries raw
//! terminal bytes (`Data`) both ways, client `Resize`s, and a final server `Exit`.

use std::io::{self, Read, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const JSON: u8 = 0;
pub const DATA: u8 = 1;
pub const RESIZE: u8 = 2;
pub const EXIT: u8 = 3;

pub fn socket_path() -> PathBuf {
    crate::config_dir().join("dinod.sock")
}

pub fn write_frame(w: &mut impl Write, kind: u8, payload: &[u8]) -> io::Result<()> {
    let mut buf = Vec::with_capacity(5 + payload.len());
    buf.push(kind);
    buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    buf.extend_from_slice(payload);
    w.write_all(&buf)
}

pub fn read_frame(r: &mut impl Read) -> io::Result<(u8, Vec<u8>)> {
    let mut head = [0u8; 5];
    r.read_exact(&mut head)?;
    let len = u32::from_be_bytes([head[1], head[2], head[3], head[4]]) as usize;
    let mut payload = vec![0; len];
    r.read_exact(&mut payload)?;
    Ok((head[0], payload))
}

pub fn write_json(w: &mut impl Write, msg: &impl Serialize) -> io::Result<()> {
    write_frame(w, JSON, &serde_json::to_vec(msg).map_err(io::Error::other)?)
}

pub fn resize_payload(cols: u16, rows: u16) -> [u8; 4] {
    let [a, b] = cols.to_be_bytes();
    let [c, d] = rows.to_be_bytes();
    [a, b, c, d]
}

pub fn parse_resize(p: &[u8]) -> Option<(u16, u16)> {
    (p.len() == 4).then(|| (u16::from_be_bytes([p[0], p[1]]), u16::from_be_bytes([p[2], p[3]])))
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// Sessions, their live stats, and provider quotas.
    State,
    /// What can be started now: allowed by the policies, the default first.
    Launchers,
    /// Every launcher, allowed or not (for choosing policies).
    AllLaunchers,
    /// Start a session; with `worktree`, in a new git worktree (and branch) of the repo at `cwd`.
    New {
        launcher: String,
        args: Vec<String>,
        cwd: Option<String>,
        cols: u16,
        rows: u16,
        #[serde(default)]
        worktree: bool,
    },
    Kill { id: String },
    /// Switch this connection to a live terminal stream for session `id`.
    Attach { id: String, cols: u16, rows: u16 },
    Shutdown,
    /// Agent sessions outside dino that it can continue. `cloud` also asks providers (slower).
    Found { cloud: bool },
    /// Continue a found session in dino: running ones are handed off (waited on until idle,
    /// stopped, resumed here). `cwd` is where cloud sessions land.
    Adopt { session: crate::found::FoundSession, cwd: Option<String> },
    /// One prompt to several agents, each in its own git worktree of the repo at `cwd`.
    Fanout { prompt: String, launchers: Vec<String>, cwd: Option<String> },
    Groups,
    /// Repos (with their worktrees) and folders where sessions run, plus `folders` the app shows.
    Tree { folders: Vec<String> },
    /// A fan-out member's changes as a patch.
    Diff { session: String },
    /// Any session's changes, per file, for review: a fan-out member's since its fan-out began,
    /// any other since its checkout's last commit.
    Changes { id: String },
    /// Type `text` into a session, as a paste; `submit` presses Return after it.
    SendInput { id: String, text: String, submit: bool },
    /// Apply this member's changes to the user's checkout and close its group.
    Keep { session: String },
    /// Close a fan-out group: stop its agents, remove their worktrees and branches.
    Discard { group: String },
    /// Close a worktree dino made for a session: stop the sessions in it, remove it and its branch.
    /// `apply` first brings its changes into the checkout it came from, uncommitted.
    RemoveWorktree { path: String, apply: bool },
    /// The settings document.
    Settings,
    /// Replace the settings document.
    SetSettings { settings: crate::settings::Settings },
    /// Which provider keys exist and where from; never their values.
    Keys,
    /// Store a key in dino's key store, or remove it with no `value`. Takes effect at once.
    SetKey { name: String, value: Option<String> },
    /// What a PR from the session's branch would hold, to fill the Create PR form.
    PrDraft { id: String },
    /// Commit what's uncommitted as `title`, push the session's branch, and open a PR into `base`.
    PrCreate { id: String, title: String, body: String, base: String, draft: bool },
    /// Tell the session's agent which checks failed on its PR, with their logs, to fix and push.
    PrFix { id: String },
    /// Squash-merge the session's PR.
    PrMerge { id: String },
    /// Turn the session's PR automation on or off; a missing flag stays as it is.
    PrAuto { id: String, fix: Option<bool>, merge: Option<bool> },
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    State { sessions: Vec<SessionInfo>, quotas: Vec<QuotaInfo> },
    Launchers { launchers: Vec<LauncherInfo> },
    Created { id: String },
    Found { sessions: Vec<crate::found::FoundSession> },
    Groups { groups: Vec<GroupInfo> },
    Tree { repos: Vec<RepoInfo> },
    Diff { stat: DiffStat, text: String },
    /// `root` is the checkout the paths are in, `base` what they're compared with (for people).
    /// No repo: no files, and `note` says why.
    Changes { root: String, base: String, files: Vec<FileDiff>, note: Option<String> },
    Settings { settings: crate::settings::Settings },
    Keys { keys: Vec<crate::settings::KeyInfo> },
    PrDraft { draft: PrDraft },
    Pr { pr: PrInfo },
    Ok,
    Error { message: String },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LauncherInfo {
    /// Stable key, also the session name stem: "claude", "free", "codex", "shell".
    pub short: String,
    pub agent_id: String,
    pub label: String,
    pub program: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct SessionInfo {
    pub id: String,
    pub name: String,
    pub agent_id: String,
    pub title: Option<String>,
    pub exited: bool,
    /// Milliseconds since the agent last wrote to its terminal.
    pub output_ms_ago: Option<u64>,
    /// Monotonic bell count; a client notices increases.
    pub bells: u64,
    pub requests: u64,
    pub in_flight: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub last_model: Option<String>,
    pub tier: Option<String>,
    /// "working", "done", or "needs:<what>".
    pub activity: Option<String>,
    /// The fan-out group this session belongs to.
    #[serde(default)]
    pub group: Option<String>,
    /// Why the agent's last model call failed, if it did.
    #[serde(default)]
    pub error: Option<String>,
    /// Where the agent runs, symlinks resolved so it matches git's worktree paths.
    #[serde(default)]
    pub cwd: String,
    /// The PR from the session's branch, as of the last poll.
    #[serde(default)]
    pub pr: Option<PrInfo>,
    /// What dino does about the PR by itself.
    #[serde(default)]
    pub auto: AutoPr,
}

/// What dino does about a session's PR by itself. Kept with the session, so it survives dinod restarts.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
#[serde(default)]
pub struct AutoPr {
    /// When checks fail, ask the agent to fix them: once per pushed commit, at most `MAX_AUTO_FIXES` times.
    pub fix: bool,
    /// When checks pass, squash-merge.
    pub merge: bool,
    /// Fixes asked for so far; turning `fix` on again starts over.
    pub fixes: u32,
    /// Why the last automatic step failed.
    pub note: Option<String>,
}

pub const MAX_AUTO_FIXES: u32 = 3;

pub use crate::pr::{Checks, PrDraft, PrInfo};
pub use crate::worktree::Worktree;

/// A git repo, or a plain folder (no worktrees) where sessions run.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RepoInfo {
    /// The main checkout, or the folder.
    pub path: String,
    pub name: String,
    pub worktrees: Vec<Worktree>,
}

pub use crate::worktree::{DiffLine, DiffStat, FileDiff};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GroupInfo {
    pub id: String,
    pub prompt: String,
    pub repo: String,
    pub members: Vec<MemberInfo>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MemberInfo {
    pub session: String,
    pub launcher: String,
    pub branch: String,
    pub worktree: String,
    /// None when the worktree can't be read (removed by hand).
    pub stat: Option<DiffStat>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct QuotaInfo {
    pub provider: String,
    pub windows: Vec<WindowInfo>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WindowInfo {
    pub name: String,
    pub utilization: f32,
    pub resets_at: Option<u64>,
}
