//! dinod ↔ client protocol over a Unix socket.
//!
//! Every message is a frame: `[kind: u8][len: u32 BE][payload]`. Control traffic is JSON
//! request/response frames. After a successful `Attach`, the connection also carries raw
//! terminal bytes (`Data`) both ways, client `Resize`s and `Focus` changes, and a final server
//! `Exit`. The `Exit`'s
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
/// The client's terminal gained (payload `[1]`) or lost (`[0]`) focus.
pub const FOCUS: u8 = 4;

/// dinod's socket's name, in dino's folder.
pub const SOCKET_NAME: &str = "dinod.sock";

pub fn socket_path() -> PathBuf {
    crate::config_dir().join(SOCKET_NAME)
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
    /// The build cache shared by every session's builds (Settings → Workspaces → Worktrees): where
    /// it stands, and its hits and misses.
    BuildCache,
    /// Install sccache for it, in a new shell where the command shows as it runs.
    BuildCacheInstall,
    /// Its server isn't answering: start it again (asked by `dino rustc-wrapper`, unanswered).
    BuildCacheEnsure,
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
        /// Start this agent even while it's at its limit, rather than the agent its fallback
        /// names for new sessions (Settings → Agents).
        #[serde(default)]
        stay: bool,
    },
    Kill { id: String },
    /// Close session `id` for everyone as `Kill` does, but keep its program and screen, unseen,
    /// for `undo_ms`: `Reopen` brings it back as it was until then. 0 is `Kill`.
    Close { id: String, undo_ms: u64 },
    /// Bring back session `id`, closed with `Close` and still within its time to undo.
    Reopen { id: String },
    /// "Keep as terminal" for shell `id`: agents typed into it stay plain processes (`on`), or
    /// report to dino again.
    KeepTerminal { id: String, on: bool },
    /// Change a session's mode, model or effort. The agent restarts, resuming its conversation;
    /// mid-turn, that waits until the turn is over. Each field replaces the session's, so `None`
    /// goes back to the agent's own default.
    SetControls { id: String, controls: Controls },
    /// Stop a background command session `id`'s agent left serving (see `SessionInfo::servers`).
    StopServer { id: String, task: String },
    /// Stop builds left running by sessions that are gone (`Response::State`'s `leftovers`), by
    /// their pids as listed there; none named: all of them.
    StopLeftovers {
        #[serde(default)]
        pids: Vec<u32>,
    },
    /// Switch this connection to a live terminal stream for session `id`. `wait`: if its agent
    /// has ended, wait until it runs again (see `Resume`) rather than answer right away.
    Attach {
        id: String,
        cols: u16,
        rows: u16,
        #[serde(default)]
        wait: bool,
        /// The client's own scrollback, in bytes as Ghostty's `scrollback-limit` counts them: the
        /// replay brings that much (and the session keeps it from then on). Without it, a shorter
        /// replay, for a terminal whose own scrollback shouldn't fill with it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scrollback: Option<u64>,
    },
    /// Start the agent of a session that ended again, in place, continuing its conversation.
    Resume { id: String },
    /// Fork session `id`: a new session on a copy of its conversation, made by the agent's own
    /// fork (see `LauncherInfo::forks`), with its mode, model, flags and account. The original
    /// conversation stays as it is. With `worktree`, in a new git worktree off the session's
    /// checkout, its uncommitted edits carried over. `prompt` is the fork's first message.
    /// Answers `Created`.
    Fork {
        id: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        worktree: bool,
        #[serde(default)]
        prompt: Option<String>,
    },
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
    /// Usage statistics over `range` (see `usage::report`): what the proxy carried, and agents'
    /// own records, read first for what's new.
    Stats { range: crate::usage::Range },
    /// Forget all usage statistics.
    StatsClear,
    /// Continue a found session in dino: running ones are handed off (waited on until idle,
    /// stopped, resumed here). `cwd` is where cloud sessions land.
    Adopt { session: crate::found::FoundSession, cwd: Option<String> },
    /// Shell `id` is running an agent started by hand (`SessionInfo::inside`): once it's idle,
    /// stop it and resume its conversation as session `id`, in the shell's place.
    TakeOver { id: String },
    /// Stop waiting to continue a conversation in dino (`TakeOver`, `Adopt`, which wait for its
    /// turn to end): `id` is the shell's session id, or for `Adopt` the conversation's id. The
    /// waiting request then fails with "cancelled", and the agent runs on where it is. An error
    /// when nothing waits for it (it may have just moved).
    CancelTakeOver { id: String },
    /// Repos (with their worktrees) and folders where sessions run, plus `folders` the app shows.
    /// `known`: the version of the tree the asker has; the same one is answered with `same`.
    Tree {
        folders: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        known: Option<String>,
    },
    /// Any session's changes, per file, for review: one in a worktree dino made since that
    /// worktree began, any other since its checkout's last commit.
    Changes { id: String },
    /// Type `text` into a session, as a paste; `submit` presses Return after it.
    SendInput { id: String, text: String, submit: bool },
    /// Write `text` to a session as typed keys, not a paste (the app's ⌘I to a shell's AI line).
    SendKeys { id: String, text: String },
    /// Interrupt the agent's turn the way its own key does (Esc in most), leaving the session
    /// running: what Stop does while it uses the Mac.
    Interrupt { id: String },
    /// What runs in the foreground of session `id`'s terminal right now, when it isn't the
    /// session's own program (a shell at its prompt): what closing it would stop.
    Foreground { id: String },
    /// What a shell's last command printed and its exit code, from its shell integration's
    /// marks: the context `dino ai` hands an agent.
    ShellOutput { id: String },
    /// What session `id`'s processes cost the Mac now: its program and everything under it.
    /// dinod measures only when asked, so a client asks while it shows the answer (a row's hover
    /// card, every couple of seconds) and stops when it's gone. Answers `SessionCost`.
    SessionCost { id: String },
    /// Close a worktree dino made for a session: stop the sessions in it, remove it and its branch.
    /// `apply` first brings its changes into the checkout it came from, uncommitted.
    RemoveWorktree { path: String, apply: bool },
    /// Remove a worktree and its branch if git sees it merged (a branch it doesn't stays). Refuses
    /// the main checkout and one in use (see `Worktree::in_use`); refuses one with
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
    /// Your other Claude accounts, which Claude Code goes on with when the one it signed in with
    /// is at its limit: `status`, `add` (`value`: a token `claude setup-token` printed, or text
    /// holding one), `create` (runs `claude setup-token` in a new shell and adds the token it
    /// prints), `remove` (`account`: its number) or `order` (`order`: every account's number, in
    /// the order to try them). Replies `ClaudeAccounts`; never with a token.
    ClaudeAccounts {
        action: String,
        #[serde(default)]
        value: Option<String>,
        #[serde(default)]
        account: Option<u32>,
        #[serde(default)]
        order: Vec<u32>,
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
    /// with uncommitted work and one a session runs in.
    RemoveStored { path: String },
    /// Remove every worktree dino made that nothing would be lost from: no session running in
    /// it, no uncommitted changes, and its commits merged or pushed.
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
    /// Automations (scheduled tasks, as they began), with their history and next run. The request
    /// names stay as they were: clients from before automations keep working.
    ScheduleList,
    /// Add an automation (no `id`) or replace one; pausing and resuming is a put with `enabled` changed.
    SchedulePut { task: crate::schedule::ScheduledTask },
    ScheduleDelete { id: String },
    /// Run an automation now, whatever its trigger and conditions; answers with the session it
    /// started (empty when its action starts none, as a command doesn't).
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
        /// Agents at their limit (Settings → Agents says what new sessions start with instead).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        limits: Vec<AgentLimit>,
        /// Builds dinod found running for no session as it started (an agent's, left behind
        /// when its session or an older dinod went), until they end or are stopped.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        leftovers: Vec<Leftover>,
        /// Tags this state for `StateChange`; only in a reply to one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<u64>,
    },
    Launchers { launchers: Vec<LauncherInfo> },
    AgentSetup { agents: Vec<AgentSetupInfo> },
    ComputerUse { info: ComputerUseInfo },
    BuildCache { info: BuildCacheInfo },
    Created { id: String },
    ShellOutput { output: Option<String>, exit: Option<i32> },
    SessionCost { cost: SessionCost },
    Found { sessions: Vec<crate::found::FoundSession> },
    /// Shown in the tmux client on `tty`; `session` is the dino tab that client runs in, if any.
    /// Neither when no client is attached.
    TmuxShown { tty: Option<String>, session: Option<String> },
    Conversation { page: crate::history::Page },
    Stats { report: Box<crate::usage::Report> },
    /// `same`: it's the version the asker has (`repos` is left empty, a thousand worktrees
    /// aren't sent and decoded again every few seconds for nothing).
    Tree {
        repos: Vec<RepoInfo>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        same: bool,
    },
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
        /// Why `settings.toml` doesn't parse: `settings` are then the last good ones, and saving is refused.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    Keys { keys: Vec<crate::settings::KeyInfo> },
    Providers { providers: Vec<crate::providers::ProviderInfo> },
    /// Open this page to go on.
    Connect { url: String },
    Sync { status: SyncStatus },
    Power { power: PowerInfo },
    ClaudeToken { token: ClaudeTokenInfo },
    ClaudeAccounts { accounts: ClaudeAccountsInfo },
    /// `loading`: dinod is asking the provider now; ask again for what it says.
    Models { provider: String, models: Vec<ModelRow>, loading: bool, error: Option<String> },
    PrDraft { draft: PrDraft },
    Pr { pr: PrInfo },
    Review { findings: Vec<crate::review::Finding> },
    Archived { sessions: Vec<ArchivedInfo> },
    /// What deleting a session does, or did.
    Deletion { deletion: Deletion },
    Storage { worktrees: Vec<StoredWorktree> },
    /// The answer to `Foreground`: nothing when the session's own program has the terminal.
    Foreground { foreground: Option<ForegroundProcess> },
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
    /// once no agent is working and no shell is running a command. `launchd`: the launch agent
    /// running this dinod (its label), if launchd started it. `build`: which build of `dino` it
    /// is (the commit it was built from, for app/build.sh and release builds), and `exe` the
    /// binary it runs from: an app restarts a dinod from another build into its own.
    Version {
        dino: String,
        installed: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        launchd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        build: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exe: Option<String>,
    },
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

