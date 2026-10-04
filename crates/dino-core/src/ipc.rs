//! dinod ↔ client protocol over a Unix socket.
//!
//! Every message is a frame: `[kind: u8][len: u32 BE][payload]`. Control traffic is JSON
//! request/response frames. After a successful `Attach`, the connection also carries raw
//! terminal bytes (`Data`) both ways, client `Resize`s, and a final server `Exit`. The `Exit`'s
//! payload, when there is one, says the session ended but is kept, and can be resumed (text to
//! show the user); an empty one means it's gone.

use std::io::{self, Read, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::controls::{Controls, Knobs};

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

/// The longest payload `read_frame` takes: far past any screen, conversation page or diff, but
/// a peer can't make it set aside 4 GiB just by saying so.
pub const MAX_FRAME: usize = 256 << 20;

pub fn read_frame(r: &mut impl Read) -> io::Result<(u8, Vec<u8>)> {
    let mut head = [0u8; 5];
    r.read_exact(&mut head)?;
    let len = u32::from_be_bytes([head[1], head[2], head[3], head[4]]) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("a {len}-byte frame is over the {MAX_FRAME}-byte limit")));
    }
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
    /// `State`, once it differs from the one tagged `seen` in anything a client shows (or after a
    /// few seconds regardless): a client waits on this instead of asking over and over.
    StateChange { seen: Option<u64> },
    /// What can be started now: allowed by the policies, the default first.
    Launchers,
    /// Every launcher, allowed or not (for choosing policies).
    AllLaunchers,
    /// Every agent dino knows, whether it's on this Mac and signed in, and how to get it.
    AgentSetup,
    /// Run agent `id`'s own `install` or `sign_in` command in a new shell session.
    AgentAction { id: String, action: String },
    /// Settings → Experimental's computer use for more agents: where it stands.
    ComputerUse,
    /// Install the pinned open-computer-use, checked, in dino's own folder.
    ComputerUseInstall,
    /// Run its `doctor`: what macOS has granted it, and its setup window when something's missing.
    ComputerUsePermissions,
    /// Add it to agent `agent` (with the agent's own MCP command), or remove what dino added.
    ComputerUseAgent { agent: String, on: bool },
    /// Start a session; with `worktree`, in a new git worktree (and branch) of the repo at `cwd`.
    New {
        launcher: String,
        args: Vec<String>,
        cwd: Option<String>,
        cols: u16,
        rows: u16,
        #[serde(default)]
        worktree: bool,
        /// Mode, model and effort; what's left open comes from Settings → Agents.
        #[serde(default)]
        controls: Controls,
        /// Run it over SSH on this host (one from Settings → Environments); `cwd` is then a path there.
        #[serde(default)]
        host: Option<String>,
        /// Its agent's first message; for a shell, a line typed at its prompt.
        #[serde(default)]
        prompt: Option<String>,
        /// The session starting it, by id: a shell whose AI line handed its request on.
        #[serde(default)]
        by: Option<String>,
        /// Run the agent on this provider's model (Settings → Providers) instead of its own account.
        #[serde(default)]
        route: Option<crate::providers::ProviderRoute>,
        /// Clients should show it now (`dino <folder>`): see `SessionInfo::revealed`.
        #[serde(default)]
        reveal: bool,
        /// For a shell: attach to this tmux session at its prompt (`tmux new -A -s`, Settings →
        /// tmux), in place of `prompt`; it stays a plain shell, saying so, when tmux doesn't answer.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tmux: Option<String>,
    },
    Kill { id: String },
    /// "Keep as terminal" for shell `id`: agents typed into it stay plain processes (`on`), or
    /// report to dino again.
    KeepTerminal { id: String, on: bool },
    /// Change a session's mode, model or effort. The agent restarts, resuming its conversation;
    /// mid-turn, that waits until the turn is over. Each field replaces the session's, so `None`
    /// goes back to the agent's own default.
    SetControls { id: String, controls: Controls },
    /// Stop a background command session `id`'s agent left serving (see `SessionInfo::servers`).
    StopServer { id: String, task: String },
    /// Switch this connection to a live terminal stream for session `id`. `wait`: if its agent
    /// has ended, wait until it runs again (see `Resume`) rather than answer right away.
    Attach {
        id: String,
        cols: u16,
        rows: u16,
        #[serde(default)]
        wait: bool,
    },
    /// Start the agent of a session that ended again, in place, continuing its conversation.
    Resume { id: String },
    Shutdown,
    /// Which dino this dinod is, and any newer one it installed that it starts once sessions are idle.
    Version,
    /// Agent sessions outside dino that it can continue. `cloud` also asks providers (slower);
    /// `running_only` leaves out finished conversations on disk.
    /// Bring an agent running in a tmux pane (a found session's `tmux`) to the front in the client
    /// attached to its server. Nothing changes when no client is attached.
    TmuxShow { socket: String, pane: String },
    /// What that pane shows, for a look without taking it over.
    TmuxScreen { socket: String, pane: String },
    Found {
        cloud: bool,
        #[serde(default)]
        running_only: bool,
    },
    /// Read a conversation, a found session's or a subagent's (see `history::conversation`).
    Conversation { agent: String, session_id: String, before: Option<u64> },
    /// Continue a found session in dino: running ones are handed off (waited on until idle,
    /// stopped, resumed here). `cwd` is where cloud sessions land.
    Adopt { session: crate::found::FoundSession, cwd: Option<String> },
    /// Shell `id` is running an agent started by hand (`SessionInfo::inside`): once it's idle,
    /// stop it and resume its conversation as session `id`, in the shell's place.
    TakeOver { id: String },
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
    /// Write `text` to a session as typed keys, not a paste (the app's ⌘I to a shell's AI line).
    SendKeys { id: String, text: String },
    /// Interrupt the agent's turn the way its own key does (Esc in most), leaving the session
    /// running: what Stop does while it uses the Mac.
    Interrupt { id: String },
    /// What a shell's last command printed and its exit code, from its shell integration's
    /// marks: the context `dino ai` hands an agent.
    ShellOutput { id: String },
    /// Apply this member's changes to the user's checkout and close its group.
    Keep { session: String },
    /// Close a fan-out group: stop its agents, remove their worktrees and branches.
    Discard { group: String },
    /// Close a worktree dino made for a session: stop the sessions in it, remove it and its branch.
    /// `apply` first brings its changes into the checkout it came from, uncommitted.
    RemoveWorktree { path: String, apply: bool },
    /// Remove a worktree and its branch if git sees it merged (a branch it doesn't stays). Refuses
    /// the main checkout, a fan-out's, and one in use (see `Worktree::in_use`); refuses one with
    /// uncommitted work unless `force`, which loses that work.
    CleanWorktree {
        path: String,
        #[serde(default)]
        force: bool,
    },
    /// The settings document.
    Settings,
    /// Replace the settings document.
    SetSettings { settings: crate::settings::Settings },
    /// Which provider keys exist and where from; never their values.
    Keys,
    /// Store a key in dino's key store, or remove it with no `value`. Takes effect at once.
    SetKey { name: String, value: Option<String> },
    /// Where models come from: OpenRouter and model servers on this Mac, as last looked at.
    Providers,
    /// The models `provider` serves, as last fetched; asks again in the background when stale.
    Models { provider: String },
    /// Start connecting a hosted provider (OpenRouter) in the browser: the page to open. dinod
    /// stores the key it gets and never shows it.
    ConnectProvider { provider: String },
    /// Forget the key dino got for it.
    DisconnectProvider { provider: String },
    /// Connect coding plan `plan` (`plan-zai`) with the key the user pasted, and for the generic
    /// entry (`plan-other`) its base URL. dinod checks the key with the plan when it can, keeps
    /// it in the key store (which never syncs) and never shows it again.
    ConnectPlan { plan: String, key: String, base: Option<String> },
    /// Keeping agents running with the lid closed: `status`, `setup` (installs the one-time
    /// permission, asking for an administrator's password) or `remove`. Replies `Power`.
    Power { action: String },
    /// The Claude subscription token: `status`, `create` (runs `claude setup-token` in a new
    /// shell and keeps the token it prints), `set` (`value`: a token to keep) or `remove`.
    /// Replies `ClaudeToken`.
    ClaudeToken {
        action: String,
        #[serde(default)]
        value: Option<String>,
    },
    /// The dino account and settings sync. `action`: `status`, `login` (sign in with GitHub;
    /// `value`: the server, else the configured one), `login_email` (`value`: the address to send a
    /// sign-in link to), `login_device`, `cancel_login`, `resolve` (`value`: `cloud`, `local` or
    /// `merge`), `now`, `undo`, `logout`. Replies `Sync`, or `Connect` with the page to open for
    /// `login`.
    Sync {
        action: String,
        #[serde(default)]
        value: Option<String>,
    },
    /// What a PR from the session's branch would hold, to fill the Create PR form.
    PrDraft { id: String },
    /// Commit what's uncommitted as `title`, push the session's branch, and open a PR into `base`.
    PrCreate { id: String, title: String, body: String, base: String, draft: bool },
    /// Tell the session's agent which checks failed on its PR, with their logs, to fix and push.
    PrFix { id: String },
    /// Squash-merge the session's PR.
    PrMerge { id: String },
    /// Have Claude review the session's changes (what `Changes` shows) for bugs. Takes minutes.
    Review { id: String },
    /// Stop the session's running review; its `Review` request answers with an error.
    ReviewCancel { id: String },
    /// Turn the session's PR automation on or off; a missing flag stays as it is.
    PrAuto { id: String, fix: Option<bool>, merge: Option<bool> },
    /// Name a session, over the title its agent sets; an empty name goes back to that title.
    Rename { id: String, name: String },
    /// Keep a session at the top of its group, and out of dino's own archiving (or stop).
    Pin { id: String, pinned: bool },
    /// Stop a session but keep it to pick up later. Its worktree goes too when nothing in it
    /// would be lost (clean, and pushed or merged); `Unarchive` makes it again from the branch.
    Archive { id: String },
    /// Archived sessions, newest first.
    Archived,
    /// Start an archived session again, resuming its agent's conversation where the agent can.
    Unarchive { id: String },
    /// Forget an archived session for good.
    DeleteArchived { id: String },
    /// Delete session `id`: stop its agent, forget it, and remove the worktree dino made for it
    /// (its branch too when merged or empty; one with unmerged commits stays). Never a shell's
    /// folder or a checkout of the user's. The agent's own conversation file stays. `dry_run`
    /// only says what it would do. Answers `Deletion`.
    Delete {
        id: String,
        #[serde(default)]
        dry_run: bool,
    },
    /// Every worktree dino made, with its size on disk.
    Storage,
    /// Remove a worktree dino made (and its branch when merged), never forcing: refuses one
    /// with uncommitted work, one a session runs in, and fan-out members (discard the group).
    RemoveStored { path: String },
    /// Remove every worktree dino made that nothing would be lost from: no session running in
    /// it, not a fan-out member, no uncommitted changes, and its commits merged or pushed.
    FreeUpSpace,
    /// The dev servers the session's folder configures (`.dino/launch.json`, `.claude/launch.json`).
    PreviewConfigs { id: String },
    /// Start the named dev server in the session's folder; it stops with the session. `approved`
    /// is the configuration the user saw and agreed to run: started only if the launch file still
    /// says exactly that.
    PreviewStart {
        id: String,
        name: String,
        #[serde(default)]
        approved: Option<crate::preview::PreviewConfig>,
    },
    PreviewStop { id: String, name: String },
    /// What the named dev server has printed (the tail).
    PreviewLog { id: String, name: String },
    /// Scheduled tasks, with their history and next run.
    ScheduleList,
    /// Add a task (no `id`) or replace one; pausing and resuming is a put with `enabled` changed.
    SchedulePut { task: crate::schedule::ScheduledTask },
    ScheduleDelete { id: String },
    /// Run a task now, whatever its schedule; answers with the session it started.
    ScheduleRun { id: String },
    /// Check for due tasks now, as if the time were `now` (seconds since the epoch) when given.
    ScheduleTick { now: Option<u64> },
    /// Start a session for another agent (`dino mcp`): `prompt` is its first message, `by` the
    /// session asking, when there is one. With `worktree`, in a new git worktree of the repo at `cwd`.
    Start { launcher: String, cwd: Option<String>, prompt: Option<String>, #[serde(default)] worktree: bool, by: Option<String> },
    /// What a session has been doing, as text: its conversation's last turns when dino can read
    /// them, and the screen now. `lines` bounds the screen part.
    ReadSession { id: String, lines: Option<u32> },
    /// A subagent and its conversation so far (answers `Subagent`): subagent `agent` of session
    /// `session`, or the one that made `worktree`.
    ReadSubagent {
        #[serde(default)]
        session: Option<String>,
        #[serde(default)]
        agent: Option<String>,
        #[serde(default)]
        worktree: Option<String>,
    },
    /// Type `text` into session `id` and submit it, only while it's between turns: refused while
    /// it works or waits on a permission. `by` is the session sending it.
    Message { id: String, text: String, by: Option<String> },
    /// Answer a question about session `id` (side chat); Claude reads it without disturbing it.
    /// Takes up to minutes; `AskCancel` stops it.
    Ask { id: String, question: String },
    AskCancel { id: String },
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    State {
        sessions: Vec<SessionInfo>,
        quotas: Vec<QuotaInfo>,
        /// Whether the Mac is being kept awake with its lid closed; absent from an older dinod.
        #[serde(default)]
        power: Option<PowerInfo>,
        /// Tags this state for `StateChange`; only in a reply to one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<u64>,
    },
    Launchers { launchers: Vec<LauncherInfo> },
    AgentSetup { agents: Vec<AgentSetupInfo> },
    ComputerUse { info: ComputerUseInfo },
    Created { id: String },
    ShellOutput { output: Option<String>, exit: Option<i32> },
    Found { sessions: Vec<crate::found::FoundSession> },
    /// Shown in the tmux client on `tty`; `session` is the dino tab that client runs in, if any.
    /// Neither when no client is attached.
    TmuxShown { tty: Option<String>, session: Option<String> },
    Conversation { page: crate::history::Page },
    Groups { groups: Vec<GroupInfo> },
    Tree { repos: Vec<RepoInfo> },
    Diff { stat: DiffStat, text: String },
    /// `root` is the checkout the paths are in, `base` what they're compared with (for people).
    /// No repo: no files, and `note` says why.
    Changes { root: String, base: String, files: Vec<FileDiff>, note: Option<String> },
    /// `settings` is what's in effect; `locked` the key paths an organization sets ("policies.allow_bypass").
    Settings {
        settings: crate::settings::Settings,
        #[serde(default)]
        locked: Vec<String>,
        /// The managed file each locked path comes from, by path.
        #[serde(default)]
        locked_from: std::collections::BTreeMap<String, String>,
        /// Hosts in `~/.ssh/config`, to suggest in Settings → Environments.
        #[serde(default)]
        ssh_config_hosts: Vec<String>,
    },
    Keys { keys: Vec<crate::settings::KeyInfo> },
    Providers { providers: Vec<crate::providers::ProviderInfo> },
    /// Open this page to go on.
    Connect { url: String },
    Sync { status: SyncStatus },
    Power { power: PowerInfo },
    ClaudeToken { token: ClaudeTokenInfo },
    /// `loading`: dinod is asking the provider now; ask again for what it says.
    Models { provider: String, models: Vec<ModelRow>, loading: bool, error: Option<String> },
    PrDraft { draft: PrDraft },
    Pr { pr: PrInfo },
    Review { findings: Vec<crate::review::Finding> },
    Archived { sessions: Vec<ArchivedInfo> },
    /// What deleting a session does, or did.
    Deletion { deletion: Deletion },
    Storage { worktrees: Vec<StoredWorktree> },
    /// What Free up space removed, and how many bytes that gave back (as last measured).
    Freed { removed: Vec<String>, bytes: u64 },
    /// A broken launch file lists nothing, and `error` says why.
    PreviewConfigs { configs: Vec<crate::preview::PreviewConfig>, error: Option<String> },
    PreviewLog { text: String },
    Schedule { tasks: Vec<crate::schedule::ScheduledTask> },
    Text { text: String },
    Subagent { subagent: SubagentView },
    Ok,
    Error { message: String },
    /// `installed`: a newer `dino` this dinod put in place of its own binary; it restarts into it
    /// once no agent is working and no shell is running a command.
    Version { dino: String, installed: Option<String> },
}

