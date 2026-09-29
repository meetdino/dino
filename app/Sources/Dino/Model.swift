import AppKit
import GhosttyTerminal
import SwiftUI
import UserNotifications

enum SessionStatus {
    case thinking, working, idle, done, needsYou, exited

    var label: String {
        switch self {
        case .thinking: "thinking"
        case .working: "working"
        case .idle: "idle"
        case .done: "done"
        case .needsYou: "needs you"
        case .exited: "exited"
        }
    }

    var color: Color {
        switch self {
        case .thinking: Color(red: 0.78, green: 0.52, blue: 1.0)
        case .working: Brand.green
        case .idle: .secondary
        case .done: Color(red: 0.45, green: 0.82, blue: 0.95)
        case .needsYou: Color(red: 1.0, green: 0.78, blue: 0.2)
        case .exited: Color(red: 0.95, green: 0.35, blue: 0.3)
        }
    }
}

enum Brand {
    static let green = Color(red: 0x75 / 255, green: 0xB3 / 255, blue: 0x40 / 255)
    static let spike = Color(red: 0xFC / 255, green: 0x4F / 255, blue: 0x26 / 255)
}

@MainActor
final class DinoModel: ObservableObject {
    @Published var sessions: [SessionInfo] = []
    @Published var quotas: [QuotaInfo] = []
    @Published var launchers: [LauncherInfo] = []
    @Published var selected: String?
    @Published var error: String?
    /// Agent sessions dino didn't start (running elsewhere, recent, cloud).
    @Published var found: [FoundSession] = []
    @Published var loadingCloud = false
    /// A handoff in progress: the session being moved, and whether we're waiting on its turn.
    @Published var moving: FoundSession?
    @Published var showContinue = false
    /// A handoff waiting for the user's confirmation.
    @Published var confirmMove: FoundSession?

    /// Fan-outs, with each member's diff size.
    @Published var groups: [GroupInfo] = []
    @Published var showFanout = false
    /// A member whose changes the user is about to keep.
    @Published var confirmKeep: MemberInfo?
    /// A session worktree the user is about to close, and whether its changes come along.
    @Published var closingWorktree: ClosingWorktree?

    /// The review panel beside the terminal, and the comments waiting to go to each session.
    @Published var showReview = false
    @Published var comments: [String: [ReviewComment]] = [:]
    /// Claude's review of each session's changes; its findings join that session's comments.
    @Published var reviews: [String: ReviewRun] = [:]
    /// The review each session is waiting on, so a cancelled one's late answer is dropped.
    private var reviewRuns: [String: UUID] = [:]

    /// The Create PR sheet, and the popover about the selected session's PR.
    @Published var showCreatePR = false
    @Published var showPR = false
    /// PRs dino just opened or merged, until dinod's poller reports them.
    @Published private var acted: [String: PrInfo] = [:]

    var elsewhere: [FoundSession] { found.filter { $0.source == "running" } }

    /// Where new sessions start.
    @Published var folder: URL = FileManager.default.homeDirectoryForCurrentUser {
        didSet { if folder != oldValue { refreshTree() } }
    }

    /// Repos and folders the sidebar shows, with their worktrees.
    @Published var repos: [RepoInfo] = []

    /// Rang the bell or finished while in the background; cleared when selected.
    @Published private(set) var attention: Set<String> = []
    @Published private(set) var unseenDone: Set<String> = []

    /// Sessions shown side by side; selecting either one shows both.
    @Published var splits: [Split] = Split.saved() {
        didSet { if splits != oldValue { Split.save(splits) } }
    }
    /// Split partners dinod has started that no poll has listed yet.
    var awaited: Set<String> = []

    /// One live Ghostty surface per session, kept mounted so switching is instant.
    private(set) var terminals: [String: TerminalViewState] = [:]
    private(set) var connection: DinoConnection?
    private var polling = false

    func start() {
        Task.detached {
            do {
                try DinoEnvironment.ensureDaemon()
                let conn = try DinoConnection(path: DinoEnvironment.socketPath)
                let launchers = try conn.request(["type": "launchers"]).launchers ?? []
                await MainActor.run {
                    self.connection = conn
                    self.launchers = launchers
                    self.polling = true
                    self.poll()
                    self.watchElsewhere()
                    self.watchGroups()
                    self.watchTree()
                }
            } catch {
                await MainActor.run { self.error = error.localizedDescription }
            }
        }
    }

