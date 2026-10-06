import Foundation

/// dinod's wire types (see crates/dino-core/src/ipc.rs).
struct SessionInfo: Codable, Identifiable, Equatable {
    var id: String
    var name: String
    var agent_id: String
    var title: String?
    var exited: Bool
    /// How its program exited, once it has; nil from an older dinod.
    var exit_code: UInt32?
    var output_ms_ago: UInt64?
    var bells: UInt64
    var requests: UInt64
    var in_flight: UInt32
    /// Everything its calls read (cache reads included) and wrote.
    var input_tokens: UInt64
    var output_tokens: UInt64
    /// Of `input_tokens`, what was read again from the prompt cache; nil from an older dinod.
    var cache_read_tokens: UInt64?
    var last_model: String?
    var tier: String?
    var activity: String?
    /// Why the agent's last model call failed.
    var error: String?
    /// Where it runs, symlinks resolved; nil from an older dinod.
    var cwd: String?
    /// The pull request for its branch, whoever opened it.
    var pr: PrInfo?
    /// What dino does about the PR by itself; nil from an older dinod.
    var auto: AutoPr?
    /// Dev servers started for its preview; nil from an older dinod.
    var previews: [PreviewInfo]?
    /// The last local web address it printed, to offer a preview of.
    var local_url: String?
    /// The mode, model and effort it runs with; nil from an older dinod.
    var controls: Controls?
    /// Asked for mid-turn; applied (by a restart) once the turn is over.
    var pending: Controls?
    /// The permission mode the agent says it's in; differs from `controls` after Claude's Shift+Tab.
    var agent_mode: String?
    /// The model the agent says it's on, once it has said; differs from what it was started with
    /// after `/model` in it. Nil from an older dinod.
    var agent_model: String?
    /// How full the context window is: tokens the last model call read, and the window's size.
    var context_tokens: UInt64?
    var context_limit: UInt64?
    /// The scheduled task that started it.
    var scheduled: String?
    /// The name the user gave it; `title` is this too while it's set.
    var label: String?
    /// Kept at the top of its group and out of dino's own archiving; nil from an older dinod.
    var pinned: Bool?
    /// A shell's "Keep as terminal": agents typed into it don't report to dino; nil from an older dinod.
    var keep_terminal: Bool?
    /// Its task list, subagents and background commands, from its hooks; nil from an older dinod.
    var tasks: SessionTasks?
    /// The session whose agent started it, and the one that last messaged it (through `dino mcp`).
    var started_by: String?
    var messaged_by: String?
    /// The SSH host it runs on (`cwd` is then a path there); nil for this Mac.
    var host: String?
    /// A shell's: the agent typed into it, while it runs. It's the session's agent then, as one
    /// dino started would be (see `agent`).
    var inside: FoundSession?
    /// The agent's own conversation id: one conversation is one row.
    var conversation: String?
    /// A shell's, from its shell integration: where it is now (`cwd` is where it started), and
    /// how its last command ended.
    var shell_cwd: String?
    /// When `dino <folder>` asked for it to be shown, in ms since the epoch.
    var revealed: UInt64?
    /// A shell running a command rather than sitting at its prompt.
    var running: Bool?
    /// What a shell runs in place of its prompt (`vim`), as dinod last looked; nil at the prompt
    /// and from an older dinod.
    var foreground: ForegroundProcess?
    /// A shell whose foreground is a tmux client: what it shows. Closing the tab only detaches it.
    var tmux: TmuxPane?
    var last_exit: Int?
    /// Background commands its agent left serving (a dev server); nil from an older dinod.
    var servers: [ServerInfo]?
    /// The provider and model it runs on, when it isn't its agent's own account.
    var route: ProviderRoute?
    /// What its agent uses outside its terminal right now, by its tool calls: "computer" (apps on
    /// this Mac) or "browser"; nil from an older dinod.
    var using: String?
    /// Answered by a route it fell back to while the one it uses is spent; nil when it isn't.
    var fallback: FallbackInfo?
    /// What each route answered: its agent's own account and any it fell back to; nil from an older dinod.
    var usage_by_route: [RouteUsage]?
    /// Started with this agent because the one asked for was at its limit.
    var instead_of: InsteadOf?
    /// Its terminal reads a password (echo off in line mode), as of its last output or keystroke;
    /// nil when not, and from an older dinod.
    var password: Bool?
    /// The session its conversation was forked from: by dino (Fork Session), or in the agent
    /// (Claude's /branch, Codex's /fork).
    var forked_from: ForkedFrom?

    /// All its turn left running is a server: the ports it listens on ("3000, 8080").
    var serving: String? {
        guard let a = activity, a.hasPrefix("server:") else { return nil }
        return String(a.dropFirst(7))
    }

    var needs: String? {
        guard let a = activity, a.hasPrefix("needs:") else { return nil }
        return String(a.dropFirst(6))
    }

    /// Its turn ended on work that still runs: "1 agent", "2 commands", "1 agent, 1 command".
    var waitingOn: String? {
        guard let a = activity, a.hasPrefix("waiting:") else { return nil }
        return String(a.dropFirst(8))
    }
}

/// A session on a fallback route, and why (see crates/dino-core/src/ipc.rs).
struct FallbackInfo: Codable, Equatable {
    /// The provider answering ("plan-zai", "ollama"), its name and model.
    var provider: String
    var name: String
    var model: String
    /// The route it uses otherwise: "Claude"; for "unavailable", the model asked for.
    var from: String
    /// "limit", "balance", "outage", or "unavailable": ChatGPT rejects the Codex model asked for
    /// (`from`), and another the account lists answers (`model`).
    var reason: String
    var said: String
    var resets_at: UInt64?
    var retry_at: UInt64?
    var since: UInt64