/// What deleting a session does (`Request::Delete`): the worktree that goes with it and what's in
/// it that would be lost.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Deletion {
    /// The worktree dino made for it, removed with it. None for a shell, a session in a folder of
    /// the user's, or on another machine.
    #[serde(default)]
    pub worktree: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    /// Files with uncommitted changes in that worktree, new files included: lost with it.
    #[serde(default)]
    pub uncommitted: u32,
    /// Commits on no remote and not on the branch it came from.
    #[serde(default)]
    pub unpushed: u32,
    /// The branch stays: it has commits that aren't merged. Else it goes with the worktree.
    #[serde(default)]
    pub keeps_branch: bool,
    /// The worktree stays because another session is in it: that session's name.
    #[serde(default)]
    pub kept_for: Option<String>,
}

/// The dino account on this Mac and where settings sync stands.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct SyncStatus {
    /// `signed_out`, `signing_in`, `conflict` (this Mac and the account differ: choose), `ready`.
    pub phase: String,
    pub server: String,
    pub email: Option<String>,
    /// Where the account's web page is (devices, what's stored, export, delete).
    pub account_url: Option<String>,
    /// Unix seconds of the last exchange with the server.
    pub last_sync: Option<u64>,
    /// Changes made here, not yet taken by the server.
    pub pending: usize,
    /// Synced settings on this Mac: the ones set to something other than their default (a default
    /// isn't stored), as the account page lists them.
    pub synced: usize,
    /// The same, in a person's terms ("Claude Code defaults", "2 terminal settings"); empty when
    /// every setting is at its default.
    #[serde(default)]
    pub synced_what: Vec<String>,
    /// For `login_email`: the address the sign-in link went to, while it waits to be opened.
    #[serde(default)]
    pub email_sent_to: Option<String>,
    /// For `login_device`: the code to enter and where.
    pub device_code: Option<String>,
    pub device_url: Option<String>,
    /// When `phase` is `conflict`: settings only here, only in the account, and set differently.
    pub conflict: Option<(usize, usize, usize)>,
    /// Earlier versions of `settings.toml` kept before a sync changed it (newest first).
    pub snapshots: usize,
    /// What went wrong, or what happened that the person should know (signed out elsewhere).
    pub message: Option<String>,
}