    @Published private(set) var daemonDown = false

    private func poll() {
        guard polling, let conn = connection else { return }
        Task.detached {
            let resp = try? conn.request(["type": "state"])
            await MainActor.run {
                if let resp {
                    self.apply(resp.sessions ?? [], resp.quotas ?? [])
                    DispatchQueue.main.asyncAfter(deadline: .now() + 0.25) { self.poll() }
                } else {
                    self.lostDaemon()
                }
            }
        }
    }

    /// dinod stopped: drop dead panes and wait for it to come back (without restarting it ourselves).
    private func lostDaemon() {
        connection = nil
        polling = false
        daemonDown = true
        terminals.removeAll()
        sessions = []
        reconnect()
    }

    private func reconnect() {
        Task.detached {
            if let conn = try? DinoConnection(path: DinoEnvironment.socketPath) {
                await MainActor.run {
                    self.connection = conn
                    self.daemonDown = false
                    self.polling = true
                    self.poll()
                }
            } else {
                try? await Task.sleep(for: .seconds(1))
                await MainActor.run { self.reconnect() }
            }
        }
    }

    /// The "Start dinod" button.
    func startDaemon() {
        Task.detached { try? DinoEnvironment.ensureDaemon() }
    }

    /// Saves and stops every session; the next dinod resumes them. Blocks: used while quitting.
    func stopDaemon() {
        _ = try? DinoConnection(path: DinoEnvironment.socketPath).request(["type": "shutdown"])
    }

    private func apply(_ next: [SessionInfo], _ quotas: [QuotaInfo]) {
        let appActive = NSApp.isActive
        for s in next {
            guard let prev = sessions.first(where: { $0.id == s.id }) else { continue }
            let looking = appActive && s.id == selected
            if !looking {
                if s.bells > prev.bells { attention.insert(s.id) }
                let wasBusy = prev.activity == "working" || prev.needs != nil
                if s.activity == "done", wasBusy {
                    unseenDone.insert(s.id)
                    Notifier.post(session: s, title: "\(s.name) finished", body: s.title ?? "Ready for your review")
                }
                if let needs = s.needs, prev.needs == nil {
                    Notifier.post(session: s, title: "\(s.name) needs you", body: needs)
                }
            }
            // CI runs for minutes: say when it's done, even about the session in front of you.
            if let pr = s.pr, let was = prev.pr, pr.number == was.number, was.checks.pending > 0, pr.checks.pending == 0 {
                if pr.checks.failed > 0 {
                    Notifier.post(session: s, title: "PR #\(pr.number): \(pr.checks.failing.first ?? "a check") failed", body: pr.title)
                } else {
                    Notifier.post(session: s, title: "PR #\(pr.number) checks passed", body: pr.title)
                }
            }
            // What dino did about the PR by itself.
            if let auto = s.auto, let pr = s.pr {
                if auto.fixes > (prev.auto?.fixes ?? 0) {
                    Notifier.post(session: s, title: "Asked \(s.name) to fix PR #\(pr.number)", body: pr.checks.failing.joined(separator: ", "))
                }
                if auto.merge, pr.state == "merged", prev.pr?.number == pr.number, prev.pr?.isOpen == true {
                    Notifier.post(session: s, title: "PR #\(pr.number) merged", body: "Auto-merged once its checks passed: \(pr.title)")
                }
                if let note = auto.note, prev.auto?.note != note {
                    Notifier.post(session: s, title: "PR #\(pr.number) needs you", body: note)
                }
            }
        }
        // A session in a folder the tree hasn't seen: ask for it now rather than on the next tick.
        if Set(next.compactMap(\.cwd)) != Set(sessions.compactMap(\.cwd)) { refreshTree() }
        if next != sessions { sessions = next }
        if quotas != self.quotas { self.quotas = quotas }
        let live = Set(next.map(\.id))
        terminals = terminals.filter { live.contains($0.key) }
        // A pane whose session ended closes, as it would in a terminal.
        awaited.subtract(live)
        let kept = splits.filter { [$0.first, $0.second].allSatisfy { live.contains($0) || awaited.contains($0) } }
        if kept != splits { splits = kept }
        if let want = pendingSelect, live.contains(want) {
            pendingSelect = nil
            select(want)
        }
        let groupSelected = selected.map { id in groups.contains { "group:\($0.id)" == id } } ?? false
        let folderSelected = selected?.hasPrefix("dir:") ?? false
        if !groupSelected, !folderSelected, selected == nil || !live.contains(selected!) {
            // Through select(), so the terminal also takes keyboard focus on launch.
            select(next.first?.id)
        }
        let waiting = next.filter { status(of: $0) == .needsYou }.count
        NSApp.dockTile.badgeLabel = waiting > 0 ? "\(waiting)" : nil
    }

