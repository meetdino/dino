import AppKit
import DinoGhostty
import SwiftUI
import UserNotifications

enum SessionStatus {
    /// `ended`: its program exited cleanly and can be resumed; `exited`: it failed.
    /// `waiting`: its turn ended on subagents or background commands that still run.
    case thinking, working, waiting, idle, done, needsYou, ended, exited

    /// Four words, the same as the sidebar's filters: Working, Needs you, Done, Idle.
    var label: String {
        switch self {
        case .thinking, .working, .waiting: "Working"
        case .needsYou: "Needs you"
        case .done: "Done"
        case .idle, .ended, .exited: "Idle"
        }
    }

    /// The finer state behind the word, for a tooltip or a second line.
    var detail: String? {
        switch self {
        case .thinking: "Waiting on the model"
        case .working: "Running tools or writing output"
        case .waiting: "Turn finished; subagents or background commands are still running"
        case .needsYou: "Asking for something"
        case .done: "Finished since you last looked"
        case .idle: "Waiting for a prompt"
        case .ended: "Exited; Enter resumes it"
        case .exited: "Exited with an error"
        }
    }

    /// System colours, so they follow light, dark and increased contrast.
    var color: Color {
        switch self {
        case .thinking, .working, .waiting: Color(nsColor: .systemGreen)
        case .needsYou: Color(nsColor: .systemOrange)
        case .done: Color(nsColor: .systemBlue)
        case .idle, .ended: .secondary
        case .exited: Color(nsColor: .systemRed)
        }
    }
}

enum Brand {
    /// dino's green; darker in light mode, where the dark mode's is too faint for text on white.
    static let green = Color(nsColor: NSColor(name: "dino.green") { appearance in
        appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua
            ? NSColor(srgbRed: 0x75 / 255, green: 0xB3 / 255, blue: 0x40 / 255, alpha: 1)
            : NSColor(srgbRed: 0x4A / 255, green: 0x85 / 255, blue: 0x1C / 255, alpha: 1)
    })
    static let spike = Color(red: 0xFC / 255, green: 0x4F / 255, blue: 0x26 / 255)
}

@MainActor
final class DinoModel: ObservableObject {
    /// Every session, as dinod last said. Only a change in what the window as a whole shows of them
    /// (`outline`) is announced here, as that redraws the window, the menu bar and every pane; the
    /// rest is announced by that session's `LiveSession`, to the views that show it.
    var sessions: [SessionInfo] = [] {
        willSet { if outline(newValue) != outline(sessions) { objectWillChange.send() } }
        didSet { LiveSessions.follow(sessions) }
    }
    @Published var quotas: [QuotaInfo] = []
    /// Your Claude accounts with their windows, once Claude Code has more than one.
    @Published var claudeAccounts: [ClaudeAccountInfo] = []

    /// A session on another Claude account that the sidebar's footer names already: the one in use
    /// now. Only a session on a different one says so in its row.
    func saysInFooter(_ f: FallbackInfo) -> Bool {
        f.isAccount && claudeAccounts.first(where: \.answering)?.short == f.name
    }
    /// Agents at their limit, and what new sessions start with meanwhile (Settings → Agents).
    @Published var limits: [AgentLimit] = []
    /// Builds dinod found running for no session as it started (Leftovers.swift).
    @Published var leftovers: [Leftover] = []
    /// Kept awake with the lid closed, why sleep came back last, and what keeps the Mac awake; nil
    /// from an older dinod. Views observe `PowerState` for it (see there).
    var power: PowerInfo? { PowerState.shared.info }
    @Published var launchers: [LauncherInfo] = []
    @Published var selected: String? {
        // Remembered per dinod, so reopening the app comes back to the same session.
        didSet { if let id = selected, !id.contains(":") { UserDefaults.standard.set(id, forKey: Self.lastSelectedKey) } }
    }
    static let lastSelectedKey = "selected.\(DinoEnvironment.home)"
    @Published var error: String?
    /// Agent sessions dino didn't start (running elsewhere, recent, cloud).
    @Published var found: [FoundSession] = []
    @Published var loadingCloud = false
    /// Finished conversations on disk are in `found` (the browser has loaded them once).
    @Published var loadedHistory = false
    /// A handoff in progress: the session being moved, and whether we're waiting on its turn.
    /// Found sessions waiting to continue in dino, by `FoundSession.id`; see `adopt`.
    @Published var adopting: Set<String> = []
    @Published var showContinue = false
    /// A handoff waiting for the user's confirmation.
    @Published var confirmMove: FoundSession?
    /// One asked to move that dino can't tell the conversation of: why it can't.
    @Published var unsureMove: FoundSession?
    /// An agent in a tmux pane nobody is attached to: its screen, read-only (TmuxLook).
    @Published var tmuxLook: FoundSession?

    /// The New Session sheet: agent, place, mode, model and effort.
    @Published var showNewSession = false
    @Published var showNewProject = false
    /// The new-session picker that's open (StartSession.swift): where, then which agent.
    @Published var startRequest: StartRequest?
    /// The toolbar's mode, model or effort picker that's open (⇧⌘M, ⇧⌘I, ⇧⌘E).
    @Published var controlPicker: ControlKind?
    /// A session worktree the user is about to close, and whether its changes come along.
    @Published var closingWorktree: ClosingWorktree?
    /// Clean Up… waiting on the user's yes.
    @Published var cleaningUp: CleanUpPlan?

    /// Automations (what dinod does by itself when something happens), the one open in the
    /// editor and the one about to go.
    @Published var scheduled: [ScheduledTask] = []
    @Published var editingTask: ScheduledTask?
    /// Automations opened in the sidebar to show their runs.
    @Published var openTasks: Set<String> = []
    @Published var deletingTask: ScheduledTask?

    /// Stopped sessions kept to start again, the name being edited, and the ⌘/ sheet.
    @Published var archived: [ArchivedInfo] = []
    /// A session the user is about to delete, and what goes with it (the confirmation).
    @Published var deleting: DeletePlan?
    /// A working session asked to be archived: it stops mid-turn, so ask first.
    @Published var archiving: SessionInfo?
    @Published var renaming: Renaming?
    @Published var showShortcuts = false
    /// Help → Show Welcome: the first-open card, again.
    @Published var showWelcome = false

    /// The review panel beside the terminal, and the comments waiting to go to each session.
    @Published var showReview = false {
        // One side pane at a time; a file with unsaved changes asks first.
        didSet { if showReview, sidePane != nil, !closeSidePane() { showReview = false } }
    }
    @Published var comments: [String: [ReviewComment]] = [:]
    /// Claude's review of each session's changes; its findings join that session's comments.
    @Published var reviews: [String: ReviewRun] = [:]
    /// The review each session is waiting on, so a cancelled one's late answer is dropped.
    private var reviewRuns: [String: UUID] = [:]

    /// A file or the web preview, beside the terminals.
    @Published var sidePane: SidePane? {
        didSet { if sidePane != nil { showReview = false } }
    }
    /// The session whose tasks the Tasks pane shows while a folder is selected (after "Show in Sidebar").
    @Published var tasksFallback: String?
    /// Each session's browser, kept so switching sessions keeps its page ("" when opened without one).
    var webPages: [String: WebPage] = [:]
    /// The local address each session printed that the user has seen, opened or waved off.
    @Published var offered: [String: String] = [:]

    /// The Create PR sheet, and the popover about the selected session's PR.
    @Published var showCreatePR = false
    @Published var showPR = false
    /// The command palette (⇧⌘P): every action, by name.
    @Published var showPalette = false
    /// The session the side chat is asking about.
    @Published var askingAbout: SessionInfo?
    /// The session Fork with Options… is forking.
    @Published var forking: SessionInfo?
    /// Sessions a one-click fork is being made of: Fork again meanwhile (a double click) makes no second.
    var forksStarting: Set<String> = []
    /// PRs dino just opened or merged, until dinod's poller reports them.
    @Published private var acted: [String: PrInfo] = [:]

    var elsewhere: [FoundSession] { found.filter { $0.source == "running" } }

