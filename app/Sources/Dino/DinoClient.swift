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
    var input_tokens: UInt64
    var output_tokens: UInt64
    var last_model: String?
    var tier: String?
    var activity: String?
    var group: String?
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
    /// How full the context window is: tokens the last model call read, and the window's size.
    var context_tokens: UInt64?
    var context_limit: UInt64?
    /// The scheduled task that started it.
    var scheduled: String?
    /// The name the user gave it; `title` is this too while it's set.
    var label: String?
    /// Kept at the top of its group and out of dino's own archiving; nil from an older dinod.
    var pinned: Bool?
    /// Its task list, subagents and background commands, from its hooks; nil from an older dinod.
    var tasks: SessionTasks?
    /// The session whose agent started it, and the one that last messaged it (through `dino mcp`).
    var started_by: String?
    var messaged_by: String?
    /// The SSH host it runs on (`cwd` is then a path there); nil for this Mac.
    var host: String?
    /// A shell's: the agent someone started in it by hand, while it runs.
    var inside: FoundSession?
    /// A shell's, from its shell integration: where it is now (`cwd` is where it started), and
    /// how its last command ended.
    var shell_cwd: String?
    var last_exit: Int?
    /// Background commands its agent left serving (a dev server); nil from an older dinod.
    var servers: [ServerInfo]?
    /// The provider and model it runs on, when it isn't its agent's own account.
    var route: ProviderRoute?

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
    var id: String { short }
}

/// A session's permission mode, model and effort (see crates/dino-core/src/controls.rs).
/// Nil is the agent's own default.
/// A session on a provider's model (Settings → Providers) instead of its agent's own account.
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

    var any: Bool { !modes.isEmpty || model || !efforts.isEmpty }

    static let none = Knobs(modes: [], model: false, models: [], default_model: nil, efforts: [], restart: false)

    /// `name`'s entry: a model id or one of its aliases.
    func listed(_ name: String?) -> ModelInfo? {
        name.flatMap { n in models.first { $0.named(n) } }
    }

    /// The listed id `name` stands for ("haiku" is claude-haiku-4-5-…), else `name` itself.
    func canonical(_ name: String) -> String {
        listed(name)?.id ?? name
    }

    /// How to show a model: its listed name, else shortened.
    func label(_ name: String) -> String {
        listed(name)?.label ?? shortModel(name)
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
    }
}

/// Neutral permission modes: dino maps each to the agent's own flags.
enum Mode {
    static let all: [(id: String, label: String, help: String)] = [
        ("ask", "Ask", "Asks before editing files or running commands"),
        ("edits", "Accept edits", "Edits files without asking; asks before commands"),
        ("plan", "Plan", "Reads and plans; changes nothing"),
        ("auto", "Auto", "The agent decides what's safe to do without asking"),
        ("bypass", "Bypass", "Never asks. Only in a sandbox you trust"),
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

    var id: String { "\(source)-\(agent)-\(session_id)-\(pid ?? 0)" }
    var isBusy: Bool { status == "busy" }
    /// Started by hand in a dino shell, and dino can continue it (it has a conversation to resume).
    var continuable: Bool { ["claude", "codex", "qwen", "kimi", "pi", "hermes"].contains(baseAgent) && !session_id.isEmpty }
    var agentName: String { ["claude": "Claude", "codex": "Codex", "qwen": "Qwen", "kimi": "Kimi", "pi": "Pi", "hermes": "Hermes"][baseAgent] ?? agent }
    /// The agent, whether or not it ran on the free tier ("kimi-free" is Kimi).
    var baseAgent: String { agent.hasSuffix("-free") ? String(agent.dropLast(5)) : agent }
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

/// A fan-out: one prompt, several agents, each in its own worktree.
struct GroupInfo: Codable, Identifiable, Equatable {
    var id: String
    var prompt: String
    var repo: String
    var members: [MemberInfo]
}

struct MemberInfo: Codable, Identifiable, Equatable {
    var session: String
    var launcher: String
    var branch: String
    var worktree: String
    var stat: DiffStat?
    var id: String { session }
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

private struct TextResponse: Decodable {
    var text: String
}

private struct GroupsResponse: Decodable {
    var groups: [GroupInfo]
}

private struct DiffResponse: Decodable {
    var stat: DiffStat
    var text: String
}

private struct FoundResponse: Decodable {
    var sessions: [FoundSession]
}

private struct ConversationResponse: Decodable {
    var page: ConversationPage
}

struct Response: Decodable {
    var type: String
    var sessions: [SessionInfo]?

    enum CodingKeys: String, CodingKey { case type, sessions, quotas, launchers, id, message }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        type = try c.decode(String.self, forKey: .type)
        // `sessions` means SessionInfo only in a state reply.
        sessions = type == "state" ? try c.decodeIfPresent([SessionInfo].self, forKey: .sessions) : nil
        quotas = try c.decodeIfPresent([QuotaInfo].self, forKey: .quotas)
        launchers = try c.decodeIfPresent([LauncherInfo].self, forKey: .launchers)
        id = try c.decodeIfPresent(String.self, forKey: .id)
        message = try c.decodeIfPresent(String.self, forKey: .message)
    }
    var quotas: [QuotaInfo]?
    var launchers: [LauncherInfo]?
    var id: String?
    var message: String?
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
            throw DinoError.socket("can't connect to dinod at \(path)")
        }
    }