    func status(of s: SessionInfo) -> SessionStatus {
        if s.exited { return .exited }
        if attention.contains(s.id) || s.needs != nil { return .needsYou }
        // Agents with hooks (Claude) say when a turn starts and ends. Between turns, their side
        // calls and their redraws when you focus or resize the pane aren't work.
        if let a = s.activity, a != "working" { return unseenDone.contains(s.id) ? .done : .idle }
        if s.in_flight > 0 { return .thinking }
        if s.activity == "working" || (s.output_ms_ago ?? .max) < 1500 { return .working }
        if unseenDone.contains(s.id) { return .done }
        return .idle
    }

    func select(_ id: String?) {
        selected = id
        guard let id else { return }
        if id.hasPrefix("dir:") {
            folder = URL(fileURLWithPath: String(id.dropFirst(4)))
            return
        }
        // New sessions start next to the one you're looking at.
        if let cwd = sessions.first(where: { $0.id == id })?.cwd { folder = URL(fileURLWithPath: cwd) }
        attention.remove(id)
        unseenDone.remove(id)
        terminals[id]?.requestFocus()
    }

    /// Next session that needs the user, then one that finished unseen.
    func jumpToAttention() {
        let order = sessions.map(\.id)
        let start = order.firstIndex(of: selected ?? "") ?? -1
        let rotated = (1 ... max(order.count, 1)).map { order[(start + $0 + order.count) % max(order.count, 1)] }
        let pick = rotated.first { id in sessions.first { $0.id == id }.map { status(of: $0) == .needsYou } ?? false }
            ?? rotated.first { unseenDone.contains($0) }
        if let pick { select(pick) }
    }

    /// Ghostty handles its own shortcuts before the menu sees them (⌘D splits, ⌘W closes, ⌘K
    /// clears), so a focused pane would swallow dino's. Hand those keys back to the menu.
    static let terminals = TerminalController(configSource: .generated(
        (["d", "alt+d", "shift+d", "w", "k", "j", "o", "n", "shift+n", "alt+n", "comma", "shift+backspace"]
            + (1 ... 9).flatMap { ["\($0)", "digit_\($0)"] })
            .map { "keybind = super+\($0)=unbind" }.joined(separator: "\n")
    ))

    func terminal(for id: String) -> TerminalViewState {
        if let t = terminals[id] { return t }
        let t = TerminalViewState(controller: Self.terminals)
        t.configuration = TerminalSurfaceOptions(
            backend: .exec,
            envVars: ["PATH": DinoEnvironment.loginPath, "DINO_HOME": DinoEnvironment.home],
            command: "\(DinoEnvironment.dinoBinary) attach \(id)",
            waitAfterCommand: false
        )
        terminals[id] = t
        return t
    }

    /// `worktree`: in a new worktree and branch of the repo, so its edits stay off your checkout.
    func newSession(_ launcher: LauncherInfo, worktree: Bool = false) {
        guard let conn = connection else { return }
        let cwd = folder.path
        Task.detached {
            do {
                let resp = try conn.request(["type": "new", "launcher": launcher.short, "args": [], "cwd": cwd, "cols": 120, "rows": 40, "worktree": worktree])
                await MainActor.run {
                    if let id = resp.id { self.select(id) }
                }
            } catch {
                await MainActor.run { self.error = error.localizedDescription }
            }
        }
    }

    /// Keep "On this Mac" fresh: sessions running in other terminals come and go.
    private func watchElsewhere() {
        Task.detached {
            while true {
                if let conn = try? DinoConnection(path: DinoEnvironment.socketPath), let list = try? conn.found(cloud: false) {
                    await MainActor.run {
                        // Keep cloud entries from the last full load.
                        let cloud = self.found.filter { $0.source == "cloud" }
                        if list + cloud != self.found { self.found = list + cloud }
                    }
                }
                try? await Task.sleep(for: .seconds(3))
            }
        }
    }