    /// Where new sessions start; kept across launches, so the first shell opens where you left off.
    @Published var folder: URL = DinoModel.lastFolder {
        didSet {
            guard folder != oldValue else { return }
            UserDefaults.standard.set(folder.path, forKey: Self.lastFolderKey)
            refreshTree()
        }
    }
    static let lastFolderKey = "folder.\(DinoEnvironment.home)"
    private static var lastFolder: URL {
        let home = FileManager.default.homeDirectoryForCurrentUser
        guard let path = UserDefaults.standard.string(forKey: lastFolderKey) else { return home }
        var dir: ObjCBool = false
        return FileManager.default.fileExists(atPath: path, isDirectory: &dir) && dir.boolValue ? URL(fileURLWithPath: path) : home
    }

    /// Launch has been handled: the session left selected is back, or a shell was started.
    private(set) var launched = false
    /// A session has been shown: from then on, one is only picked among the tabs.
    private var shownOne = false
    /// The shell started at launch, until dinod says which it is.
    private var startingShell = false

    /// Repos and folders the sidebar shows, with their worktrees.
    @Published var repos: [RepoInfo] = []

    /// The sidebar's search (⇧⌘F) and the one project or host it's narrowed to; not kept
    /// across launches, so the app never opens with sessions hidden.
    @Published var sidebarQuery = ""
    @Published var sidebarScope: SidebarScope?
    /// The search field is up; set to put the cursor in it.
    @Published var findingSessions = false

    /// Sessions whose computer-use banner was closed: hidden until their agent's next separate
    /// burst of using the Mac or a browser (see `noteUsing`).
    @Published var usingHidden: Set<String> = []
    /// When each session's agent was last seen using the Mac or a browser.
    var usingSeen: [String: Date] = [:]

    /// Rang the bell or finished while in the background; cleared when selected.
    @Published private(set) var attention: Set<String> = []
    /// Kept across relaunch, so a session that finished while dino was closed still says Done.
    @Published private(set) var unseenDone: Set<String> = Set(UserDefaults.standard.stringArray(forKey: DinoModel.unseenKey) ?? []) {
        didSet { if unseenDone != oldValue { UserDefaults.standard.set(Array(unseenDone), forKey: Self.unseenKey) } }
    }
    static let unseenKey = "unseenDone.\(DinoEnvironment.home)"

    /// Each tab's panes, when it has more than one: selecting any of them shows them all.
    @Published var splits: [SplitTree] = SplitTree.saved() {
        didSet { if splits != oldValue { SplitTree.save(splits) } }
    }
    /// Split partners dinod has started that no poll has listed yet.
    var awaited: Set<String> = []
    /// The size of the area the panes share, as last laid out: what Ghostty's `resize_split`
    /// moves a divider in.
    var paneArea = CGSize(width: 1000, height: 700)

    /// One live Ghostty surface per session, kept mounted so switching is instant.
    private(set) var terminals: [String: TerminalViewState] = [:]
    /// The session whose terminal has keyboard focus, for ⌘W.
    @Published var focusedTerminal: String? {
        didSet { if focusedTerminal != oldValue { SecureInput.shared.update() } }
    }
    private(set) var connection: DinoConnection?
    private var polling = false

    private var started = false

    /// Connects to dinod and starts listening; a window opened again later reuses it.
    func start() {
        guard !started else { return }
        started = true
        Task.detached {
            do {
                // Before dinod starts: `dino ping` starts it through the agent, once registered.
                // Not running (stopped as dino quit for an update, say): the agent can be
                // registered again if an update changed it.
                DinodAgent.setUp(stopped: (try? DinoConnection(path: DinoEnvironment.socketPath)) == nil)
                try DinoEnvironment.ensureDaemon()
                let conn = try DinoConnection(path: DinoEnvironment.socketPath)
                let launchers = try conn.request(["type": "launchers"]).launchers ?? []
                await MainActor.run {
                    self.connection = conn
                    self.checkDaemonVersion()
                    self.launchers = launchers
                    self.polling = true
                    self.poll()
                    self.watchElsewhere()
                    self.watchSlower()
                    self.watchTree()
                    self.watchGhosttyConfig()
                    self.watchTerminalSettings()
                    DinodAgent.askForApproval()
                }
            } catch {
                await MainActor.run { self.error = error.localizedDescription }
            }
        }
    }

    @Published private(set) var daemonDown = false
    /// dinod has answered once for everything the sidebar lists: the sessions, the repos and
    /// worktrees they're filed under, the agents in other terminals and the scheduled tasks. The
    /// sidebar is built then, whole (see Sidebar).
    @Published private(set) var sidebarReady = false
    private var sessionsPolled = false
    private var schedulePolled = false
    private var treePolled = false
    private var elsewherePolled = false

    private func notePolled(sessions: Bool = false, schedule: Bool = false, tree: Bool = false, elsewhere: Bool = false) {
        sessionsPolled = sessionsPolled || sessions
        schedulePolled = schedulePolled || schedule
        treePolled = treePolled || tree
        elsewherePolled = elsewherePolled || elsewhere
        if !sidebarReady, sessionsPolled, schedulePolled, treePolled, elsewherePolled { sidebarReady = true }
    }
    /// dinod isn't the dino this app carries (it's from before an update); see Updates.swift.
    @Published var daemonOutdated = false
    @Published var daemonVersion: String?
    /// dinod isn't run by this app's launch agent, though it's registered: it was started before
    /// the agent was (or allowed), so what it runs lacks dino's permissions; or an update changed
    /// the agent. Offered a restart like an outdated one (LaunchAgent.swift).
    @Published var daemonUnmanaged = false
    @Published var restartingDaemon = false

    /// dinod's tag for the state last applied: it answers once there's something else to show.
    private var stateSeen: UInt64?
    /// The waits have a connection of their own: on the shared one, every request would wait too.
    private var stateConnection: DinoConnection?

    /// Wait for the state to change, apply it, and wait again.
    private func poll() {
        guard polling else { return }
        if stateConnection == nil { stateConnection = try? DinoConnection(path: DinoEnvironment.socketPath) }
        guard let wait = stateConnection else { return lostDaemon() }
        var body: [String: Any] = ["type": "state_change"]
        body["seen"] = stateSeen
        let request = body
        Task.detached {
            // An error answer, rather than none: dinod is there, so ask again in a moment.
            let (resp, refused): (Response?, Bool) = {
                do { return (try wait.request(request), false) }
                catch DinoError.daemon { return (nil, true) }
                catch { return (nil, false) }
            }()
            await MainActor.run {
                if let resp {
                    self.apply(resp.sessions ?? [], resp.quotas ?? [])
                    self.applyPower(resp.power)
                    let limits = resp.limits ?? []
                    if limits != self.limits { self.limits = limits }
                    let leftovers = resp.leftovers ?? []
                    if leftovers != self.leftovers { self.leftovers = leftovers }
                    let accounts = resp.claude_accounts ?? []
                    if accounts != self.claudeAccounts { self.claudeAccounts = accounts }
                    self.stateSeen = resp.version
                    self.poll()
                } else if refused {
                    DispatchQueue.main.asyncAfter(deadline: .now() + 1) { self.poll() }
                } else {
                    self.lostDaemon()
                }
            }
        }
    }

    /// dinod stopped: drop dead panes and wait for it to come back (without restarting it ourselves).
    private func lostDaemon() {
        connection = nil
        stateConnection = nil
        stateSeen = nil
        polling = false
        daemonDown = true
        terminals.removeAll()
        sessions = []
        restartAfterCrash()
        reconnect()
    }

    /// When dinod was started again because it crashed: a few, then it's left to the user.
    private var crashRestarts: [Date] = []