/// What a session's processes cost the Mac (`Request::SessionCost`), counted as Activity Monitor
/// counts them: memory is the physical footprint, CPU is per core (100 is one core).
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct SessionCost {
    /// Its program's and every process under it, together.
    #[serde(default)]
    pub mem_bytes: u64,
    /// CPU since the last time anyone asked (a moment ago when nobody has).
    #[serde(default)]
    pub cpu_pct: f64,
    /// CPU over the last `avg_secs` seconds, children that ended in that time included.
    #[serde(default)]
    pub cpu_avg_pct: f64,
    #[serde(default)]
    pub avg_secs: u32,
    /// How many processes that is.
    #[serde(default)]
    pub processes: u32,
    /// The process under it using the most memory, on its own (none when it runs nothing).
    #[serde(default)]
    pub top_child: Option<ProcessCost>,
    /// The builds it runs (Cargo, SwiftPM, Xcode, make…), each with everything under it, those
    /// its agent started in the background included.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub builds: Vec<ProcessCost>,
    /// Fields from a newer dinod, kept as they came.
    #[serde(flatten, default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// One process of a session's (`SessionCost::top_child`), or one of its builds with everything
/// under it (`SessionCost::builds`).
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct ProcessCost {
    /// Its name, as Activity Monitor shows it ("rustc", "node"); a build's says what it does
    /// ("cargo test").
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub pid: u32,
    #[serde(default)]
    pub mem_bytes: u64,
    #[serde(default)]
    pub cpu_pct: f64,
    /// A build that runs apart from the agent's terminal: in the background, or left by an
    /// earlier run of the agent.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub background: bool,
    #[serde(flatten, default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// A build running for no session (`Response::State`'s `leftovers`): what it is and where.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Leftover {
    /// The process it all runs under, which stopping it stops with everything under it.
    pub pid: u32,
    /// What it builds with ("cargo test", "swift-build").
    #[serde(default)]
    pub name: String,
    /// The folder it builds in.
    #[serde(default)]
    pub cwd: String,
    /// When it started, in seconds since the epoch.
    #[serde(default)]
    pub started: u64,
    #[serde(flatten, default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra: serde_json::Map<String, serde_json::Value>,
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

/// The build cache (see `crate::build_cache`), as Settings and `dino build-cache` show it.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct BuildCacheInfo {
    /// Turned on (Settings, or the organization's).
    pub enabled: bool,
    /// sccache, where dino found it; none: not installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sccache: Option<String>,
    /// What `sccache --version` says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// How to install it.
    pub install: String,
    /// Why sessions don't get it although it's on and installed (dinod's own environment sets up
    /// a wrapper or sccache already, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unused: Option<String>,
    /// Its server is up.
    pub running: bool,
    /// The folder it keeps the cache in.
    pub dir: String,
    /// The limit, and what it holds now (while the server is up).
    pub max_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    /// Since the server started: compiles it was asked for, found in the cache, compiled and
    /// kept, and that it can't keep (programs, build scripts, incremental builds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<BuildCacheStats>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct BuildCacheStats {
    pub requests: u64,
    pub hits: u64,
    pub misses: u64,
    pub not_cacheable: u64,
    /// Compiles the cache didn't take part in after all (errors, not a compile at all).
    pub other: u64,
}

impl BuildCacheStats {
    /// Hits among the compiles it could have kept, in percent.
    pub fn hit_rate(&self) -> Option<f64> {
        let cacheable = self.hits + self.misses;
        (cacheable > 0).then(|| self.hits as f64 * 100.0 / cacheable as f64)
    }
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
    /// The APIs it can talk to a provider's model in, best first: what a route must serve for it
    /// (to run on, or fall back to).
    #[serde(default)]
    pub formats: Vec<crate::providers::Format>,
    /// Its conversations can be forked (`Request::Fork`), by the agent's own fork.
    #[serde(default)]
    pub forks: bool,
}

/// The process group leading a session's terminal, when it isn't the session's own program.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ForegroundProcess {
    pub pid: u32,
    /// Its process name: `vim`, `htop`, `cargo`.
    pub name: String,
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
    /// A shell whose agent `TakeOver` waits on, until its turn ends (`CancelTakeOver` stops it).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub taking_over: bool,
    /// The agent's own conversation id, when dino knows it: one conversation is one session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
    /// Where a shell with shell integration says it is now (`cwd` is where it started), and the
    /// exit code of the last command it ran.
    #[serde(default)]
    pub shell_cwd: Option<String>,
    /// A shell running a command (not at its prompt), as last looked at: closing it would stop it.
    /// Never for a tmux client: closing that only detaches it.
    #[serde(default)]
    pub running: bool,
    /// What a shell runs in the foreground in place of its prompt (`vim`, `cargo`, a tmux client),
    /// as last looked at: the program closing it would stop. None at the prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreground: Option<ForegroundProcess>,
    /// Its terminal reads a password (echo off in canonical mode, as `sudo` and `ssh` ask for
    /// one): clients turn on secure keyboard entry. Local sessions only.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub password: bool,
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
    /// Answered by a route it fell back to, because the one it uses is spent (Settings → Agents).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<FallbackInfo>,
    /// What each route answered: its agent's own account, and any it fell back to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub usage_by_route: Vec<RouteUsageInfo>,
    /// Started with this agent instead of the one asked for, which was at its limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instead_of: Option<InsteadOf>,
    /// Its conversation is a fork of another session's: one dino forked (`Request::Fork`), or
    /// one the agent forked itself (Claude's `/branch`, Codex's `/fork`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forked_from: Option<ForkedFrom>,
}