/// One agent in Settings → Agents. The commands are the agent's own; dino runs them in a shell.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AgentSetupInfo {
    pub id: String,
    pub name: String,
    /// Where it's installed; none when it isn't.
    pub path: Option<String>,
    pub version: Option<String>,
    /// From the agent's own status command; none when it has no quick way to ask.
    pub signed_in: Option<bool>,
    /// How it's signed in: "Claude Max", "ChatGPT", "API key".
    pub account: Option<String>,
    pub install: String,
    pub sign_in: Option<String>,
    /// What to type in the agent once it's open, for ones that sign in from inside ("/login").
    pub sign_in_hint: Option<String>,
    pub homepage: String,
    /// What signing in means for it, when that isn't obvious ("Pi has no models of its own…").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sign_in_note: Option<String>,
}

/// Computer use for agents that have none of their own (Settings → Experimental).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ComputerUseInfo {
    /// The open-computer-use release dino pins.
    pub version: String,
    /// It's in dino's folder, checked.
    pub installed: bool,
    /// What macOS has granted its app, as its `doctor` last said; none until asked.
    #[serde(default)]
    pub accessibility: Option<bool>,
    #[serde(default)]
    pub screen_recording: Option<bool>,
    /// The agents on this Mac it can be added to.
    pub agents: Vec<ComputerUseAgentInfo>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ComputerUseAgentInfo {
    pub id: String,
    pub name: String,
    /// dino added it, and it's still there as dino left it.
    pub on: bool,
    /// What dino runs (or edits) to add it, exactly.
    pub command: String,
    /// The agent has computer use of its own, and how to turn it on; it isn't offered then.
    #[serde(default)]
    pub native: Option<String>,
    /// The agent already has a server by that name that dino didn't add: left alone.
    #[serde(default)]
    pub theirs: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LauncherInfo {
    /// Stable key, also the session name stem: "claude", "free", "codex", "shell".
    pub short: String,
    pub agent_id: String,
    pub label: String,
    pub program: String,
    /// The mode, model and effort it offers.
    #[serde(default)]
    pub knobs: Knobs,
    /// It can answer one request with no tools, as the shell's ⌘I asks (`Agent::answers_once`).
    #[serde(default)]
    pub answers_once: bool,
}

/// The pane a tmux client in a dino shell shows.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TmuxPane {
    /// `session:window.pane`.
    pub target: String,
    /// `session:window name`.
    pub label: String,
    /// Something other than a shell runs in it.
    pub busy: bool,
    /// Bells and notifications from the client's tmux, newest last, each with where it came from;
    /// numbered so a client can tell which it has shown.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alerts: Vec<TmuxAlert>,
}