    /// dinod went away without being asked to (no stop mark, see dinod's `stopped_mark`): start it
    /// again, so its sessions come back, as they would at the next launch. One stopped on purpose
    /// (`dino stop`, quitting with Stop) stays stopped until someone starts it.
    private func restartAfterCrash() {
        let mark = URL(fileURLWithPath: DinoEnvironment.socketPath).deletingLastPathComponent().appendingPathComponent("dinod.stopped")
        guard !FileManager.default.fileExists(atPath: mark.path) else { return }
        crashRestarts = crashRestarts.filter { $0.timeIntervalSinceNow > -60 }
        guard crashRestarts.count < 3 else { return }
        crashRestarts.append(Date())
        Task.detached {
            try? await Task.sleep(for: .milliseconds(500))
            try? DinoEnvironment.ensureDaemon()
        }
    }

    private func reconnect() {
        Task.detached {
            if let conn = try? DinoConnection(path: DinoEnvironment.socketPath) {
                await MainActor.run {
                    self.connection = conn
                    self.checkDaemonVersion()
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

    private func applyPower(_ next: PowerInfo?) {
        guard next != power else { return }
        // A safety stop (battery, heat, time) is worth a notification; the first state isn't.
        if let note = next?.note, let at = next?.note_at, power != nil, at != power?.note_at {
            Notifier.post(key: "lid", title: "Closing the lid sleeps your Mac again", body: note)
        }
        PowerState.shared.info = next
    }

    private func apply(_ next: [SessionInfo], _ quotas: [QuotaInfo]) {
        // The quick terminal's shell is its own, not a session in the sidebar.
        let quick = next.first { $0.id == QuickTerminal.shared.sessionID }
        QuickTerminal.shared.sessionAlive = quick.map { !$0.exited } ?? false
        let prompts = (quick?.password ?? false, next.filter { $0.password == true }.map(\.id))
        defer {
            // A password prompt came or went: Secure Keyboard Entry follows (only the one with the
            // keyboard counts).
            if prompts.0 != QuickTerminal.shared.passwordPrompt || prompts.1 != passwordPrompts {
                QuickTerminal.shared.passwordPrompt = prompts.0
                passwordPrompts = prompts.1
                SecureInput.shared.update()
            }
        }
        let raw = next.filter { $0.id != QuickTerminal.shared.sessionID }
        // Only which side of 1.5s and 5s the last output is matters here. Kept exact, a session
        // printing anything differs on every tick and the whole window redraws four times a second.
        // Agents also animate a spinner at the front of their terminal title (Claude cycles
        // ◐◓◑◒ while it works); dino shows that with the status dot, and each glyph would too.
        let next = raw.map { s in
            var s = s
            s.output_ms_ago = s.output_ms_ago.map { $0 < 1500 ? 0 : $0 < 5000 ? 1500 : 5000 }
            s.title = s.title.flatMap(Self.undecorated)
            if let t = s.inside?.title { s.inside?.title = Self.undecorated(t) ?? t }
            return s
        }
        let appActive = NSApp.isActive
        // Only when there's something new: mutating a published set announces a change even when
        // it adds nothing, and this runs on every state, which redrew the window four times a second.
        let tmuxNeeds = noteTmux(next) { appActive && $0.id == self.selected }
        if !Set(tmuxNeeds).isSubset(of: attention) { attention.formUnion(tmuxNeeds) }
        noteUsing(next)
        for s in next {
            // Started with another agent, its own at its limit: said once, as it appears (a
            // scheduled task, ⌘N, an agent that started it, as well as New Session).
            if let why = s.instead_of, !sessions.isEmpty, !sessions.contains(where: { $0.id == s.id }) {
                let asked = launchers.first { $0.agent_id == why.agent_id }?.label ?? why.agent_id
                let until = why.resets_at.map { " until \(Clock.short($0))" } ?? ""
                Notifier.post(session: s, title: "Started \(s.display) instead of \(asked)", body: "\(why.name) is at its limit\(until) (Settings → Agents)")
            }
            guard let prev = sessions.first(where: { $0.id == s.id }) else { continue }
            // Its route is spent and a fallback took over: said once, as it does.
            if let f = s.fallback, prev.fallback == nil {
                if f.isModel {
                    Notifier.post(session: s, title: "\(s.display) switched to \(f.model)", body: "\(f.name) rejects \(f.from) for your account: \(f.said)")
                } else if f.isAccount {
                    Notifier.post(session: s, title: "\(s.display) switched to \(f.name)", body: "\(f.why). \(f.name) answers until then.")
                } else {
                    Notifier.post(session: s, title: "\(s.display) switched to \(f.name)", body: "\(f.why). \(f.name) answers with \(f.model) until then.")
                }
            }
            let looking = appActive && s.id == selected
            // Working again: a bell it rang before isn't asking for you any more.
            let resumed = (s.activity == "working" && prev.activity != "working") || (s.inside?.status == "busy" && prev.inside?.status != "busy")
            if resumed, attention.contains(s.id) { attention.remove(s.id) }
            if !looking {
                // A bell asks for you only where nothing else says how the session is (a plain
                // shell, an agent with no hooks). One that reports its own state rings as a turn
                // ends (Claude Code does): its Needs you and Done already say what it means.
                if s.bells > prev.bells, !s.reportsStatus, !attention.contains(s.id) { attention.insert(s.id) }
                let wasBusy = prev.activity == "working" || prev.needs != nil || prev.waitingOn != nil
                if s.activity == "done", wasBusy {
                    unseenDone.insert(s.id)
                    if s.scheduled == nil {
                        Notifier.post(session: s, title: "\(s.display) finished", body: s.title ?? "Ready for your review")
                    }
                }
                if let needs = s.needs, prev.needs == nil {
                    Notifier.post(session: s, title: "\(s.display) needs you", body: needs)
                }
                // Not in front of you: say an agent started using the Mac or the browser.
                if let sentence = s.usingSentence, prev.reach == nil, s.needs == nil, !appActive, UsingDisplay.current != .off {
                    Notifier.post(session: s, title: sentence, body: "\(s.display) · Open the session to watch or stop it")
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
                    Notifier.post(session: s, title: "Asked \(s.display) to fix PR #\(pr.number)", body: pr.checks.failing.joined(separator: ", "))
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
        if Set(next.compactMap(\.here)) != Set(sessions.compactMap(\.here)) { refreshTree() }
        // The selected shell `cd`d: the tree, and sessions started next to it, follow.
        if let id = selected, let s = next.first(where: { $0.id == id }), s.host == nil, let here = s.here,
           here != sessions.first(where: { $0.id == id })?.here { folder = URL(fileURLWithPath: here) }
        placeHandedOff(next)
        if next != sessions { sessions = next }
        notePolled(sessions: true)
        syncTabs(next)
        restoreReopened(Set(next.map(\.id)))
        if quotas != self.quotas { self.quotas = quotas }
        let live = Set(next.map(\.id))
        terminals = terminals.filter { live.contains($0.key) }
        PaneSignals.keep(live.union([QuickTerminal.shared.sessionID].compactMap { $0 }))
        if !unseenDone.isSubset(of: live) { unseenDone.formIntersection(live) }
        webPages = webPages.filter { $0.key.isEmpty || live.contains($0.key) }
        for s in next { webPages[s.id]?.follow(s.previews ?? []) }
        // A pane whose session ended closes, as it would in a terminal.
        awaited.subtract(live)
        let present = { (id: String) in live.contains(id) || self.awaited.contains(id) }
        // The selected pane's session gone: the pane Ghostty would focus next takes over.
        if pendingSelect == nil, let id = selected, !present(id), let t = split(of: id),
           let next = t.pruned({ present($0) || $0 == id })?.afterClosing(id) {
            pendingSelect = next
        }
        let kept = splits.compactMap { $0.pruned(present) }
        if kept != splits { splits = kept }
        // `dino <folder>`: the session it started, brought forward even from another app.
        let revealed = next.filter { ($0.revealed ?? 0) > revealedUpTo }.max { ($0.revealed ?? 0) < ($1.revealed ?? 0) }
        if let r = revealed {
            revealedUpTo = r.revealed ?? revealedUpTo
            pendingSelect = r.id
            NSApp.activate(ignoringOtherApps: true)
            if !NSApp.windows.contains(where: { $0.isVisible && $0.canBecomeMain }) { showWindow?() }
        }
        if let want = pendingSelect, live.contains(want) {
            pendingSelect = nil
            select(want)
        }
        // Opening dino opens a terminal: a shell when there's nothing to come back to, or always if
        // Settings says so.
        if !launched {
            launched = true
            // Opened with something (a folder, a script): that instead of the usual first shell.
            if !Opening.pending.isEmpty {
                open(Opening.pending)
                Opening.pending = []
            } else if revealed == nil, next.isEmpty || StartWith.current == .shell {
                startingShell = newShell()
            }
        }
        // Rows that aren't sessions (subagents, runs) carry a "kind:" prefix: one of
        // those stays selected. Only a session that's gone falls back to another.
        let sessionGone = selected.map { !$0.contains(":") && !live.contains($0) } ?? true
        if !startingShell, sessionGone, !(shownOne && tabs.isEmpty) {
            // The one selected when the app last quit, else the one that last did something; once
            // running, only among the tabs: with the last one closed, nothing is.
            // Through select(), so the terminal also takes keyboard focus on launch.
            let last = UserDefaults.standard.string(forKey: Self.lastSelectedKey).flatMap { live.contains($0) ? $0 : nil }
            let recent = raw.filter { !$0.exited }.min { ($0.output_ms_ago ?? .max) < ($1.output_ms_ago ?? .max) }
            let open = tabs.first { live.contains($0) }
            select(shownOne ? open : last ?? recent?.id ?? next.first?.id)
        }
        // An update waiting for quiet installs now, restarting dino and dinod both (Updates.swift).
        if Updates.shared.pending?.whenIdle == true {
            Updates.shared.sessionsChanged(quiet: restartIsQuiet)
        }
        updateBadge(next)
    }

    /// What the whole window shows of each session: all dinod says of it but a shell's title (an
    /// agent's is its name, which counts) and how it's doing within its sidebar group: thinking or
    /// working, what it asks or waits on, its last output, what a shell runs and how its last command
    /// ended, its bells, a password prompt. Those change all the time, and only the session's own
    /// views show them (its row, its pane's header, the toolbar, Open Beside, an automation's run),
    /// through its `LiveSession`.
    private func outline(_ list: [SessionInfo]) -> [SessionOutline] {
        // The sidebar's search matches titles: while it narrows the sidebar, a title counts too.
        let searching = !sidebarQuery.trimmingCharacters(in: .whitespaces).isEmpty
        return list.map { s in
            var o = s
            if !searching { o.title = nil }
            o.activity = nil
            o.in_flight = 0
            o.output_ms_ago = nil
            o.running = nil
            o.foreground = nil
            o.last_exit = nil
            o.bells = 0
            o.password = nil
            // The sidebar's filters count and list sessions by their group (a plain shell isn't
            // there, and its tab has no dot); whether it asks for something words the computer-use
            // banner.
            return SessionOutline(session: o, name: s.display, group: s.plainShell ? nil : status(of: s).label, asks: s.needs != nil)
        }
    }

    private struct SessionOutline: Equatable {
        var session: SessionInfo
        var name: String
        var group: String?
        var asks: Bool
    }

    /// A terminal title without the spinner or status glyphs an agent puts before its words.
    nonisolated static func undecorated(_ title: String) -> String? {
        let t = title.drop { !$0.isLetter && !$0.isNumber && $0 != "~" && $0 != "/" && $0 != "." }.trimmingCharacters(in: .whitespaces)
        return t.isEmpty ? nil : t
    }

    func status(of s: SessionInfo) -> SessionStatus {
        if s.exited { return (s.exit_code ?? 0) == 0 ? .ended : .exited }
        if attention.contains(s.id) || s.needs != nil { return .needsYou }
        // An agent run by hand in a shell says whether it's busy, unless its hooks report to dino
        // (typed into a dino shell, see dino-agents.zsh): those say more, as for dino's sessions.
        if s.activity == nil, let f = s.inside, let st = f.status {
            if st == "needs" { return .needsYou }
            if st == "busy" { return s.in_flight > 0 ? .thinking : .working }
            return unseenDone.contains(s.id) ? .done : .idle
        }
        // Agents with hooks (Claude) say when a turn starts and ends. Between turns, their side
        // calls and their redraws when you focus or resize the pane aren't work. A turn that ended
        // on subagents or background commands isn't done until they are.
        if s.waitingOn != nil { return .waiting }
        if let a = s.activity, a != "working" { return unseenDone.contains(s.id) ? .done : .idle }
        if s.in_flight > 0 { return .thinking }
        if s.activity == "working" || (s.output_ms_ago ?? .max) < 1500 { return .working }
        if unseenDone.contains(s.id) { return .done }
        return .idle
    }

    /// The selection came from arrowing through the sidebar: the keyboard stays there, so the next
    /// arrow goes on to the next row. A click on the row or into the terminal hands it over.
    private(set) var selectingFromSidebarKeys = false

    func select(_ id: String?, keepKeyboard: Bool = false) {
        selectingFromSidebarKeys = keepKeyboard
        // Each only when it changes: a write of the same value still redraws everything watching.
        if selected != id { selected = id }
        guard let id else { return }
        // A folder's row (a repo's, or one of its checkouts): new sessions start there.
        if let path = Self.folderPath(id) {
            moveFolder(to: path)
            return
        }
        openTab(id)
        shownOne = true
        // New sessions start next to the one you're looking at.
        // (Only one on this Mac: a remote session's folder isn't one here.)
        if let s = sessions.first(where: { $0.id == id }), s.host == nil, let cwd = s.here { moveFolder(to: cwd) }
        if attention.contains(id) { attention.remove(id) }
        if unseenDone.contains(id) { unseenDone.remove(id) }
        if !keepKeyboard { terminals[id]?.requestFocus() }
    }

    /// Whether the main area has something of its own for a sidebar row: a session's terminal, a
    /// subagent's conversation, an automation's run. A repo's row, a
    /// worktree's over its sessions, "Other worktrees" and a worktree in it have none: selected,
    /// they left the main area empty, so they open and close instead and the session stays.
    func showsSomething(_ tag: String) -> Bool {
        if tag.hasPrefix("repo:") || tag.hasPrefix("others:") { return false }
        if tag.hasPrefix("dir:") { return worktree(at: String(tag.dropFirst(4)))?.owner != nil }
        return true
    }

    /// A folder's row was clicked: new sessions start there, and what's shown stays.
    func startHere(_ tag: String) {
        if let path = Self.folderPath(tag) { moveFolder(to: path) }
    }

    /// The folder a sidebar selection stands for: "dir:" a checkout, "repo:" a repo's own row.
    static func folderPath(_ selection: String?) -> String? {
        guard let selection else { return nil }
        for prefix in ["dir:", "repo:"] where selection.hasPrefix(prefix) {
            return String(selection.dropFirst(prefix.count))
        }
        return nil
    }

    private func moveFolder(to path: String) {
        let url = URL(fileURLWithPath: path)
        if folder != url { folder = url }
    }

    /// Back to finished-and-not-looked-at, under Needs you, until it's selected again.
    func markUnread(_ id: String) {
        unseenDone.insert(id)
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
    /// clears, ⇧⌘P its command palette, ⇧⌘[ ] its tabs), so a focused pane would swallow dino's.
    /// Hand every dino shortcut back to the menu, over whatever the user's Ghostty config binds it
    /// to. A shortcut added to a menu belongs here too (not Edit › Find's: those are Ghostty's own
    /// keybinds for the same thing, and stay the user's to change). Ghostty's ⌘K (clear) is ⌥⌘K.
    /// A punctuation key goes by its name and by its character: Ghostty binds ⌘, (open its config)
    /// as `super+,`, and an unbind takes away only a binding written the same way.
    static let menuKeys = ((["d", "shift+d", "alt+c", "w", "k", "alt+k", "j", "o", "n", "t", "shift+n", "alt+n", "ctrl+n", "alt+shift+n",
                             "comma", ",", "shift+backspace", "s", "shift+o", "alt+p", "alt+t", "shift+p", "shift+bracket_left", "shift+[",
                             "shift+bracket_right", "shift+]", "shift+semicolon", "shift+;", "backslash", "\\", "shift+m", "shift+i",
                             "shift+e", "alt+b", "ctrl+alt+b"]
        + (1 ... 9).flatMap { ["\($0)", "digit_\($0)"] })
        .map { "super+\($0)" }
        // Ctrl+Tab cycles sessions, ⌃` swaps split panes, ⌘/ lists shortcuts, ⇧⌘A archives, ⇧⌘F
        // finds sessions.
        + ["ctrl+tab", "ctrl+shift+tab", "ctrl+backquote", "ctrl+`", "super+slash", "super+/", "super+shift+a", "super+shift+f"])
        .map { "keybind = \($0)=unbind" }.joined(separator: "\n")

    /// Each pane then takes the light or dark of its own window (the app's look: the Mac's mode or
    /// View > Appearance) and follows it, as a Ghostty window does. The app starts in the app's.
    static let terminals: TerminalController = {
        // The configs earlier runs put together for Ghostty, left behind: this one lives as long
        // as the app does, so its own goes only when the next run starts.
        try? FileManager.default.removeItem(at: TerminalController.managedConfigDirectory)
        let c = TerminalController(configSource: .generated(menuKeys))
        c.setColorScheme(NSApp.effectiveAppearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua ? .dark : .light)
        GhosttyConfig.apply(to: c, overrides: menuKeys)
        GhosttyActions.install(on: c)
        return c
    }()

    /// Picks up edits to the Ghostty config, as Ghostty does when told to reload.
    private func watchGhosttyConfig() {
        // Read now, not at the first pane: Settings says what's in effect.
        _ = Self.terminals
        GhosttyActions.model = self
        Timer.scheduledTimer(withTimeInterval: 2, repeats: true) { _ in
            MainActor.assumeIsolated {
                if Self.isShown, GhosttyConfig.changed { GhosttyConfig.apply(to: Self.terminals, overrides: Self.menuKeys) }
            }
        }
    }

    /// Some of the app's windows are on screen. The slower pollers fetch what only a window shows,
    /// so they rest while none does (hidden, minimized, covered or closed); what notifies goes on.
    static var isShown: Bool { NSApp.occlusionState.contains(.visible) }

    /// Wait until a window of the app shows: at once when one does.
    nonisolated static func untilShown() async {
        var changes = NotificationCenter.default.notifications(named: NSApplication.didChangeOcclusionStateNotification).makeAsyncIterator()
        while !(await MainActor.run { isShown }) {
            _ = await changes.next()
        }
    }

    /// The terminal's own settings as dinod and the app last agreed on them.
    private var agreedTerminal: DinoSettings.Terminal?

    /// The app reads its terminal settings itself (at launch, before dinod answers), and dinod keeps
    /// them in `settings.toml` so they sync: whichever side changed since they last agreed wins.
    private func watchTerminalSettings() {
        let check: @Sendable () -> Void = {
            Task.detached {
                guard let conn = try? DinoConnection(path: DinoEnvironment.socketPath),
                      var settings = try? conn.settings() else { return }
                let tmux = settings.tmux
                let shell = await MainActor.run {
                    self.noteTmuxSettings(tmux)
                    return GhosttyConfig.shellSetup
                }
                // The user's Ghostty `shell-integration` and its features, for the shells dinod
                // starts (a dinod that knows them).
                if settings.machine.shell_features != nil,
                   settings.machine.shell_integration_mode != shell.mode || settings.machine.shell_features != shell.features {
                    settings.machine.shell_integration_mode = shell.mode
                    settings.machine.shell_features = shell.features
                    try? conn.setSettings(settings)
                }
                guard let whole = settings.terminal else { return }
                // Only what the app keeps for itself: the rest is dinod's alone.
                let there = whole.appOwn
                let (here, agreed) = await MainActor.run { (DinoSettings.Terminal.mirrored, self.agreedTerminal) }
                // First look: an app that had these before dinod kept them hands them over once.
                let fromApp = agreed.map { here != $0 && there == $0 } ?? (there == .defaults && here != .defaults)
                if fromApp {
                    settings.terminal = whole.with(appOwn: here)
                    try? conn.setSettings(settings)
                    await MainActor.run { self.agreedTerminal = here }
                } else {
                    await MainActor.run {
                        there.mirror()
                        self.agreedTerminal = there
                    }
                }
            }
        }
        check()
        Timer.scheduledTimer(withTimeInterval: 5, repeats: true) { _ in
            if MainActor.assumeIsolated({ Self.isShown }) { check() }
        }
    }

    /// What a pane's `dino attach` runs with: the scrollback the pane keeps (Ghostty's
    /// `scrollback-limit`) is what a reattach brings back.
    static var attachEnv: [String: String] {
        ["PATH": DinoEnvironment.loginPath, "DINO_HOME": DinoEnvironment.home,
         "DINO_SCROLLBACK_LIMIT": String(terminals.configCount("scrollback-limit") ?? 10_000_000)]
    }

    func terminal(for id: String) -> TerminalViewState {
        if let t = terminals[id] { return t }
        let t = TerminalViewState(controller: Self.terminals)
        t.configuration = TerminalSurfaceOptions(
            backend: .exec,
            envVars: Self.attachEnv,
            command: "\(DinoEnvironment.dinoBinary) attach --fresh \(id)",
            waitAfterCommand: false
        )
        // Unsafe pastes and programs reading or writing the clipboard ask, as Ghostty's config says.
        ClipboardConfirmation.install(on: t, session: id)
        // ⌘-clicked paths and local URLs open in dino's side pane; dropped files paste as paths.
        t.makePlatformView = { [weak self] in
            let view = LinkTerminalView(local: { self?.sessions.first { $0.id == id }?.host == nil }) { self?.openLink($0, from: id) }
            view.pointerEntered = { self?.pointerEntered(id) }
            return view
        }
        terminals[id] = t
        return t
    }

    /// `worktree`: in a new worktree and branch of the repo, so its edits stay off your checkout.
    /// `controls`: mode, model and effort; what's left open comes from Settings → Agents.
    /// `host`: over SSH on that host, in `remoteFolder` there (empty: the host's default folder).
    /// `dir`: where on this Mac, instead of the current folder.
    /// `line`: for a shell, a line typed at its prompt once it's up; `label`: its name in the sidebar.
    /// `route`: on a provider's model (Settings → Models & Providers) instead of the agent's own account.
    /// `tmux`: for a shell, the tmux session it attaches to at its prompt (dinod types it, and keeps
    /// a plain shell when tmux doesn't answer); `line` is that for a dinod that doesn't know it.
    /// `stay`: start this agent even while it's at its limit, not the one Settings → Agents names.
    func newSession(_ launcher: LauncherInfo, worktree: Bool = false, controls: Controls = Controls(), host: String? = nil, remoteFolder: String = "", in dir: String? = nil, line: String? = nil, tmux: String? = nil, label: String? = nil, route: ProviderRoute? = nil, stay: Bool = false) {
        guard let conn = connection else { return }
        var body: [String: Any] = [
            "type": "new", "launcher": launcher.short, "args": [], "cwd": dir ?? folder.path, "cols": 120, "rows": 40,
            "worktree": worktree, "controls": controls.json,
        ]
        if let line { body["prompt"] = line }
        if let tmux { body["tmux"] = tmux }
        if let route { body["route"] = ["provider": route.provider, "model": route.model] }
        if stay { body["stay"] = true }
        if let host {
            body["host"] = host
            body["cwd"] = remoteFolder.isEmpty ? nil : remoteFolder
        }
        let request = body
        Task.detached {
            do {
                let resp = try conn.request(request)
                if let label, let id = resp.id { try? conn.rename(id, to: label) }
                await MainActor.run {
                    if let host, !remoteFolder.isEmpty { self.rememberFolder(remoteFolder, on: host) }
                    if let id = resp.id { self.select(id) }
                    self.startingShell = false
                }
            } catch {
                await MainActor.run {
                    self.error = error.localizedDescription
                    self.startingShell = false
                }
            }
        }
    }

    /// A new shell in `dir` (Open in New Terminal on a worktree). False if none could start.
    @discardableResult
    func newShell(in dir: String) -> Bool {
        guard connection != nil, let l = launchers.first(where: { $0.short == "shell" }) else { return false }
        newSession(l, in: dir)
        return true
    }

    /// A new shell where you are, like a new Ghostty tab (⌘T): the folder the selected shell has
    /// moved to, when it says, else the current folder. False if none could start.
    @discardableResult
    func newShell() -> Bool {
        guard connection != nil, let l = launchers.first(where: { $0.short == "shell" }) else { return false }
        // Settings → tmux: a new tab goes straight into that tmux session (kept here
        // too, so the first tab at launch does, before dinod has answered).
        let tabs = UserDefaults.standard.string(forKey: Self.tmuxTabsKey) ?? ""
        let tmux = DinoSettings.Tmux.valid(tabs) ? tabs : nil
        newSession(l, in: selectedSession.flatMap { $0.host == nil ? $0.shell_cwd : nil }, line: tmux.map { "tmux new -A -s \($0)" }, tmux: tmux)
        return true
    }

    static let tmuxTabsKey = "tmux.newTabs"

    /// What Settings → tmux says, as dinod last had it.
    private func noteTmuxSettings(_ t: DinoSettings.Tmux?) {
        let t = t ?? .defaults
        if UserDefaults.standard.string(forKey: Self.tmuxTabsKey) != t.new_tabs {
            UserDefaults.standard.set(t.new_tabs, forKey: Self.tmuxTabsKey)
        }
        let on = t.show_agents || !t.new_tabs.isEmpty
        if on != tmuxOptionsOn { tmuxOptionsOn = on }
    }

    /// Folders sessions recently started in on `host`, newest first. Kept by the app, not in
    /// settings.toml: they're history, not configuration.
    func recentFolders(on host: String) -> [String] {
        UserDefaults.standard.stringArray(forKey: "recentFolders.\(host)") ?? []
    }

    /// A folder on this Mac a session was started in, first among the picker's recent places.
    func rememberLocalFolder(_ folder: String) { rememberFolder(folder, on: "local") }

    private func rememberFolder(_ folder: String, on host: String) {
        let list = [folder] + recentFolders(on: host).filter { $0 != folder }
        UserDefaults.standard.set(Array(list.prefix(8)), forKey: "recentFolders.\(host)")
    }

    /// Keep "On this Mac" fresh: sessions running in other terminals come and go. Only running
    /// ones are polled; finished conversations load when the browser opens.
    private func watchElsewhere() {
        Task.detached {
            var first = true
            while true {
                if let conn = try? DinoConnection(path: DinoEnvironment.socketPath), let running = try? conn.found(cloud: false, runningOnly: true) {
                    let ended = await MainActor.run { () -> Bool in
                        let before = Set(self.elsewhere.map(\.session_id))
                        let now = Set(running.map(\.session_id))
                        // An agent in a tmux pane that just started asking: tell, once.
                        let asked = Set(self.elsewhere.filter(\.asking).map(\.id))
                        for f in running where f.asking && f.tmux != nil && !asked.contains(f.id) {
                            Notifier.post(key: "tmux-\(f.id)", title: "\(f.agentName) needs you", body: "\(f.title) · in \(f.terminal ?? "tmux")")
                        }
                        let rest = self.found.filter { $0.source != "running" && !now.contains($0.session_id) }
                        if running + rest != self.found { self.found = running + rest }
                        // One that stopped is a finished conversation now.
                        return self.showContinue && !before.subtracting(now).isEmpty
                    }
                    if ended { await self.loadFound(cloud: false) }
                    await MainActor.run { self.updateBadge(self.sessions) }
                }
                // Answered or not, the sidebar needn't wait for it again.
                if first {
                    first = false
                    await MainActor.run { self.notePolled(elsewhere: true) }
                }
                try? await Task.sleep(for: .seconds(3))
            }
        }
    }

    /// The Dock badge: sessions asking for something, and agents asking in tmux panes.
    private func updateBadge(_ sessions: [SessionInfo]) {
        let waiting = sessions.filter { status(of: $0) == .needsYou }.count + elsewhere.filter { $0.asking && $0.tmux != nil }.count
        let badge = waiting > 0 ? "\(waiting)" : nil
        if NSApp.dockTile.badgeLabel != badge { NSApp.dockTile.badgeLabel = badge }
    }

    /// An agent in a tmux pane: brought to the front in the tmux client attached to its server
    /// (and that dino tab, when the client runs in one), else shown read-only. tmux keeps it.
    func showInTmux(_ f: FoundSession) {
        guard let place = f.tmux else { return }
        Task.detached {
            let shown = try? DinoConnection(path: DinoEnvironment.socketPath).tmuxShow(place)
            await MainActor.run {
                if let id = shown?.session {
                    self.select(id)
                    NSApp.activate(ignoringOtherApps: true)
                } else if shown?.tty == nil {
                    self.tmuxLook = f
                }
            }
        }
    }

    /// Everything on this Mac, then cloud sessions (slower); for the session browser.
    func loadFound() {
        loadingCloud = true
        Task {
            await loadFound(cloud: false)
            await loadFound(cloud: true)
            loadingCloud = false
        }
    }

    private func loadFound(cloud: Bool) async {
        guard let list = await Task.detached(operation: { try? DinoConnection(path: DinoEnvironment.socketPath).found(cloud: cloud) }).value else { return }
        // Without cloud, keep the cloud entries already loaded.
        let merged = cloud ? list : list + found.filter { $0.source == "cloud" }
        if merged != found { found = merged }
        loadedHistory = true
    }

    /// Ask before continuing running `f` in dino; one whose conversation dino can't tell says why
    /// instead.
    func askToMove(_ f: FoundSession) {
        if f.unsure != nil { unsureMove = f } else { confirmMove = f }
    }

    /// Move a found session into dino. A running one finishes its turn first, then continues
    /// here; its row says so meanwhile, with Cancel, and the rest of the app goes on.
    func adopt(_ f: FoundSession) {
        guard f.unsure == nil else { unsureMove = f; return }
        guard !adopting.contains(f.id) else { return }
        adopting.insert(f.id)
        let cwd = folder.path
        Task.detached {
            do {
                // Own connection: a handoff may wait minutes for the turn to end.
                let conn = try DinoConnection(path: DinoEnvironment.socketPath)
                let id = try conn.adopt(f, cwd: cwd)
                await MainActor.run {
                    self.adopting.remove(f.id)
                    self.showContinue = false
                    self.found.removeAll { $0 == f }
                    // The next poll brings the new session; select it once it's there.
                    self.pendingSelect = id
                }
            } catch {
                await MainActor.run {
                    self.adopting.remove(f.id)
                    if !Self.cancelled(error) { self.error = error.localizedDescription }
                }
            }
        }
    }

    /// Stop waiting to continue `f` in dino: it runs on where it is.
    func cancelAdopt(_ f: FoundSession) {
        // What dinod waits on it by (see adopt in dinod).
        let key = f.session_id.isEmpty ? f.pid.map(String.init) : f.session_id
        guard let key else { return }
        Task.detached { try? DinoConnection(path: DinoEnvironment.socketPath).cancelTakeOver(id: key) }
    }

    /// dinod's answer to a wait that was cancelled: nothing to tell the user.
    nonisolated static func cancelled(_ error: Error) -> Bool { error.localizedDescription == "cancelled" }

    var pendingSelect: String?
    /// One of Settings → tmux's options is on (then dino stops suggesting them).
    @Published var tmuxOptionsOn = false
    /// Per shell running tmux: the newest of its bells and notifications seen (Tabs.swift).
    @Published var tmuxSeen: [String: UInt64] = [:]
    /// The tabs along the top, by session id, in order (see Tabs.swift).
    @Published var tabs: [String] = UserDefaults.standard.stringArray(forKey: "tabs") ?? [] {
        didSet { if tabs != oldValue { UserDefaults.standard.set(tabs, forKey: "tabs") } }
    }
    /// Closes that ⌘Z can still undo (and closes undone, that Redo can do again), until each one's
    /// time is up (UndoClose.swift).
    var undoRecords: [ClosedLayout] = []
    /// A close being undone, waiting for dinod to list its shells again.
    var reopening: ClosedLayout?
    /// Sessions whose terminal reads a password, as of the last state.
    var passwordPrompts: [String] = []
    /// Opens the main window again when it was closed; set once the first one has appeared.
    var showWindow: (() -> Void)?
    /// Reveals up to here are handled: ones from before the app started (and opened it) count too.
    private var revealedUpTo = UInt64(Date().timeIntervalSince1970 * 1000) - 10_000

    /// Automations, archived sessions and launchers (keys and policies change which agents can
    /// start) refresh slower than session state. Automations are looked at even with no window
    /// showing (their runs notify); the rest only while one shows.
    private func watchSlower() {
        Task.detached {
            while true {
                if let conn = try? DinoConnection(path: DinoEnvironment.socketPath) {
                    let tasks = try? conn.scheduleList()
                    let shown = await MainActor.run { Self.isShown }
                    let launchers = shown ? try? conn.request(["type": "launchers"]).launchers : nil
                    let archived = shown ? try? conn.archived() : nil
                    await MainActor.run {
                        if let tasks { self.applySchedule(tasks) }
                        self.notePolled(schedule: true)
                        if let archived, archived != self.archived { self.archived = archived }
                        if let launchers, launchers != self.launchers { self.launchers = launchers }
                    }
                } else {
                    // A dinod that can't answer this still gets a sidebar.
                    await MainActor.run { self.notePolled(schedule: true) }
                }
                try? await Task.sleep(for: .seconds(2))
            }
        }
    }

    private func watchTree() {
        Task.detached {
            while true {
                await Self.untilShown()
                await MainActor.run { self.refreshTree() }
                try? await Task.sleep(for: .seconds(3))
            }
        }
    }

    /// The version of `repos` dinod sent, and for which folders: asked with, so an unchanged
    /// tree comes back as "the same" instead of again in full.
    private var treeVersion: (folders: [String], version: String)?

    /// Worktrees come from git, so this runs off the main thread and off the session poll.
    func refreshTree() {
        let folders = [folder.path]
        let known = treeVersion.flatMap { $0.folders == folders ? $0.version : nil }
        Task.detached {
            // A dinod that can't answer still gets a sidebar.
            let answer: (repos: [RepoInfo], version: String?)??
            do {
                answer = .some(try DinoConnection(path: DinoEnvironment.socketPath).tree(folders: folders, known: known))
            } catch {
                answer = nil
            }
            guard let answer else {
                await MainActor.run { self.notePolled(tree: true) }
                return
            }
            // An answer for a folder we've since left (on launch: home, before the first session
            // is selected) would show that folder until the next tick.
            await MainActor.run {
                guard [self.folder.path] == folders else { return }
                if let (list, version) = answer {
                    self.treeVersion = version.map { (folders, $0) }
                    if list != self.repos { self.repos = list }
                }
                self.notePolled(tree: true)
            }
        }
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
        guard let cwd = s.here else { return nil }
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
    /// The agent restarts with them, resuming its conversation; mid-turn, once the turn is over.
    func setControls(_ session: String, _ controls: Controls) {
        Task.detached {
            do {
                try DinoConnection(path: DinoEnvironment.socketPath).setControls(session: session, controls: controls)
            } catch {
                await MainActor.run { self.error = "\(error)" }
            }
        }
    }

    /// Stop a server the agent left running; the agent sees its command end.
    func stopServer(_ session: String, _ server: ServerInfo) {
        Task.detached {
            do {
                try DinoConnection(path: DinoEnvironment.socketPath).stopServer(session: session, task: server.task)
            } catch {
                await MainActor.run { self.error = error.localizedDescription }
            }
        }
    }

    /// Models this Mac's sessions of `agent` are on or have answered with, for the model menus.
    func seenModels(_ agent: String) -> [String] {
        Array(Set(sessions.filter { $0.agent == agent }.flatMap { [$0.agent_model, $0.last_model] }.compactMap { $0 })).sorted()
    }

    func setAutoPR(_ session: String, fix: Bool? = nil, merge: Bool? = nil) async throws {
        try await Task.detached { try DinoConnection(path: DinoEnvironment.socketPath).prAuto(session: session, fix: fix, merge: merge) }.value
    }

    /// The agent gets the failing checks and their logs as a prompt; you watch it work.
    func fixPR(_ session: String) async throws {
        try await Task.detached { try DinoConnection(path: DinoEnvironment.socketPath).prFix(session: session) }.value
        showPR = false
        select(session)
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

    /// Remove worktrees, and their branches where git sees them merged. dinod refuses one in use,
    /// and one with uncommitted work unless `force`, which loses that work.
    func cleanWorktrees(_ paths: [String], force: Bool = false) {
        guard !paths.isEmpty else { return }
        Task.detached {
            for path in paths {
                do {
                    _ = try DinoConnection(path: DinoEnvironment.socketPath).request(["type": "clean_worktree", "path": path, "force": force])
                } catch {
                    await MainActor.run { self.error = error.localizedDescription }
                }
            }
            await MainActor.run { self.refreshTree() }
        }
    }

    // MARK: Automations

    /// A new automation: the editor opens on its templates, for the current folder and first agent.
    func newTask() {
        editingTask = blankTask()
    }

    /// The editor, with a copy of `task` to save as a new one.
    func duplicateTask(_ task: ScheduledTask) {
        var t = task
        t.id = ""
        t.name = "\(task.name) copy"
        t.history = []
        t.state = nil
        t.problem = nil
        editingTask = t
    }

    func launcherLabel(_ short: String) -> String {
        launchers.first { $0.short == short }?.label ?? short
    }

    /// A run of it is going.
    func isRunning(_ task: ScheduledTask) -> Bool {
        guard let last = task.history.last, last.outcome == "started", last.finished_at == nil else { return false }
        return last.id != nil
    }

    /// What starts it, in words.
    func triggerText(_ t: ScheduledTask) -> String {
        let tr = t.trigger
        let repo = tr.repo.isEmpty ? "this repo" : tr.repo
        switch tr.on {
        case "schedule": return t.frequency.label
        case "pr_opened": return "PR opened in \(repo)"
        case "pr_merged": return "PR merged in \(repo)"
        case "review_requested": return tr.repo.isEmpty ? "Your review requested" : "Your review requested in \(repo)"
        case "ci_failed": return tr.branch.isEmpty ? (tr.mine ? "CI failed on your PRs" : "CI failed on a PR") + " in \(repo)" : "CI failed on \(tr.branch)"
        case "issue_labeled": return "Labeled \(tr.label) in \(repo)"
        case "comment": return "Comment says “\(tr.phrase)”"
        case "new_commits": return "New commits on \(tr.branch.isEmpty ? "the default branch" : tr.branch)"
        case "behind": return "\(tr.branch.isEmpty ? "The branch" : tr.branch) falls behind"
        case "files": return "\(tr.glob.isEmpty ? "Files" : tr.glob) changed\(tr.path.isEmpty ? "" : " in \(tr.path)")"
        case "after":
            let name = scheduled.first { $0.id == tr.after }?.name ?? sessions.first { $0.id == tr.after }.map { tabName($0) } ?? "another run"
            return tr.when == "success" ? "After \(name) succeeds" : tr.when == "failure" ? "After \(name) fails" : "After \(name)"
        default: return tr.on
        }
    }

    /// What it does, in words.
    func actionText(_ t: ScheduledTask) -> String {
        switch t.action.kind {
        case "continue": return "into \(sessions.first { $0.id == t.action.session }.map { tabName($0) } ?? "a session")"
        case "fanout": return t.action.agents.map(launcherLabel).joined(separator: ", ")
        case "command": return t.action.then_agent == "never" ? t.action.command : "\(t.action.command) → \(launcherLabel(t.launcher))"
        default: return launcherLabel(t.launcher)
        }
    }

    /// The whole automation in a sentence, as the editor shows it: "When a check fails on your pull
    /// requests in o/r, start Claude in its own worktree, then comment on the PR and notify you."
    /// `origin` is the repo an empty one means.
    func sentence(_ t: ScheduledTask, origin: String? = nil) -> String {
        let tr = t.trigger
        let repo = tr.repo.isEmpty ? (origin ?? "this folder's repo") : tr.repo
        let f = t.frequency
        let lead: String
        switch tr.on {
        case "schedule":
            switch f.every {
            case "manual": lead = "When you choose Run Now"
            case "hourly": lead = String(format: "Every hour at :%02d", f.minute)
            case "weekdays": lead = "Every weekday at \(f.time)"
            case "weekly": lead = "Every \(Calendar.current.weekdaySymbols[f.weekday]) at \(f.time)"
            default: lead = "Every day at \(f.time)"
            }
        case "pr_opened": lead = "When a pull request opens in \(repo)"
        case "pr_merged": lead = "When a pull request merges in \(repo)"
        case "review_requested": lead = tr.repo.isEmpty ? "When your review is requested in any repo" : "When your review is requested in \(repo)"
        case "ci_failed":
            lead = !tr.branch.isEmpty ? "When a check fails on \(tr.branch) in \(repo)" : tr.mine ? "When a check fails on your pull requests in \(repo)" : "When a check fails on a pull request in \(repo)"
        case "issue_labeled": lead = "When an issue gets the label “\(tr.label.isEmpty ? "…" : tr.label)” in \(repo)"
        case "comment": lead = "When a comment says “\(tr.phrase.isEmpty ? "…" : tr.phrase)” in \(repo)"
        case "new_commits": lead = "When new commits land on \(tr.branch.isEmpty ? "the default branch" : tr.branch)"
        case "behind": lead = "When \(tr.branch.isEmpty ? "the checked-out branch" : tr.branch) falls behind its upstream"
        case "files":
            let what = tr.glob.isEmpty ? "files" : tr.glob
            lead = "When \(what) change\(tr.path.isEmpty ? "" : " in \(tr.path)")"
        default:
            let name = scheduled.first { $0.id == tr.after }?.name ?? sessions.first { $0.id == tr.after }.map { tabName($0) }
            let verb = tr.when == "success" ? "succeeds" : tr.when == "failure" ? "fails" : "finishes"
            lead = name.map { "When \($0) \(verb)" } ?? "When another run \(verb)"
        }
        let agent = launcherLabel(t.launcher)
        let place = t.worktree ? " in its own worktree" : ""
        var action: String
        switch t.action.kind {
        case "continue": action = "send the prompt to \(sessions.first { $0.id == t.action.session }.map { tabName($0) } ?? "a session")"
        case "fanout": action = "start \(t.action.agents.map(launcherLabel).joined(separator: ", ")), each in its own worktree"
        case "command":
            let cmd = t.action.command.split(whereSeparator: \.isNewline).first.map(String.init) ?? ""
            action = "run “\(cmd.isEmpty ? "…" : cmd)”"
            if t.action.then_agent == "failure" { action += " and, if it fails, start \(agent)\(place)" }
            if t.action.then_agent == "always" { action += ", then start \(agent)\(place)" }
        default: action = "start \(agent)\(place)"
        }
        var only: [String] = []
        let c = t.conditions
        if c.if_changed { only.append("the repo changed") }
        if c.ac_power { only.append("your Mac is plugged in") }
        if c.lid_open { only.append("the lid is open") }
        let when = only.isEmpty ? "" : " (only if \(only.joined(separator: " and ")))"
        var then: [String] = []
        if t.output.pr_comment {
            then.append(tr.on == "comment" ? "reply in the thread" : tr.on == "issue_labeled" ? "comment on the issue" : "comment on the PR")
        }
        if t.output.notify { then.append("notify you") }
        let after = then.isEmpty ? "" : ", then \(then.joined(separator: " and "))"
        return "\(lead), \(action)\(when)\(after)."
    }

    /// The sidebar's second line: when, and what.
    func automationLine(_ t: ScheduledTask) -> String {
        "\(triggerText(t)) · \(actionText(t))"
    }

    /// Say what happened to runs nobody asked for just now: missed times made up, skips,
    /// failures, and runs that finished.
    private func applySchedule(_ tasks: [ScheduledTask]) {
        for t in tasks {
            guard let prev = scheduled.first(where: { $0.id == t.id }) else { continue }
            let seen = prev.history.last?.at ?? 0
            for run in t.history where run.at > seen && (run.due != nil || run.event != nil) {
                let when = run.due.map(whenText) ?? ""
                switch run.outcome {
                case "failed":
                    Notifier.post(key: "schedule-\(t.id)", title: "\(t.name) couldn't run", body: run.reason ?? "")
                case "skipped" where run.due != nil:
                    Notifier.post(key: "schedule-\(t.id)", title: "Skipped \(t.name) (\(when))", body: run.reason ?? "")
                case _ where run.catch_up:
                    Notifier.post(key: "schedule-\(t.id)", title: "Running \(t.name)", body: "It was due \(when), while your Mac was asleep.", session: run.session)
                default:
                    break
                }
            }
            // Nobody watched it start: say when it's done, even if it's in front of you.
            guard t.output.notify else { continue }
            for run in t.history where run.finished_at != nil {
                guard let id = run.id, prev.history.contains(where: { $0.id == id && $0.finished_at == nil }) else { continue }
                let failed = run.result == "failure"
                let body = run.summary?.firstLine ?? run.reason ?? run.event?.title ?? (failed ? "It failed" : "Ready for your review")
                Notifier.post(key: "schedule-\(t.id)", title: "\(t.name) \(failed ? "failed" : "finished")", body: body, session: run.session)
            }
        }
        if tasks != scheduled { scheduled = tasks }
    }

    /// Throws dinod's reason the automation can't run as set up (untrusted folder, no repo, …).
    func saveTask(_ task: ScheduledTask) async throws {
        let tasks = try await Task.detached { try DinoConnection(path: DinoEnvironment.socketPath).schedulePut(task) }.value
        if tasks != scheduled { scheduled = tasks }
    }

    func setTask(_ task: ScheduledTask, enabled: Bool) {
        var t = task
        t.enabled = enabled
        Task {
            do { try await saveTask(t) } catch { self.error = error.localizedDescription }
        }
    }

    func deleteTask(_ task: ScheduledTask) {
        Task.detached {
            do {
                let tasks = try DinoConnection(path: DinoEnvironment.socketPath).scheduleDelete(task.id)
                await MainActor.run { if tasks != self.scheduled { self.scheduled = tasks } }
            } catch {
                await MainActor.run { self.error = error.localizedDescription }
            }
        }
    }

    /// Start a run now and show it once dinod lists it.
    func runTask(_ task: ScheduledTask) {
        Task.detached {
            do {
                let conn = try DinoConnection(path: DinoEnvironment.socketPath)
                let id = try conn.scheduleRun(task.id)
                let tasks = try? conn.scheduleList()
                await MainActor.run {
                    if let tasks, tasks != self.scheduled { self.scheduled = tasks }
                    if let id, !id.isEmpty { self.pendingSelect = id } else { self.openTasks.insert(task.id) }
                }
            } catch {
                let tasks = try? DinoConnection(path: DinoEnvironment.socketPath).scheduleList()
                await MainActor.run {
                    if let tasks, tasks != self.scheduled { self.scheduled = tasks }
                    self.error = error.localizedDescription
                }
            }
        }
    }

    func kill(_ id: String) {
        guard let conn = connection else { return }
        Task.detached { _ = try? conn.request(["type": "kill", "id": id]) }
    }

    /// Start an ended session again in place; its pane picks it up (see `dino attach`).
    func resume(_ id: String) {
        guard let conn = connection else { return }
        Task.detached {
            do {
                try conn.resume(session: id)
            } catch {
                await MainActor.run { self.error = error.localizedDescription }
            }
        }
    }
}