/// The session a fork was made from.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct ForkedFrom {
    /// The session's id; it may since have been closed.
    pub session: String,
    /// What it was called when the fork was made.
    pub name: String,
    /// Its conversation, the one forked.
    pub conversation: String,
}

/// A session on a fallback route, and why.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct FallbackInfo {
    /// The provider answering, as Settings → Models & Providers knows it ("plan-zai", "ollama",
    /// "openrouter", "chatgpt", "free"), its name and the model.
    pub provider: String,
    pub name: String,
    pub model: String,
    /// The route it uses otherwise: "Claude", "GLM Coding Plan".
    pub from: String,
    /// "limit", "balance" or "outage".
    pub reason: String,
    /// What that route said.
    pub said: String,
    /// When it resets, in Unix seconds, when it said.
    pub resets_at: Option<u64>,
    /// When dino tries it again: the next turn after this.
    pub retry_at: Option<u64>,
    /// Since when, in Unix seconds.
    pub since: u64,
}

/// What one route answered for a session.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct RouteUsageInfo {
    /// Where dino's proxy serves it: "anthropic", "plan/zai", "local/ollama".
    pub route: String,
    pub name: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// The agent a session was asked for, at its limit when it started.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct InsteadOf {
    pub agent_id: String,
    /// The route that was spent: "Claude".
    pub name: String,
    pub resets_at: Option<u64>,
}