    /// Answered by another of the user's Claude accounts ("Claude account 2"), not a fallback route.
    var isAccount: Bool { provider == "anthropic" && !isModel }

    /// Answered by another model of the same account, the one asked for being rejected.
    var isModel: Bool { reason == "unavailable" }

    /// What the session shows: "On fallback: GLM Coding Plan · Claude limit resets 14:00", for
    /// another Claude account "On Claude account 2 · account 1 back at 14:00" (the reset is the
    /// spent account's, Claude Code's own sign-in), for another model "gpt-5.5 unavailable ·
    /// using gpt-5.4".
    var label: String {
        if isModel { return "\(why) · using \(model)" }
        guard isAccount else { return "On fallback: \(name) · \(why)" }
        if let t = resets_at { return "On \(name) · account 1 back at \(Clock.short(t))" }
        return "On \(name) · account 1 at its limit"
    }

    /// The sidebar row's, short: "On Claude account 3"; the footer says when account 1 is back.
    var rowLabel: String { isAccount ? "On \(name)" : label }

    /// "Claude limit resets 14:00", "GLM out of balance", "Claude down".
    var why: String {
        switch reason {
        case "balance": return "\(from) out of balance"
        case "outage": return "\(from) down"
        case "unavailable": return "\(from) unavailable"
        default:
            if let t = resets_at { return "\(from) limit resets \(Clock.short(t))" }
            return "\(from) at its limit"
        }
    }
}

/// What one route answered for a session.
struct RouteUsage: Codable, Equatable {
    var route: String
    var name: String
    var input_tokens: UInt64
    var output_tokens: UInt64
}

/// The agent a session was asked for, at its limit when it started.
/// The session a fork was made from (see crates/dino-core/src/ipc.rs).
struct ForkedFrom: Codable, Equatable {
    /// Its id; it may since have been closed.
    var session: String
    /// What it was called when the fork was made.
    var name: String
    var conversation: String
}

struct InsteadOf: Codable, Equatable {
    var agent_id: String
    /// The route that was spent: "Claude".
    var name: String
    var resets_at: UInt64?
}

/// An agent at its limit, and what new sessions start with meanwhile.
struct AgentLimit: Codable, Equatable {
    var agent_id: String
    var name: String
    var reason: String
    var said: String
    var resets_at: UInt64?
    var retry_at: UInt64
    var instead: String?
    var instead_model: String?

    /// "Claude is at its limit until 14:00".
    var sentence: String {
        let until = resets_at.map { " until \(Clock.short($0))" } ?? ""
        return reason == "balance" ? "\(name) is out of balance" : "\(name) is at its limit\(until)"
    }
}

/// Times as the Mac shows them: "14:00" today, "Mon 14:00" within a week, else the date.
enum Clock {
    static func short(_ unix: UInt64) -> String {
        let date = Date(timeIntervalSince1970: TimeInterval(unix))
        let cal = Calendar.current
        let f = DateFormatter()
        if cal.isDateInToday(date) {
            f.timeStyle = .short
            f.dateStyle = .none
        } else if date.timeIntervalSinceNow < 6 * 86_400 {
            f.setLocalizedDateFormatFromTemplate("EEE jj:mm")
        } else {
            f.dateStyle = .medium
            f.timeStyle = .short
        }
        return f.string(from: date)
    }
}

/// A background command that listens on a port (see crates/dino-core/src/ipc.rs).
struct ServerInfo: Codable, Equatable, Hashable {
    var task: String
    var command: String
    var ports: [Int]

    var where_: String { ports.map { ":\($0)" }.joined(separator: " ") }
}

/// What an agent tracks underneath its conversation (see crates/dino-core/src/ipc.rs).
struct SessionTasks: Codable, Equatable {
    var todos: [TodoItem]
    var subagents: [SubagentItem]
    var background: [BackgroundItem]

    var isEmpty: Bool { todos.isEmpty && subagents.isEmpty && background.isEmpty }
    /// Subagents and background commands still going.
    var running: Int { subagents.filter(\.running).count + background.filter(\.running).count }
}

struct TodoItem: Codable, Equatable, Identifiable {
    var id: String
    var subject: String
    /// "pending", "in_progress" or "completed".
    var status: String
    /// Shown while it's in progress ("Running the tests").
    var active: String?
}

struct SubagentItem: Codable, Equatable, Identifiable {
    var id: String
    var agent_type: String?
    var description: String?
    var running: Bool
    /// Unix seconds; 0 when dino didn't see it start.
    var started: UInt64
    var finished: UInt64?
    /// Its own worktree, when it runs in one.
    var worktree: String?
}

struct BackgroundItem: Codable, Equatable, Identifiable {
    var id: String
    /// "shell" or "monitor".
    var kind: String
    var description: String?
    var command: String?
    var running: Bool
    var started: UInt64
    var finished: UInt64?
}

/// A GitHub pull request, as `gh pr view` sees it (see crates/dino-core/src/pr.rs).
struct PrInfo: Codable, Equatable {
    var number: UInt32
    var url: String
    var title: String
    /// "open", "merged" or "closed".
    var state: String
    var draft: Bool
    var checks: Checks
    /// "approved", "changes_requested" or "review_required".
    var review: String?

    /// The head commit.
    var head: String?

    var isOpen: Bool { state == "open" }
    var canMerge: Bool { isOpen && !draft && checks.failed == 0 && checks.pending == 0 }
}