    /// Everything, including cloud (slower); for the Continue sheet.
    func loadFound() {
        loadingCloud = true
        Task.detached {
            let list = (try? DinoConnection(path: DinoEnvironment.socketPath).found(cloud: true)) ?? []
            await MainActor.run {
                self.found = list
                self.loadingCloud = false
            }
        }
    }

    /// Move a found session into dino. A running one finishes its turn first, then continues here.
    func adopt(_ f: FoundSession) {
        moving = f
        let cwd = folder.path
        Task.detached {
            do {
                // Own connection: a handoff may wait minutes for the turn to end.
                let conn = try DinoConnection(path: DinoEnvironment.socketPath)
                let id = try conn.adopt(f, cwd: cwd)
                await MainActor.run {
                    self.moving = nil
                    self.showContinue = false
                    self.found.removeAll { $0 == f }
                    // The next poll brings the new session; select it once it's there.
                    self.pendingSelect = id
                }
            } catch {
                await MainActor.run {
                    self.moving = nil
                    self.error = error.localizedDescription
                }
            }
        }
    }

    var pendingSelect: String?

    /// Diff sizes need git, so these refresh slower than session state. Launchers too: keys and
    /// policies change which agents can start.
    private func watchGroups() {
        Task.detached {
            while true {
                if let conn = try? DinoConnection(path: DinoEnvironment.socketPath), let list = try? conn.groups() {
                    let launchers = try? conn.request(["type": "launchers"]).launchers
                    await MainActor.run {
                        if let launchers, launchers != self.launchers { self.launchers = launchers }
                        if list != self.groups { self.groups = list }
                        if let want = self.pendingGroup, list.contains(where: { $0.id == want }) {
                            self.pendingGroup = nil
                            self.select("group:\(want)")
                        }
                    }
                }
                try? await Task.sleep(for: .seconds(2))
            }
        }
    }

    private var pendingGroup: String?

    private func watchTree() {
        Task.detached {
            while true {
                await MainActor.run { self.refreshTree() }
                try? await Task.sleep(for: .seconds(3))
            }
        }
    }

    /// Worktrees come from git, so this runs off the main thread and off the session poll.
    func refreshTree() {
        let folders = [folder.path]
        Task.detached {
            guard let list = try? DinoConnection(path: DinoEnvironment.socketPath).tree(folders: folders) else { return }
            await MainActor.run { if list != self.repos { self.repos = list } }
        }
    }

    /// Start a fan-out in the current folder; throws dinod's reason (not a git repo, …).
    func fanout(prompt: String, launchers: [String]) async throws {
        let cwd = folder.path
        let id = try await Task.detached {
            try DinoConnection(path: DinoEnvironment.socketPath)
                .request(["type": "fanout", "prompt": prompt, "launchers": launchers, "cwd": cwd]).id
        }.value
        showFanout = false
        pendingGroup = id
    }

    func diff(_ session: String) async -> String {
        await Task.detached { (try? DinoConnection(path: DinoEnvironment.socketPath).diff(session: session)) ?? "" }.value
    }

    /// Nil when dinod can't be reached; the panel keeps what it last showed.
    func changes(_ session: String) async -> Changes? {
        await Task.detached { try? DinoConnection(path: DinoEnvironment.socketPath).changes(session: session) }.value
    }

    /// Have Claude review the session's changes. Its findings replace the last review's among the
    /// comments, quoting lines from `changes`.
    func review(_ session: String, changes: Changes?) {
        guard reviews[session] != .running else { return }
        reviews[session] = .running
        let run = UUID()
        reviewRuns[session] = run
        Task {
            do {
                let found = try await Task.detached {
                    try DinoConnection(path: DinoEnvironment.socketPath).review(session: session)
                }.value
                guard reviewRuns[session] == run else { return }
                let added = found.map { f in
                    let at = LineRef(path: f.file, line: f.line, removed: false)
                    let code = changes?.files.first { $0.path == f.file }?.lines.first { LineRef(path: f.file, $0) == at }?.text ?? ""
                    return ReviewComment(at: at, code: code, text: f.message, severity: f.severity)
                }
                let list = (comments[session] ?? []).filter { $0.severity == nil } + added
                comments[session] = list.isEmpty ? nil : list
                reviews[session] = .done(found: found.count)
            } catch {
                // Cancelled, or replaced by a newer review.
                guard reviewRuns[session] == run else { return }
                reviews[session] = .failed(error.localizedDescription)
            }
        }
    }