/// An agent whose route is spent: new sessions with it would hit the limit.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct AgentLimit {
    pub agent_id: String,
    /// The route that's spent ("Claude"), why ("limit", "balance") and in its words.
    pub name: String,
    pub reason: String,
    pub said: String,
    pub resets_at: Option<u64>,
    /// When dino tries it again.
    pub retry_at: u64,
    /// The agent new sessions start with meanwhile, and its model (Settings → Agents).
    pub instead: Option<String>,
    pub instead_model: Option<String>,
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
    /// Always false. Clients from before fan-out was removed require the field.
    #[serde(default)]
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
    /// What keeps the Mac from idle sleep now, from the system's power assertions: dinod's own
    /// first, then the rest by when they started. Empty from an older dinod.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub awake: Vec<AwakeHolder>,
}

/// One process holding a power assertion that keeps the Mac from idle sleep.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct AwakeHolder {
    /// The process the assertion is for: who asked, when another process holds it for them
    /// (`caffeinate -w`, a browser's audio).
    pub pid: u32,
    /// Its name (`caffeinate`, `Safari`, `dino`).
    pub process: String,
    /// What the assertion says it's for ("caffeinate command-line tool", "dino: 2 agents working").
    pub name: String,
    /// The assertion's type: `PreventUserIdleSystemSleep`, `PreventSystemSleep`,
    /// `PreventUserIdleDisplaySleep`.
    pub kind: String,
    /// When it was taken (unix seconds).
    pub since: Option<u64>,
    /// The dino session whose processes it's under, if it is.
    pub session: Option<String>,
    /// dinod's own, for working agents (`awake_while_working`), scheduled automations or the lid.
    pub ours: bool,
    /// Part of macOS (powerd while the display is on, Handoff…), not something you started.
    pub system: bool,
}