/// One bell or notification from a tmux in a dino shell.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TmuxAlert {
    pub seq: u64,
    pub text: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct SessionInfo {
    pub id: String,
    pub name: String,
    pub agent_id: String,
    pub title: Option<String>,
    pub exited: bool,
    /// How its agent exited, when it has: 0 is a clean exit (the user quit it).
    #[serde(default)]
    pub exit_code: Option<u32>,
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
    /// "working", "done", "needs:<what>", or "waiting:<what>" when the turn ended on background
    /// work that still runs ("waiting:1 agent, 2 commands"), or "server:<ports>" when all that runs
    /// is a server ("server:3000, 8080").
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
    /// The SSH host it runs on (`cwd` is then a path there); none for this Mac.
    #[serde(default)]
    pub host: Option<String>,
    /// The PR from the session's branch, as of the last poll.
    #[serde(default)]
    pub pr: Option<PrInfo>,
    /// What dino does about the PR by itself.
    #[serde(default)]
    pub auto: AutoPr,
    /// Dev servers started for the session's preview, running or ended.
    #[serde(default)]
    pub previews: Vec<PreviewInfo>,
    /// The last local web address the agent printed (a dev server it started), to offer a preview of.
    #[serde(default)]
    pub local_url: Option<String>,
    /// The mode, model and effort it runs with.
    #[serde(default)]
    pub controls: Controls,
    /// Asked for mid-turn; applied (by a restart) once the turn is over.
    #[serde(default)]
    pub pending: Option<Controls>,
    /// The permission mode the agent says it's in, in dino's words: it can differ from `controls`
    /// when changed in the agent itself (Claude's Shift+Tab).
    #[serde(default)]
    pub agent_mode: Option<String>,
    /// Tokens the last model call read (cached ones included): how full the context window is.
    #[serde(default)]
    pub context_tokens: u64,
    /// The size of that model's context window, when dino knows it.
    #[serde(default)]
    pub context_limit: Option<u64>,
    /// The scheduled task that started it, by name.
    #[serde(default)]
    pub scheduled: Option<String>,
    /// The session whose agent started it (through `dino mcp`), by id.
    #[serde(default)]
    pub started_by: Option<String>,
    /// The session whose agent last messaged it, by id.
    #[serde(default)]
    pub messaged_by: Option<String>,
    /// The name the user gave it (`Rename`); `title` already shows it over the agent's.
    #[serde(default)]
    pub label: Option<String>,
    /// Pinned: kept at the top of its group, never archived by dino on its own.
    #[serde(default)]
    pub pinned: bool,
    /// A shell's "Keep as terminal": agents typed into it don't report to dino.
    #[serde(default)]
    pub keep_terminal: bool,
    /// When someone last asked for it to be shown (`dino <folder>`), in ms since the epoch.
    #[serde(default)]
    pub revealed: Option<u64>,
    /// What the agent tracks underneath: its task list, subagents and background commands.
    #[serde(default)]
    pub tasks: SessionTasks,
    /// A shell's foreground agent that someone started by hand: its title and busy/idle status.
    #[serde(default)]
    pub inside: Option<crate::found::FoundSession>,
    /// Where a shell with shell integration says it is now (`cwd` is where it started), and the
    /// exit code of the last command it ran.
    #[serde(default)]
    pub shell_cwd: Option<String>,
    /// A shell running a command (not at its prompt), as last looked at: closing it would stop it.
    /// Never for a tmux client: closing that only detaches it.
    #[serde(default)]
    pub running: bool,
    /// A shell whose foreground is a tmux client: what the client shows. Closing the tab detaches
    /// it, and the tmux server keeps everything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux: Option<TmuxPane>,
    #[serde(default)]
    pub last_exit: Option<i32>,
    /// Background commands its agent left running that listen on a port: a dev server, not work
    /// to wait on. With nothing else left running, `activity` is "server:<ports>".
    #[serde(default)]
    pub servers: Vec<ServerInfo>,
    /// The provider and model it runs on, when it isn't its agent's own account.
    #[serde(default)]
    pub route: Option<crate::providers::ProviderRoute>,
    /// What its agent is using outside its terminal right now, by its tool calls: "computer" (apps
    /// on this Mac) or "browser". Stays a few seconds after the last call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub using: Option<String>,
}