/// Automatic steps on a session's PR (see `AutoPr` in crates/dino-core/src/ipc.rs).
struct AutoPr: Codable, Equatable {
    /// Ask the agent to fix failing checks, up to three times.
    var fix: Bool
    /// Squash-merge once checks pass.
    var merge: Bool
    /// Fixes asked for so far.
    var fixes: UInt32
    /// Why the last automatic step failed.
    var note: String?

    var any: Bool { fix || merge }
}

/// CI checks on a PR; all zero when the repo has none.
struct Checks: Codable, Equatable {
    var passed: UInt32
    var failed: UInt32
    var pending: UInt32
    /// Names of the checks that failed.
    var failing: [String]
}

/// What the Create PR sheet starts from.
struct PrDraft: Codable, Equatable {
    var branch: String
    var base: String
    var title: String
    var body: String
    /// Changed files not committed yet: committed with the title as the message first.
    var uncommitted: UInt32
    var commits: UInt32
    /// Why a PR can't be made from here.
    var note: String?
}

private struct PrDraftResponse: Decodable {
    var draft: PrDraft
}

private struct PrResponse: Decodable {
    var pr: PrInfo
}

struct WindowInfo: Codable, Equatable {
    var name: String
    var utilization: Float
    var resets_at: UInt64?

    /// A plan's window ("5h", "7d"), not another figure of the provider's.
    var isWindow: Bool { name.hasSuffix("h") || name.hasSuffix("d") }
    /// Ended since it was reported: its use now isn't known until a call reports it again.
    var isPast: Bool { resets_at.map { TimeInterval($0) <= Date().timeIntervalSince1970 } ?? false }
}

struct QuotaInfo: Codable, Equatable {
    var provider: String
    var windows: [WindowInfo]
}

struct LauncherInfo: Codable, Identifiable, Equatable {
    var short: String
    var agent_id: String
    var label: String
    var program: String
    /// The mode, model and effort it offers; nil from an older dinod.
    var knobs: Knobs?
    /// It can answer the shell's ⌘I with no tools; nil from an older dinod.
    var answers_once: Bool?
    /// The APIs it talks to a provider's model in ("anthropic", "chat", "responses"); nil from an older dinod.
    var formats: [String]?
    /// Its conversations can be forked, by the agent's own fork; nil from an older dinod.
    var forks: Bool?
    var id: String { short }
}

/// A session's permission mode, model and effort (see crates/dino-core/src/controls.rs).
/// Nil is the agent's own default.
/// A session on a provider's model (Settings → Models & Providers) instead of its agent's own account.
struct ProviderRoute: Codable, Equatable, Hashable {
    var provider: String
    var model: String
    /// "anthropic", "chat", "responses": how the agent and the provider talk.
    var format: String?
    /// The provider as people know it: "Ollama".
    var name: String?

    /// "Ollama · qwen3:4b".
    var label: String { "\(name ?? provider) · \(model)" }
}

struct Controls: Codable, Equatable, Hashable {
    var mode: String?
    var model: String?
    var effort: String?

    var json: [String: Any] {
        var d: [String: Any] = [:]
        if let mode { d["mode"] = mode }
        if let model { d["model"] = model }
        if let effort { d["effort"] = effort }
        return d
    }
}

/// One model an agent lists, as its own files say (see crates/dino-core/src/models.rs).
struct ModelInfo: Codable, Equatable {
    var id: String
    var label: String
    /// Effort levels it takes, lowest first; empty when it has none.
    var efforts: [String]
    var default_effort: String?
    /// Listed after the main ones, under this heading.
    var group: String?
    /// Other names the agent takes for it ("haiku").
    var aliases: [String]

    func named(_ name: String) -> Bool {
        id == name || aliases.contains { $0.caseInsensitiveCompare(name) == .orderedSame }
    }
}

/// What a launcher lets you choose; empty lists mean it has no such control.
struct Knobs: Codable, Equatable {
    var modes: [String]
    var model: Bool
    /// The models the agent lists; empty when it keeps no list, and the model is typed.
    var models: [ModelInfo]
    /// The model the agent starts with when none is chosen.
    var default_model: String?
    /// Every effort its models take, lowest first.
    var efforts: [String]
    /// Changing a control restarts the agent, resuming its conversation.
    var restart: Bool
    /// What the agent itself calls each mode ("Manual" for Claude's ask); nil from an older dinod.
    var mode_labels: [String: String]?
    /// What a conversation it resumes keeps as it was ("model", a mode id): fixed for a running
    /// session, open for a new one. Nil from an older dinod.
    var resume_keeps: [String]? = nil
    /// Its mode switches in place (Claude's Shift+Tab) where it can, rather than by restarting it.
    /// Nil from an older dinod.
    var live_modes: Bool? = nil

    /// A running session can't change `what` (see `resume_keeps`).
    func keeps(_ what: String) -> Bool { resume_keeps?.contains(what) ?? false }

    var any: Bool { !modes.isEmpty || model || !efforts.isEmpty }

    static let none = Knobs(modes: [], model: false, models: [], default_model: nil, efforts: [], restart: false)

    /// A mode in the agent's own words, else dino's.
    func modeLabel(_ id: String?) -> String {
        id.flatMap { mode_labels?[$0] } ?? Mode.label(id)
    }

    /// `name`'s entry: a model id or one of its aliases.
    func listed(_ name: String?) -> ModelInfo? {
        name.flatMap { n in models.first { $0.named(n) } }
    }

    /// The listed id `name` stands for ("haiku" is claude-haiku-4-5-…), else `name` itself.
    func canonical(_ name: String) -> String {
        listed(name)?.id ?? name
    }