impl AwakeHolder {
    /// What sleep it keeps away, in a word or two.
    pub fn kind_label(&self) -> &str {
        match self.kind.as_str() {
            "PreventUserIdleSystemSleep" | "NoIdleSleepAssertion" => "idle sleep",
            "PreventSystemSleep" => "all sleep",
            "PreventUserIdleDisplaySleep" | "NoDisplaySleepAssertion" => "display sleep",
            k => k,
        }
    }
}

impl PowerInfo {
    /// What keeps the Mac awake now, in a line, or `None` when nothing you'd care about does:
    /// "Staying awake · 2 agents working", "Staying awake · caffeinate (Claude: Fix the build)",
    /// "Awake with the lid closed · 2 agents working". `session` names a session by its id.
    pub fn awake_line(&self, session: impl Fn(&str) -> Option<String>) -> Option<String> {
        let mut parts: Vec<String> = vec![];
        if let Some(o) = self.awake.iter().find(|h| h.ours) {
            parts.push(o.name.strip_prefix("dino: ").unwrap_or(&o.name).to_string());
        }
        // Those in a dino session first: they're about your agents.
        let mut others: Vec<&AwakeHolder> = self.awake.iter().filter(|h| !h.ours && !h.system).collect();
        others.sort_by_key(|h| h.session.as_deref().and_then(&session).is_none());
        let mut seen: Vec<String> = vec![];
        for h in &others {
            let said = match h.session.as_deref().and_then(&session) {
                Some(s) => format!("{} ({s})", h.process),
                None => h.process.clone(),
            };
            if !seen.contains(&said) {
                seen.push(said);
            }
        }
        // dinod's own reason, then one other or how many others.
        match (parts.is_empty(), seen.as_slice()) {
            (_, []) => {}
            (_, [one]) => parts.push(one.clone()),
            (false, many) => parts.push(format!("{} others", many.len())),
            (true, [first, rest @ ..]) => parts.extend([first.clone(), format!("{} more", rest.len())]),
        }
        let head = if self.holding { "Awake with the lid closed" } else { "Staying awake" };
        if parts.is_empty() {
            return self.holding.then(|| head.to_string());
        }
        Some(format!("{head} · {}", parts.join(" · ")))
    }
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

/// Your Claude accounts as dinod holds them: never their tokens.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct ClaudeAccountsInfo {
    /// Claude Code's own sign-in first (number 1), then your other accounts in the order they're tried.
    pub accounts: Vec<ClaudeAccountInfo>,
    /// The account `add` or `create` just added.
    #[serde(default)]
    pub added: Option<u32>,
    /// The shell running `claude setup-token`, while dinod waits for the token it prints.
    #[serde(default)]
    pub creating: Option<String>,
    /// What went wrong with the last `create`.
    #[serde(default)]
    pub error: Option<String>,
}