    func cancelReview(_ session: String) {
        reviews[session] = nil
        reviewRuns[session] = nil
        Task.detached { try? DinoConnection(path: DinoEnvironment.socketPath).cancelReview(session: session) }
    }

    /// Hand the review to the agent as one message, submitted, as if the user had typed it.
    func sendComments(to session: String) async throws {
        guard let list = comments[session], !list.isEmpty else { return }
        let text = ReviewComment.message(list)
        try await Task.detached {
            try DinoConnection(path: DinoEnvironment.socketPath).sendInput(session: session, text: text, submit: true)
        }.value
        comments[session] = nil
        select(session)
    }

    /// The session's PR: dinod's view, or what dino just did if dinod hasn't caught up.
    func pr(of s: SessionInfo) -> PrInfo? {
        s.pr ?? acted[s.id]
    }

    /// The worktree dino made that the session runs in, if any: closable once its PR lands.
    func dinoWorktree(of s: SessionInfo) -> Worktree? {
        guard let cwd = s.cwd else { return nil }
        return repos.flatMap(\.worktrees).filter { $0.dino && SessionTree.contains($0.path, cwd) }.max { $0.path.count < $1.path.count }
    }

    func prDraft(_ session: String) async throws -> PrDraft {
        try await Task.detached { try DinoConnection(path: DinoEnvironment.socketPath).prDraft(session: session) }.value
    }

    func createPR(_ session: String, title: String, body: String, base: String, draft: Bool) async throws {
        let pr = try await Task.detached {
            try DinoConnection(path: DinoEnvironment.socketPath).prCreate(session: session, title: title, body: body, base: base, draft: draft)
        }.value
        acted[session] = pr
    }

    func mergePR(_ session: String) async throws {
        let pr = try await Task.detached { try DinoConnection(path: DinoEnvironment.socketPath).prMerge(session: session) }.value
        acted[session] = pr
    }

    /// Turn automatic fixing or merging of the session's PR on or off; the next poll shows it.
    func setAutoPR(_ session: String, fix: Bool? = nil, merge: Bool? = nil) async throws {
        try await Task.detached { try DinoConnection(path: DinoEnvironment.socketPath).prAuto(session: session, fix: fix, merge: merge) }.value
    }

    /// The agent gets the failing checks and their logs as a prompt; you watch it work.
    func fixPR(_ session: String) async throws {
        try await Task.detached { try DinoConnection(path: DinoEnvironment.socketPath).prFix(session: session) }.value
        showPR = false
        select(session)
    }

    /// Apply this member's changes to the checkout and close its fan-out.
    func keep(_ m: MemberInfo) {
        groupAction(["type": "keep", "session": m.session])
    }

    func discard(_ g: GroupInfo) {
        groupAction(["type": "discard", "group": g.id])
    }

    private func groupAction(_ body: [String: Any]) {
        Task.detached {
            do {
                _ = try DinoConnection(path: DinoEnvironment.socketPath).request(body)
                let list = try DinoConnection(path: DinoEnvironment.socketPath).groups()
                await MainActor.run {
                    self.groups = list
                    self.selected = nil
                }
            } catch {
                await MainActor.run { self.error = error.localizedDescription }
            }
        }
    }

    func chooseFolder() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.directoryURL = folder
        panel.prompt = "Use Folder"
        if panel.runModal() == .OK, let url = panel.url { folder = url }
    }

    /// Stop the sessions in a worktree dino made and remove it and its branch; `apply` first brings
    /// its changes into the checkout it came from, uncommitted.
    func closeWorktree(_ w: ClosingWorktree) {
        Task.detached {
            do {
                _ = try DinoConnection(path: DinoEnvironment.socketPath).request(["type": "remove_worktree", "path": w.path, "apply": w.apply])
            } catch {
                await MainActor.run { self.error = error.localizedDescription }
            }
        }
    }

    /// Remove finished worktrees, and their branches where git sees them merged. Never forced:
    /// dinod refuses one with uncommitted work.
    func cleanWorktrees(_ paths: [String]) {
        Task.detached {
            for path in paths {
                do {
                    _ = try DinoConnection(path: DinoEnvironment.socketPath).request(["type": "clean_worktree", "path": path])
                } catch {
                    await MainActor.run { self.error = error.localizedDescription }
                }
            }
            await MainActor.run { self.refreshTree() }
        }
    }

    func kill(_ id: String) {
        guard let conn = connection else { return }
        Task.detached { _ = try? conn.request(["type": "kill", "id": id]) }
    }
}