    /// How to show a model: its listed name, else shortened. A variant of a listed one, as the
    /// agent names it (Claude's `claude-opus-5-5[1m]`), is that one's name and the variant.
    func label(_ name: String) -> String {
        if let m = listed(name) { return m.label }
        if name.hasSuffix("]"), let open = name.lastIndex(of: "["), let base = listed(String(name[..<open])) {
            return "\(base.label) \(name[open...])"
        }
        return shortModel(name)
    }

    /// The efforts `model` takes (the agent's default when nil); all of them for one it doesn't list.
    func efforts(for model: String?) -> [String] {
        listed(model ?? default_model)?.efforts ?? efforts
    }

    /// `effort` kept for `model`: as is when it takes it, else the nearest level below that it does.
    func clamp(_ effort: String?, for model: String?) -> String? {
        guard let effort else { return nil }
        let takes = efforts(for: model)
        if takes.contains(effort) { return effort }
        guard let wanted = efforts.firstIndex(of: effort) else { return nil }
        return takes.filter { (efforts.firstIndex(of: $0) ?? .max) < wanted }.last
    }
}

extension Knobs {
    /// Leniently: a dinod from before model lists sent names, not entries.
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        modes = try c.decodeIfPresent([String].self, forKey: .modes) ?? []
        model = try c.decodeIfPresent(Bool.self, forKey: .model) ?? false
        models = (try? c.decodeIfPresent([ModelInfo].self, forKey: .models)) ?? []
        default_model = try c.decodeIfPresent(String.self, forKey: .default_model)
        efforts = try c.decodeIfPresent([String].self, forKey: .efforts) ?? []
        restart = try c.decodeIfPresent(Bool.self, forKey: .restart) ?? false
        mode_labels = try? c.decodeIfPresent([String: String].self, forKey: .mode_labels)
        resume_keeps = try? c.decodeIfPresent([String].self, forKey: .resume_keeps)
        live_modes = try? c.decodeIfPresent(Bool.self, forKey: .live_modes)
    }
}

/// Neutral permission modes: dino maps each to the agent's own flags.
enum Mode {
    static let all: [(id: String, label: String, help: String)] = [
        ("ask", "Ask", "Asks before editing files or running commands"),
        ("edits", "Accept edits", "Edits files without asking; asks before commands"),
        ("plan", "Plan", "Reads and plans; changes nothing"),
        ("auto", "Auto", "The agent decides what's safe to do without asking"),
        ("bypass", "Bypass", "Never asks. Use only in a sandbox you trust"),
    ]
    static func label(_ id: String?) -> String {
        guard let id else { return "Default" }
        return all.first { $0.id == id }?.label ?? id
    }
    static func help(_ id: String) -> String { all.first { $0.id == id }?.help ?? "" }
}

/// A session dino didn't start: running in another terminal, recent on disk, or in the cloud.
struct FoundSession: Codable, Identifiable, Equatable {
    var source: String
    var agent: String
    var session_id: String
    var title: String
    var cwd: String?
    var updated_at: UInt64
    var pid: UInt32?
    var status: String?
    var terminal: String?
    var args: [String]
    var url: String?
    /// Running in a tmux pane: where. tmux owns it; dino watches, and can show it there.
    var tmux: TmuxPlace?
    /// Running, but dino can't tell which conversation it's on (Codex's shared server runs every
    /// terminal's Codex, and more than one fits): nothing to continue.
    var unsure: Unsure?

    var id: String { "\(source)-\(agent)-\(session_id)-\(pid ?? 0)" }
    var isBusy: Bool { status == "busy" || status == "needs" }
    /// Asking for something (a permission) in its tmux pane.
    var asking: Bool { status == "needs" }
    var agentName: String { AgentNames.of(agent) }
    /// The agent, whether or not it ran on the free tier ("kimi-free" is Kimi).
    var baseAgent: String { agent.hasSuffix("-free") ? String(agent.dropLast(5)) : agent }
}

/// What each agent dino can continue is called, short ("Copilot"), by its id; free-tier ones
/// ("kimi-free") as their agent.
enum AgentNames {
    static let short = ["claude": "Claude", "codex": "Codex", "qwen": "Qwen", "kimi": "Kimi", "pi": "Pi", "hermes": "Hermes", "codewhale": "CodeWhale", "opencode": "OpenCode", "copilot": "Copilot", "cursor": "Cursor", "amp": "Amp"]

    static func of(_ agent: String) -> String {
        short[agent.hasSuffix("-free") ? String(agent.dropLast(5)) : agent] ?? agent
    }
}

/// Why dino can't tell which conversation a running agent is on, and the ones it may be on.
struct Unsure: Codable, Equatable {
    var why: String
    var maybe: [String]
}

/// The tmux pane an agent runs in.
struct TmuxPlace: Codable, Equatable {
    var socket: String
    /// The pane's id (`%3`): stays the same as windows and panes move.
    var pane: String
    /// `session:window.pane`, as it is now.
    var target: String
    /// `session:window name`.
    var label: String
    /// A client is attached to its session.
    var attached: Bool
}

/// Part of a conversation, oldest first. `start` is where it begins in its file; 0 means the beginning.
struct ConversationPage: Codable, Equatable {
    var turns: [ConversationTurn]
    var start: UInt64
    var path: String?
}

struct DiffStat: Codable, Equatable {
    var files: UInt32
    var added: UInt32
    var removed: UInt32
}

/// A session's changes, per file (see worktree::changes).
struct Changes: Codable, Equatable {
    /// The checkout the paths are in, and what they're compared with, in words.
    var root: String
    var base: String
    var files: [FileDiff]
    /// Why there's nothing to show (not a git repo).
    var note: String?
}

struct FileDiff: Codable, Equatable, Identifiable {
    var path: String
    var old_path: String?
    var status: String
    var added: UInt32
    var removed: UInt32
    var binary: Bool
    var lines: [DiffLine]
    var truncated: Bool
    var id: String { path }
}