/// A background command that serves: the agent's task id for it, and where it listens.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ServerInfo {
    pub task: String,
    pub command: String,
    pub ports: Vec<u16>,
}

/// From the agent's hooks, so Claude only for now; empty for agents that don't report them.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct SessionTasks {
    #[serde(default)]
    pub todos: Vec<TodoInfo>,
    #[serde(default)]
    pub subagents: Vec<SubagentInfo>,
    #[serde(default)]
    pub background: Vec<BackgroundInfo>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct TodoInfo {
    pub id: String,
    pub subject: String,
    /// "pending", "in_progress" or "completed".
    pub status: String,
    /// Shown while it's in progress ("Running the tests").
    #[serde(default)]
    pub active: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SubagentInfo {
    pub id: String,
    #[serde(default)]
    pub agent_type: Option<String>,
    /// The task it was given.
    #[serde(default)]
    pub description: Option<String>,
    pub running: bool,
    /// Unix seconds; 0 when dino didn't see it start.
    #[serde(default)]
    pub started: u64,
    #[serde(default)]
    pub finished: Option<u64>,
    /// Its own worktree, when it runs in one; symlinks resolved.
    #[serde(default)]
    pub worktree: Option<String>,
}

/// A subagent, to watch: it runs inside its session's agent, so there's no terminal of its own,
/// only the conversation its agent writes down.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SubagentView {
    pub id: String,
    /// The session whose agent started it.
    pub session: String,
    /// Its own worktree, when it has one.
    #[serde(default)]
    pub worktree: Option<String>,
    #[serde(default)]
    pub agent_type: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    pub running: bool,
    /// What it was asked, even when `conversation` starts after it.
    #[serde(default)]
    pub task: Option<String>,
    /// The newest part of its conversation (older parts: `Conversation` with its id); `None` when
    /// dino can't read it (not Claude, or it's gone).
    #[serde(default)]
    pub conversation: Option<crate::history::Page>,
}