    deinit { close(fd) }

    func request(_ body: [String: Any]) throws -> Response {
        try JSONDecoder().decode(Response.self, from: send(body))
    }

    /// `runningOnly` skips finished conversations on disk (cheap enough to poll).
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

    func groups() throws -> [GroupInfo] {
        try JSONDecoder().decode(GroupsResponse.self, from: send(["type": "groups"])).groups
    }

    func diff(session: String) throws -> String {
        try JSONDecoder().decode(DiffResponse.self, from: send(["type": "diff", "session": session])).text
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

    /// Write `text` to a session as typed keys, not a paste.
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

    func previewStart(session: String, name: String) throws {
        _ = try send(["type": "preview_start", "id": session, "name": name])
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

    /// Start a session whose program ended again, in place: the agent resumes its conversation.
    func resume(session: String) throws {
        _ = try send(["type": "resume", "id": session])
    }

    /// Continue `session` in dino; returns the new dino session id.
    func adopt(_ session: FoundSession, cwd: String?) throws -> String? {
        let encoded = try JSONSerialization.jsonObject(with: JSONEncoder().encode(session))
        var body: [String: Any] = ["type": "adopt", "session": encoded]
        if let cwd { body["cwd"] = cwd }
        return try request(body).id
    }

    /// Continue the agent started by hand in shell `session` as a dino session, in the shell's place.
    func takeOver(session: String) throws {
        _ = try send(["type": "take_over", "id": session])
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
                throw DinoError.daemon("dinod is older than this app, so it can't do this yet. Restart it: run `dino stop` (your sessions come back), then start dinod again.")
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
            guard n > 0 else { throw DinoError.socket("dinod closed the connection") }
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

    static let dinoBinary: String = {
        let env = ProcessInfo.processInfo.environment
        if let bin = env["DINO_BIN"] { return bin }
        for dir in loginPath.split(separator: ":") {
            let candidate = "\(dir)/dino"
            if FileManager.default.isExecutableFile(atPath: candidate) { return candidate }
        }
        return NSString(string: "~/.local/bin/dino").expandingTildeInPath
    }()

    /// `$DINO_HOME` points the app at a second, isolated dinod (as it does the CLI).
    static let home = ProcessInfo.processInfo.environment["DINO_HOME"] ?? NSString(string: "~/.config/dino").expandingTildeInPath
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
        try p.run()
        p.waitUntilExit()
        guard p.terminationStatus == 0 else { throw DinoError.daemon("`dino ping` failed; is \(dinoBinary) installed?") }
    }
}