struct DiffLine: Codable, Equatable {
    /// "hunk", "add", "del" or "ctx".
    var kind: String
    var old: UInt32?
    var new: UInt32?
    var text: String
}

/// An issue Claude's review found; `line` is in the new version of `file`.
struct Finding: Codable, Equatable {
    var file: String
    var line: UInt32
    /// "high", "medium" or "low".
    var severity: String
    var message: String
}

/// A dev server from the session folder's launch.json (see crates/dino-core/src/preview.rs).
struct PreviewConfig: Codable, Equatable, Identifiable {
    var name: String
    var argv: [String]
    var cwd: String
    /// Variables set for it, on top of dinod's own.
    var env: [String: String]?
    var port: UInt16?
    var url: String?
    /// The launch file it came from, relative to the session's folder.
    var source: String
    var id: String { name }
}

/// A dev server dinod started for a session's preview.
struct PreviewInfo: Codable, Equatable, Identifiable {
    var name: String
    var running: Bool
    /// The page to load, once its port is known.
    var url: String?
    /// How it ended, if it has.
    var exit: String?
    var id: String { name }
}

private struct PreviewConfigsResponse: Decodable {
    var configs: [PreviewConfig]
    var error: String?
}

private struct PreviewLogResponse: Decodable {
    var text: String
}

private struct ReviewResponse: Decodable {
    var findings: [Finding]
}

private struct TmuxShownResponse: Decodable {
    var tty: String?
    var session: String?
}

/// The program that has a session's terminal, when it isn't the session's own (a shell's prompt).
struct ForegroundProcess: Codable, Equatable {
    var pid: UInt32
    var name: String
}

private struct ForegroundResponse: Decodable {
    var foreground: ForegroundProcess?
}

private struct TextResponse: Decodable {
    var text: String
}

private struct FoundResponse: Decodable {
    var sessions: [FoundSession]
}

private struct ConversationResponse: Decodable {
    var page: ConversationPage
}

private struct StatsResponse: Decodable {
    var report: StatsReport
}

struct Response: Decodable {
    var type: String
    var sessions: [SessionInfo]?

    enum CodingKeys: String, CodingKey { case type, sessions, quotas, power, limits, leftovers, claude_accounts, launchers, id, message, version, dino, installed, launchd, build, exe }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        type = try c.decode(String.self, forKey: .type)
        // `sessions` means SessionInfo only in a state reply.
        sessions = type == "state" ? try c.decodeIfPresent([SessionInfo].self, forKey: .sessions) : nil
        quotas = try c.decodeIfPresent([QuotaInfo].self, forKey: .quotas)
        power = try c.decodeIfPresent(PowerInfo.self, forKey: .power)
        limits = try c.decodeIfPresent([AgentLimit].self, forKey: .limits)
        leftovers = try c.decodeIfPresent([Leftover].self, forKey: .leftovers)
        claude_accounts = try c.decodeIfPresent([ClaudeAccountInfo].self, forKey: .claude_accounts)
        launchers = try c.decodeIfPresent([LauncherInfo].self, forKey: .launchers)
        id = try c.decodeIfPresent(String.self, forKey: .id)
        message = try c.decodeIfPresent(String.self, forKey: .message)
        version = try c.decodeIfPresent(UInt64.self, forKey: .version)
        dino = try c.decodeIfPresent(String.self, forKey: .dino)
        installed = try c.decodeIfPresent(String.self, forKey: .installed)
        launchd = try c.decodeIfPresent(String.self, forKey: .launchd)
        build = try c.decodeIfPresent(String.self, forKey: .build)
        exe = try c.decodeIfPresent(String.self, forKey: .exe)
    }
    var quotas: [QuotaInfo]?
    var power: PowerInfo?
    /// Agents at their limit; nil when none is (and from an older dinod).
    var limits: [AgentLimit]?
    /// Builds running for no session, found as dinod started; nil when none are (and from an older dinod).
    var leftovers: [Leftover]?
    /// Your Claude accounts with their windows, once there's more than one; nil with one, and from
    /// an older dinod.
    var claude_accounts: [ClaudeAccountInfo]?
    var launchers: [LauncherInfo]?
    var id: String?
    var message: String?
    /// Tags a state reply to a `state_change` request: the `seen` of the next one.
    var version: UInt64?
    /// A version reply: which dino the running dinod is, and a newer one it installed.
    var dino: String?
    var installed: String?
    /// The launch agent running dinod (its label), if launchd started it; nil from an older dinod.
    var launchd: String?
    /// Which build of dino it is (the commit), and the binary it runs from; nil from an older dinod.
    var build: String?
    var exe: String?
}

/// Keeping agents running with the lid closed, as dinod sees it.
struct PowerInfo: Codable, Equatable {
    var holding: Bool
    var since: UInt64?
    /// Sleep is off, but someone else turned it off: dino leaves it alone.
    var external: Bool
    /// Why sleep came back last time, when it's worth saying.
    var note: String?
    var note_at: UInt64?
    /// The one-time permission is in place; only in a `power` reply.
    var ready: Bool?
    var error: String?
    /// What keeps the Mac from idle sleep now, dinod's own first; nil from an older dinod.
    var awake: [AwakeHolder]?
}

/// A process holding a power assertion that keeps the Mac awake.
struct AwakeHolder: Codable, Equatable, Identifiable {
    var pid: UInt32
    /// `caffeinate`, `Google Chrome`, `dino`.
    var process: String
    /// What the assertion says it's for.
    var name: String
    var kind: String
    var since: UInt64?
    /// The session whose processes it's under.
    var session: String?
    /// dinod's own.
    var ours: Bool
    /// Part of macOS, not something you started.
    var system: Bool