/// A shell command or monitor the agent runs in the background.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct BackgroundInfo {
    pub id: String,
    /// "shell" or "monitor".
    pub kind: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    pub running: bool,
    #[serde(default)]
    pub started: u64,
    #[serde(default)]
    pub finished: Option<u64>,
}

/// A stopped session kept to start again.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ArchivedInfo {
    pub id: String,
    pub name: String,
    pub label: Option<String>,
    pub launcher: String,
    pub cwd: String,
    pub branch: Option<String>,
    /// Unix seconds.
    pub archived_at: u64,
    /// The agent's own session id, when dino can resume its conversation.
    pub resumable: bool,
    /// Its worktree was removed and comes back from `branch` when it's started again.
    pub worktree_removed: bool,
    /// The agent (`claude`, `codex`…) and its own conversation id, to read the conversation back.
    pub agent: String,
    pub agent_session: Option<String>,
    pub pinned: bool,
}

/// A worktree dino made, for the Storage list.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StoredWorktree {
    pub path: String,
    pub repo: String,
    pub branch: String,
    /// "in_progress", "ready", "merged" or "empty" (see `worktree::Summary`).
    pub state: String,
    /// Has uncommitted changes, so dino won't remove it.
    pub dirty: bool,
    /// Bytes on disk; None until measured.
    pub size: Option<u64>,
    /// A live session running in it, by name.
    pub session: Option<String>,
    /// That session's state: "working", "idle", or "ended" (its agent exited; removing the
    /// worktree archives it). None when no session is there.
    #[serde(default)]
    pub session_state: Option<String>,
    /// Kept for an archived session (removing it here still lets that one come back from its branch).
    pub archived: bool,
    /// Belongs to a fan-out group.
    pub fanout: bool,
    /// Free up space would remove it: see [`Request::FreeUpSpace`].
    #[serde(default)]
    pub reclaimable: bool,
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
pub use crate::preview::PreviewInfo;
pub use crate::worktree::Worktree;