/// One of your Claude accounts.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct ClaudeAccountInfo {
    /// 1 for the account Claude Code signed in with, 2 on for the others (`CLAUDE_ACCOUNT_<n>`).
    pub number: u32,
    /// Claude Code's calls go to this one now: the first that isn't spent.
    pub answering: bool,
    /// Found at its subscription limit, and not tried again yet.
    pub spent: bool,
    /// When its limit resets (unix seconds), when Anthropic said so.
    #[serde(default)]
    pub resets_at: Option<u64>,
    /// When it's tried again (unix seconds): its reset, or a while after it was found spent.
    #[serde(default)]
    pub retry_at: Option<u64>,
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
    fn awake_line_says_what_keeps_the_mac_awake() {
        let h = |process: &str, name: &str, session: Option<&str>, ours: bool, system: bool| AwakeHolder {
            pid: 1,
            process: process.into(),
            name: name.into(),
            kind: "PreventUserIdleSystemSleep".into(),
            since: None,
            session: session.map(String::from),
            ours,
            system,
        };
        let names = |id: &str| (id == "3").then(|| "Claude: Retry with backoff".to_string());
        let mut p = PowerInfo::default();
        assert_eq!(p.awake_line(names), None);
        // macOS's own (the display is on) isn't worth a line.
        p.awake = vec![h("powerd", "Powerd - Prevent sleep while display is on", None, false, true)];
        assert_eq!(p.awake_line(names), None);
        p.awake.push(h("caffeinate", "caffeinate command-line tool", Some("3"), false, false));
        assert_eq!(p.awake_line(names).as_deref(), Some("Staying awake · caffeinate (Claude: Retry with backoff)"));
        p.awake.insert(0, h("dino", "dino: 2 agents working", None, true, false));
        assert_eq!(p.awake_line(names).as_deref(), Some("Staying awake · 2 agents working · caffeinate (Claude: Retry with backoff)"));
        // Claude Code's caffeinates come one after another: the same holder says it once.
        p.awake.push(h("caffeinate", "caffeinate command-line tool", Some("3"), false, false));
        p.awake.push(h("Safari", "Playing audio", None, false, false));
        assert_eq!(p.awake_line(names).as_deref(), Some("Staying awake · 2 agents working · 2 others"));
        p.awake.remove(0);
        assert_eq!(p.awake_line(names).as_deref(), Some("Staying awake · caffeinate (Claude: Retry with backoff) · 1 more"));
        // One in a session comes first, whenever it started.
        p.awake.insert(0, h("caffeinate", "caffeinate command-line tool", None, false, false));
        assert_eq!(p.awake_line(names).as_deref(), Some("Staying awake · caffeinate (Claude: Retry with backoff) · 2 more"));
        p.awake.clear();
        p.holding = true;
        assert_eq!(p.awake_line(names).as_deref(), Some("Awake with the lid closed"));
        p.awake = vec![h("dino", "dino: an agent working", None, true, false)];
        assert_eq!(p.awake_line(names).as_deref(), Some("Awake with the lid closed · an agent working"));
        // An older dinod sends no list, and a newer client reads it as empty.
        let old: PowerInfo = serde_json::from_str(r#"{"holding":false,"since":null,"external":false,"note":null,"note_at":null,"ready":null,"error":null}"#).unwrap();
        assert!(old.awake.is_empty());
    }

    #[test]
    fn session_cost_takes_older_and_newer_dinods() {
        // An older dinod's answer, missing fields: defaults.
        let old: Response = serde_json::from_str(r#"{"type":"session_cost","cost":{"mem_bytes":5}}"#).unwrap();
        let Response::SessionCost { cost } = old else { panic!() };
        assert_eq!((cost.mem_bytes, cost.cpu_pct, cost.top_child), (5, 0.0, None));
        // A newer one's fields come back out as they went in.
        let json = r#"{"mem_bytes":1,"cpu_pct":2.5,"cpu_avg_pct":1.0,"avg_secs":30,"processes":3,"top_child":{"name":"rustc","pid":9,"mem_bytes":4,"cpu_pct":85.0,"gpu":7},"energy":12}"#;
        let cost: SessionCost = serde_json::from_str(json).unwrap();
        assert_eq!(cost.top_child.as_ref().unwrap().name, "rustc");
        let back: serde_json::Value = serde_json::to_value(&cost).unwrap();
        assert_eq!(back, serde_json::from_str::<serde_json::Value>(json).unwrap());
        let req: Request = serde_json::from_str(r#"{"type":"session_cost","id":"4"}"#).unwrap();
        assert!(matches!(req, Request::SessionCost { id } if id == "4"));
    }

    #[test]
    fn version_says_the_build_and_binary_when_it_knows_them() {
        // An older dinod says only its version: no build, no binary.
        let old: Response = serde_json::from_str(r#"{"type":"version","dino":"0.1.3","installed":null}"#).unwrap();
        assert!(matches!(old, Response::Version { build: None, exe: None, .. }));
        let v = Response::Version { dino: "0.1.4".into(), installed: None, launchd: None, build: Some("c1ddaea2f".into()), exe: Some("/Applications/Dino.app/Contents/Helpers/dino".into()) };
        let json = serde_json::to_string(&v).unwrap();
        assert_eq!(json, r#"{"type":"version","dino":"0.1.4","installed":null,"build":"c1ddaea2f","exe":"/Applications/Dino.app/Contents/Helpers/dino"}"#);
        // A plain `cargo build` has no build to say: the field isn't sent.
        let plain = Response::Version { dino: "0.1.4".into(), installed: None, launchd: None, build: None, exe: None };
        assert_eq!(serde_json::to_string(&plain).unwrap(), r#"{"type":"version","dino":"0.1.4","installed":null}"#);
    }

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