    var id: String { "\(pid) \(kind) \(name) \(since ?? 0)" }

    /// What sleep it keeps away, in a word or two.
    var kindLabel: String {
        switch kind {
        case "PreventUserIdleSystemSleep", "NoIdleSleepAssertion": "Idle sleep"
        case "PreventSystemSleep": "All sleep"
        case "PreventUserIdleDisplaySleep", "NoDisplaySleepAssertion": "Display sleep"
        default: kind
        }
    }
}

extension PowerInfo {
    /// What keeps the Mac awake now, in a line, or nil when nothing worth saying does; as
    /// `PowerInfo::awake_line` in dino-core, which `dino status` prints.
    func awakeLine(session: (String) -> String?) -> String? {
        let holders = awake ?? []
        var parts: [String] = []
        if let o = holders.first(where: \.ours) {
            parts.append(o.name.hasPrefix("dino: ") ? String(o.name.dropFirst(6)) : o.name)
        }
        var seen: [String] = []
        // Those in a dino session first: they're about your agents.
        let others = holders.filter { !$0.ours && !$0.system }
        for h in others.filter({ $0.session.flatMap(session) != nil }) + others.filter({ $0.session.flatMap(session) == nil }) {
            let said = h.session.flatMap(session).map { "\(h.process) (\($0))" } ?? h.process
            if !seen.contains(said) { seen.append(said) }
        }
        switch (parts.isEmpty, seen.count) {
        case (_, 0): break
        case (_, 1): parts.append(seen[0])
        case (false, let n): parts.append("\(n) others")
        case (true, let n): parts += [seen[0], "\(n - 1) more"]
        }
        let head = holding ? "Awake with the lid closed" : "Staying awake"
        if parts.isEmpty { return holding ? head : nil }
        return ([head] + parts).joined(separator: " · ")
    }
}

private struct PowerResponse: Decodable {
    var power: PowerInfo
}

extension DinoConnection {
    /// `status`, `setup` (macOS asks for an administrator's password) or `remove`.
    func power(_ action: String) throws -> PowerInfo {
        try JSONDecoder().decode(PowerResponse.self, from: send(["type": "power", "action": action])).power
    }
}

enum DinoError: Error, LocalizedError {
    case socket(String)
    case daemon(String)
    var errorDescription: String? {
        switch self {
        case let .socket(m), let .daemon(m): m
        }
    }
}

/// One connection to dinod speaking the framed protocol: `[kind u8][len u32 BE][payload]`.
final class DinoConnection: @unchecked Sendable {
    private let fd: Int32
    /// One request at a time: the poller and user actions share this connection.
    private let lock = NSLock()