/// A git repo, or a plain folder (no worktrees) where sessions run.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RepoInfo {
    /// The main checkout, or the folder.
    pub path: String,
    pub name: String,
    pub worktrees: Vec<Worktree>,
    /// The branch PRs go into (origin's HEAD, else main or master); None for a plain folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,
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

/// Keeping agents running with the lid closed, as dinod sees it.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct PowerInfo {
    /// System sleep is off now because dino turned it off.
    pub holding: bool,
    /// Since when (unix seconds).
    pub since: Option<u64>,
    /// Sleep was already off, turned off by someone else: dino leaves it alone.
    pub external: bool,
    /// Why it stopped last time, when that's worth saying (battery, heat, time…), and when.
    pub note: Option<String>,
    pub note_at: Option<u64>,
    /// The one-time permission is installed; only filled in for `Power { status }`.
    pub ready: Option<bool>,
    /// The last time turning sleep off or on failed.
    pub error: Option<String>,
}

/// The Claude subscription token, as dinod holds it: never the token itself.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct ClaudeTokenInfo {
    /// Kept, and shaped like one `claude setup-token` makes.
    pub set: bool,
    /// Its kind and last four characters.
    pub masked: Option<String>,
    /// When it was made and runs out (unix seconds); unknown for a pasted one.
    pub created: Option<u64>,
    pub expires: Option<u64>,
    /// Whether Claude Code on this Mac is signed in on its own, when dinod last looked.
    pub signed_in: Option<bool>,
    /// The shell running `claude setup-token`, while dinod waits for the token it prints.
    pub creating: Option<String>,
    /// What went wrong with the last attempt.
    pub error: Option<String>,
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

/// A provider's model and what dino makes of it for each agent, the one to run it in first.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ModelRow {
    #[serde(flatten)]
    pub model: crate::providers::ProviderModel,
    pub agents: Vec<crate::compat::Verdict>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_up_to_the_limit() {
        let mut buf = vec![];
        write_frame(&mut buf, DATA, b"hello").unwrap();
        assert_eq!(read_frame(&mut buf.as_slice()).unwrap(), (DATA, b"hello".to_vec()));

        // Refused from the header alone, before anything is allocated or read.
        let mut huge = vec![JSON];
        huge.extend_from_slice(&(MAX_FRAME as u32 + 1).to_be_bytes());
        let err = read_frame(&mut huge.as_slice()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        let mut most = vec![JSON];
        most.extend_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(read_frame(&mut most.as_slice()).unwrap_err().kind(), io::ErrorKind::InvalidData);
    }
}