    init(path: String) throws {
        fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { throw DinoError.socket("socket() failed") }
        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let bytes = Array(path.utf8CString)
        guard bytes.count <= MemoryLayout.size(ofValue: addr.sun_path) else { throw DinoError.socket("socket path too long") }
        withUnsafeMutableBytes(of: &addr.sun_path) { dst in
            bytes.withUnsafeBytes { dst.copyMemory(from: $0) }
        }
        let ok = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { connect(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size)) }
        }
        guard ok == 0 else {
            close(fd)
            throw DinoError.socket("Can't connect to dino's background service at \(path).")
        }
    }

    deinit { close(fd) }

    /// Give up on an answer, or on sending, after `seconds`: for a question the UI waits on.
    func timeout(_ seconds: Double) {
        var tv = timeval(tv_sec: Int(seconds), tv_usec: Int32((seconds - Double(Int(seconds))) * 1_000_000))
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
        setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
    }

    func request(_ body: [String: Any]) throws -> Response {
        try JSONDecoder().decode(Response.self, from: send(body))
    }

    /// `runningOnly` skips finished conversations on disk (cheap enough to poll).
    /// Bring a tmux pane to the front in the client attached to its server: that client's terminal,
    /// and the dino tab running it if one does; neither when nobody is attached.
    func tmuxShow(_ place: TmuxPlace) throws -> (tty: String?, session: String?) {
        let r = try JSONDecoder().decode(TmuxShownResponse.self, from: send(["type": "tmux_show", "socket": place.socket, "pane": place.pane]))
        return (r.tty, r.session)
    }

    /// What a tmux pane shows, as text.
    func tmuxScreen(_ place: TmuxPlace) throws -> String {
        try JSONDecoder().decode(TextResponse.self, from: send(["type": "tmux_screen", "socket": place.socket, "pane": place.pane])).text
    }

    func found(cloud: Bool, runningOnly: Bool = false) throws -> [FoundSession] {
        try JSONDecoder().decode(FoundResponse.self, from: send(["type": "found", "cloud": cloud, "running_only": runningOnly])).sessions
    }

    /// Part of a conversation (a found session's, or a Claude subagent's by its id), ending at byte
    /// `before` of its file (default: the end).
    func conversation(agent: String, sessionID: String, before: UInt64? = nil) throws -> ConversationPage {
        var body: [String: Any] = ["type": "conversation", "agent": agent, "session_id": sessionID]
        if let before { body["before"] = before }
        return try JSONDecoder().decode(ConversationResponse.self, from: send(body)).page
    }

    /// Usage statistics for `range` ("7d", "30d", "all"). dinod reads what's new in agents' own
    /// records first, which can take a few seconds the first time: call it off the main thread.
    func stats(range: String) throws -> StatsReport {
        try JSONDecoder().decode(StatsResponse.self, from: send(["type": "stats", "range": range])).report
    }

    /// Forget every usage statistic dino has kept.
    func clearStats() throws {
        _ = try send(["type": "stats_clear"])
    }

    func changes(session: String) throws -> Changes {
        try JSONDecoder().decode(Changes.self, from: send(["type": "changes", "id": session]))
    }

    /// Claude's review of the session's changes. Blocks for as long as the review takes.
    func review(session: String) throws -> [Finding] {
        try JSONDecoder().decode(ReviewResponse.self, from: send(["type": "review", "id": session])).findings
    }

    func cancelReview(session: String) throws {
        _ = try send(["type": "review_cancel", "id": session])
    }

    /// Claude's answer to a question about a session (side chat). Blocks for as long as it takes.
    func ask(session: String, question: String) throws -> String {
        try JSONDecoder().decode(TextResponse.self, from: send(["type": "ask", "id": session, "question": question])).text
    }

    func cancelAsk(session: String) throws {
        _ = try send(["type": "ask_cancel", "id": session])
    }

    /// Type `text` into a session as a paste; `submit` presses Return after it.
    func sendInput(session: String, text: String, submit: Bool) throws {
        _ = try send(["type": "send_input", "id": session, "text": text, "submit": submit])
    }

    /// Interrupt the agent's turn with its own key (Esc in most); the session goes on.
    func interrupt(session: String) throws {
        _ = try send(["type": "interrupt", "id": session])
    }

    /// Write `text` to a session as typed keys, not a paste.
    /// What has session `id`'s terminal right now, asked of its pty: nil at a shell's prompt.
    func foreground(session: String) throws -> ForegroundProcess? {
        try JSONDecoder().decode(ForegroundResponse.self, from: send(["type": "foreground", "id": session])).foreground
    }

    func sendKeys(session: String, text: String) throws {
        _ = try send(["type": "send_keys", "id": session, "text": text])
    }

    func prDraft(session: String) throws -> PrDraft {
        try JSONDecoder().decode(PrDraftResponse.self, from: send(["type": "pr_draft", "id": session])).draft
    }

    /// Commit what's uncommitted, push the branch and open the PR.
    func prCreate(session: String, title: String, body: String, base: String, draft: Bool) throws -> PrInfo {
        try JSONDecoder().decode(PrResponse.self, from: send([
            "type": "pr_create", "id": session, "title": title, "body": body, "base": base, "draft": draft,
        ])).pr
    }

    /// Hand the failing checks' logs to the session's agent.
    func prFix(session: String) throws {
        _ = try send(["type": "pr_fix", "id": session])
    }

    func prMerge(session: String) throws -> PrInfo {
        try JSONDecoder().decode(PrResponse.self, from: send(["type": "pr_merge", "id": session])).pr
    }

    /// Turn automatic fixing or merging on or off; nil leaves it as it is.
    func prAuto(session: String, fix: Bool? = nil, merge: Bool? = nil) throws {
        var body: [String: Any] = ["type": "pr_auto", "id": session]
        if let fix { body["fix"] = fix }
        if let merge { body["merge"] = merge }
        _ = try send(body)
    }

    /// The dev servers the session's folder configures, or why its launch file can't be read.
    func previewConfigs(session: String) throws -> ([PreviewConfig], String?) {
        let r = try JSONDecoder().decode(PreviewConfigsResponse.self, from: send(["type": "preview_configs", "id": session]))
        return (r.configs, r.error)
    }

    /// `approved` is what the user agreed to run; dinod starts it only if the launch file still says that.
    func previewStart(session: String, name: String, approved: PreviewConfig) throws {
        let config = try JSONSerialization.jsonObject(with: JSONEncoder().encode(approved))
        _ = try send(["type": "preview_start", "id": session, "name": name, "approved": config])
    }

    func previewStop(session: String, name: String) throws {
        _ = try send(["type": "preview_stop", "id": session, "name": name])
    }

    func previewLog(session: String, name: String) throws -> String {
        try JSONDecoder().decode(PreviewLogResponse.self, from: send(["type": "preview_log", "id": session, "name": name])).text
    }

    /// Change mode, model or effort. The agent restarts, resuming its conversation; mid-turn, once the turn is over.
    func setControls(session: String, controls: Controls) throws {
        _ = try send(["type": "set_controls", "id": session, "controls": controls.json])
    }

    /// Stop a server the session's agent left running in the background.
    func stopServer(session: String, task: String) throws {
        _ = try send(["type": "stop_server", "id": session, "task": task])
    }

    /// Stop builds left running for no session, by pid; none named: all of them.
    func stopLeftovers(pids: [UInt32] = []) throws {
        _ = try send(["type": "stop_leftovers", "pids": pids])
    }

    /// Start a session whose program ended again, in place: the agent resumes its conversation.
    func resume(session: String) throws {
        _ = try send(["type": "resume", "id": session])
    }

    /// Close `session` for everyone, keeping it `undoMs` ms so `reopenClosed` can bring it back as it was.
    func closeLater(session: String, undoMs: UInt64) throws {
        _ = try send(["type": "close", "id": session, "undo_ms": undoMs])
    }

    /// Bring back a session closed with `close` while its time to undo lasts.
    func reopenClosed(session: String) throws {
        _ = try send(["type": "reopen", "id": session])
    }

    /// Continue `session` in dino; returns the new dino session id.
    func adopt(_ session: FoundSession, cwd: String?) throws -> String? {
        let encoded = try JSONSerialization.jsonObject(with: JSONEncoder().encode(session))
        var body: [String: Any] = ["type": "adopt", "session": encoded]
        if let cwd { body["cwd"] = cwd }
        return try request(body).id
    }

    /// Stop waiting to continue a found session in dino (adopt): `id` is its conversation's id, or
    /// its process's.
    func cancelTakeOver(id: String) throws {
        _ = try send(["type": "cancel_take_over", "id": id])
    }

    /// One request/response exchange; throws dinod's error message as-is.
    func send(_ body: [String: Any]) throws -> Data {
        lock.lock()
        defer { lock.unlock() }
        let payload = try JSONSerialization.data(withJSONObject: body)
        var frame = Data([0])
        var len = UInt32(payload.count).bigEndian
        frame.append(Data(bytes: &len, count: 4))
        frame.append(payload)
        try writeAll(frame)
        let head = try readExact(5)
        let n = Int(UInt32(head[1]) << 24 | UInt32(head[2]) << 16 | UInt32(head[3]) << 8 | UInt32(head[4]))
        let data = Data(try readExact(n))
        if let err = try? JSONDecoder().decode(Response.self, from: data), err.type == "error" {
            let message = err.message ?? "error"
            // A request this dinod predates: it's still running an older build than the app.
            if message.hasPrefix("bad request: unknown variant") {
                throw DinoError.daemon("dino's background service is older than this app and can't do this yet. To restart it, run `dino stop` in a terminal, then click Start Background Service in dino. Your sessions come back.")
            }
            throw DinoError.daemon(message)
        }
        return data
    }

    private func writeAll(_ data: Data) throws {
        try data.withUnsafeBytes { raw in
            var off = 0
            while off < raw.count {
                let n = write(fd, raw.baseAddress! + off, raw.count - off)
                guard n > 0 else { throw DinoError.socket("write failed") }
                off += n
            }
        }
    }

    private func readExact(_ count: Int) throws -> [UInt8] {
        var buf = [UInt8](repeating: 0, count: count)
        var off = 0
        while off < count {
            let n = buf.withUnsafeMutableBytes { read(fd, $0.baseAddress! + off, count - off) }
            guard n > 0 else { throw DinoError.socket("dino's background service closed the connection.") }
            off += n
        }
        return buf
    }
}

/// Locating the `dino` CLI and giving it the user's real environment.
enum DinoEnvironment {
    /// Apps launched from Finder get a minimal PATH; agents live in the login shell's PATH.
    static let loginPath: String = {
        let shell = ProcessInfo.processInfo.environment["SHELL"] ?? "/bin/zsh"
        let p = Process()
        p.executableURL = URL(fileURLWithPath: shell)
        p.arguments = ["-l", "-c", "printf %s \"$PATH\""]
        let out = Pipe()
        p.standardOutput = out
        p.standardError = FileHandle.nullDevice
        try? p.run()
        p.waitUntilExit()
        let path = String(data: out.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
        return path.isEmpty ? (ProcessInfo.processInfo.environment["PATH"] ?? "/usr/bin:/bin") : path
    }()

    /// The `dino` a release build (and the dino you use, built by app/build.sh --install) carries
    /// in Contents/Helpers; nil in a build to try things in.
    static let bundledDino: String? = {
        let path = Bundle.main.bundleURL.appendingPathComponent("Contents/Helpers/dino").path
        return FileManager.default.isExecutableFile(atPath: path) ? path : nil
    }()

    /// The app's own `dino` first, so its dinod is the one it was built with.
    static let dinoBinary: String = {
        let env = ProcessInfo.processInfo.environment
        if let bin = env["DINO_BIN"] { return bin }
        if let bin = bundledDino { return bin }
        for dir in loginPath.split(separator: ":") {
            let candidate = "\(dir)/dino"
            if FileManager.default.isExecutableFile(atPath: candidate) { return candidate }
        }
        return NSString(string: "~/.local/bin/dino").expandingTildeInPath
    }()

    /// `$DINO_HOME` points the app at a second, isolated dinod (as it does the CLI). A build made
    /// for one (scripts/release.sh's `DINO_AGENT_HOME`) says which in Info.plist, so it uses that
    /// one however it's opened (Finder, a relaunch after an update), as do the programs it starts.
    static let home: String = {
        if let home = ProcessInfo.processInfo.environment["DINO_HOME"] { return home }
        if let home = Bundle.main.object(forInfoDictionaryKey: "DinoHome") as? String, !home.isEmpty {
            setenv("DINO_HOME", home, 1)
            return home
        }
        return NSString(string: "~/.config/dino").expandingTildeInPath
    }()
    static let socketPath = "\(home)/dinod.sock"

    /// `dino ping` starts dinod (with the login PATH) if it isn't running.
    static func ensureDaemon() throws {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: dinoBinary)
        p.arguments = ["ping"]
        var env = ProcessInfo.processInfo.environment
        env["PATH"] = loginPath
        p.environment = env
        p.standardOutput = FileHandle.nullDevice
        let err = Pipe()
        p.standardError = err
        do {
            try p.run()
        } catch {
            throw DinoError.daemon("dino's command-line tool isn't at \(dinoBinary): \(error.localizedDescription)")
        }
        let said = err.fileHandleForReading.readDataToEndOfFile()
        p.waitUntilExit()
        guard p.terminationStatus == 0 else {
            // What it said, not a guess: on a new Mac, say, the folder it couldn't make.
            let why = String(decoding: said, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
            throw DinoError.daemon("dino's background service didn't start: \(why.isEmpty ? "`dino ping` exited with \(p.terminationStatus)" : why)")
        }
    }
}

/// The pane a tmux client in a dino shell shows, and the bells and notifications from that tmux.
struct TmuxPane: Codable, Equatable {
    var target: String
    var label: String
    var busy: Bool
    var alerts: [TmuxAlert]?
}

struct TmuxAlert: Codable, Equatable {
    var seq: UInt64
    var text: String
}
