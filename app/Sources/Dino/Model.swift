import AppKit
import GhosttyTerminal
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
        case .working: "Running tools or printing"
        case .waiting: "Its turn is over; subagents or background commands still run"
        case .needsYou: "Asking for something"
        case .done: "Finished; you haven't looked yet"
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
    static let green = Color(red: 0x75 / 255, green: 0xB3 / 255, blue: 0x40 / 255)
    static let spike = Color(red: 0xFC / 255, green: 0x4F / 255, blue: 0x26 / 255)
}

@MainActor
final class DinoModel: ObservableObject {
    @Published var sessions: [SessionInfo] = []
    @Published var quotas: [QuotaInfo] = []
    /// Kept awake with the lid closed, and why sleep came back last; nil from an older dinod.
    @Published var power: PowerInfo?
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
    @Published var moving: FoundSession?
    @Published var showContinue = false
    /// A handoff waiting for the user's confirmation.
    @Published var confirmMove: FoundSession?

    /// Fan-outs, with each member's diff size.
    @Published var groups: [GroupInfo] = []
    @Published var showFanout = false
    /// The New Session sheet: agent, place, mode, model and effort.
    @Published var showNewSession = false
    @Published var showNewProject = false
    /// The toolbar's mode, model or effort picker that's open (⇧⌘M, ⇧⌘I, ⇧⌘E).
    @Published var controlPicker: ControlKind?
    /// A member whose changes the user is about to keep.
    @Published var confirmKeep: MemberInfo?
    /// A session worktree the user is about to close, and whether its changes come along.
    @Published var closingWorktree: ClosingWorktree?

    /// Prompts dinod runs on a schedule, the one open in the editor and the one about to go.
    @Published var scheduled: [ScheduledTask] = []
    @Published var editingTask: ScheduledTask?
    @Published var deletingTask: ScheduledTask?
    /// Scheduled runs that have been working, to say when they go quiet.
    private var scheduledBusy: Set<String> = []

    /// Stopped sessions kept to start again, the name being edited, and the ⌘/ sheet.
    @Published var archived: [ArchivedInfo] = []
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

    /// Rang the bell or finished while in the background; cleared when selected.
    @Published private(set) var attention: Set<String> = []
    /// Kept across relaunch, so a session that finished while dino was closed still says Done.
    @Published private(set) var unseenDone: Set<String> = Set(UserDefaults.standard.stringArray(forKey: DinoModel.unseenKey) ?? []) {
        didSet { if unseenDone != oldValue { UserDefaults.standard.set(Array(unseenDone), forKey: Self.unseenKey) } }
    }
    static let unseenKey = "unseenDone.\(DinoEnvironment.home)"

    /// Sessions shown side by side; selecting either one shows both.
    @Published var splits: [Split] = Split.saved() {
        didSet { if splits != oldValue { Split.save(splits) } }
    }
    /// Split partners dinod has started that no poll has listed yet.
    var awaited: Set<String> = []

    /// One live Ghostty surface per session, kept mounted so switching is instant.
    private(set) var terminals: [String: TerminalViewState] = [:]
    /// The session whose terminal has keyboard focus, for ⌘W.
    @Published var focusedTerminal: String?
    private(set) var connection: DinoConnection?
    private var polling = false

    private var started = false

    /// Connects to dinod and starts listening; a window opened again later reuses it.
    func start() {
        guard !started else { return }
        started = true
        Task.detached {
            do {
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
                    self.watchGroups()
                    self.watchTree()
                    self.watchGhosttyConfig()
                    self.watchTerminalSettings()
                }
            } catch {
                await MainActor.run { self.error = error.localizedDescription }
            }
        }
    }

    @Published private(set) var daemonDown = false
    /// dinod isn't the dino this app carries (it's from before an update); see Updates.swift.
    @Published var daemonOutdated = false
    @Published var daemonVersion: String?
    @Published var restartingDaemon = false
    /// The automatic restart into a new dino happens once a launch at most.
    private var restartedForUpdate = false

    /// dinod's tag for the state last applied: it answers once there's something else to show.
    private var stateSeen: UInt64?
    /// The waits have a connection of their own: on the shared one, every request would wait too.
    private var stateConnection: DinoConnection?
    /// False with a dinod too old to wait for a change: then ask four times a second.
    private var stateWaits = true

    private func poll() {
        guard polling, let conn = connection else { return }
        if stateWaits, stateConnection == nil { stateConnection = try? DinoConnection(path: DinoEnvironment.socketPath) }
        let wait = stateWaits ? stateConnection : nil
        var body: [String: Any] = ["type": wait == nil ? "state" : "state_change"]
        body["seen"] = stateSeen
        let request = body
        Task.detached {
            // An error answer, rather than none: a dinod too old to wait.
            let (resp, older): (Response?, Bool) = {
                do { return (try (wait ?? conn).request(request), false) }
                catch DinoError.daemon { return (nil, wait != nil) }
                catch { return (nil, false) }
            }()
            await MainActor.run {
                if older {
                    self.stateWaits = false
                    self.stateConnection = nil
                    self.poll()
                } else if let resp {
                    self.apply(resp.sessions ?? [], resp.quotas ?? [])
                    self.applyPower(resp.power)
                    self.stateSeen = resp.version
                    if wait != nil {
                        self.poll()
                    } else {
                        DispatchQueue.main.asyncAfter(deadline: .now() + 0.25) { self.poll() }
                    }
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
        stateWaits = true
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
            Notifier.post(key: "lid", title: "Closing the lid sleeps the Mac again", body: note)
        }
        power = next
    }

    private func apply(_ next: [SessionInfo], _ quotas: [QuotaInfo]) {
        // The quick terminal's shell is its own, not a session in the sidebar.
        QuickTerminal.shared.sessionAlive = next.contains { $0.id == QuickTerminal.shared.sessionID && !$0.exited }
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
        for s in next {
            guard let prev = sessions.first(where: { $0.id == s.id }) else { continue }
            let looking = appActive && s.id == selected
            if !looking {
                if s.bells > prev.bells { attention.insert(s.id) }
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
            }
            // Nobody watched a scheduled run start: say when it's done, even if it's in front of you.
            if let task = s.scheduled {
                let busy = !s.exited && (s.activity == "working" || s.waitingOn != nil || s.in_flight > 0 || (s.activity == nil && (s.output_ms_ago ?? .max) < 5000))
                if busy {
                    scheduledBusy.insert(s.id)
                } else if scheduledBusy.remove(s.id) != nil {
                    Notifier.post(session: s, title: "\(task) finished", body: s.title ?? (s.exited ? "The session ended" : "Ready for your review"))
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
        if let id = selected, let here = next.first(where: { $0.id == id })?.here,
           here != sessions.first(where: { $0.id == id })?.here { folder = URL(fileURLWithPath: here) }
        placeHandedOff(next)
        if next != sessions { sessions = next }
        syncTabs(next)
        if quotas != self.quotas { self.quotas = quotas }
        let live = Set(next.map(\.id))
        terminals = terminals.filter { live.contains($0.key) }
        if !unseenDone.isSubset(of: live) { unseenDone.formIntersection(live) }
        webPages = webPages.filter { $0.key.isEmpty || live.contains($0.key) }
        for s in next { webPages[s.id]?.follow(s.previews ?? []) }
        // A pane whose session ended closes, as it would in a terminal.
        awaited.subtract(live)
        let kept = splits.filter { [$0.first, $0.second].allSatisfy { live.contains($0) || awaited.contains($0) } }
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
        // Rows that aren't sessions (folders, worktree lists, fan-outs, subagents, tasks, sessions on
        // this Mac) carry a "kind:" prefix: one of those stays selected. Only a session that's gone
        // falls back to another, or a click on "Other worktrees" would jump straight back.
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
        // After an update: into the new dinod once nothing would be cut off.
        if daemonOutdated, !restartedForUpdate, restartIsQuiet {
            restartedForUpdate = true
            restartDaemon()
        }
        let waiting = next.filter { status(of: $0) == .needsYou }.count
        let badge = waiting > 0 ? "\(waiting)" : nil
        if NSApp.dockTile.badgeLabel != badge { NSApp.dockTile.badgeLabel = badge }
    }

    /// A terminal title without the spinner or status glyphs an agent puts before its words.
    static func undecorated(_ title: String) -> String? {
        let t = title.drop { !$0.isLetter && !$0.isNumber && $0 != "~" && $0 != "/" && $0 != "." }.trimmingCharacters(in: .whitespaces)
        return t.isEmpty ? nil : t
    }

    func status(of s: SessionInfo) -> SessionStatus {
        if s.exited { return (s.exit_code ?? 0) == 0 ? .ended : .exited }
        if attention.contains(s.id) || s.needs != nil { return .needsYou }
        // An agent run by hand in a shell says whether it's busy; its shell has no hooks.
        if let f = s.inside, let st = f.status {
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

    func select(_ id: String?) {
        selected = id
        guard let id else { return }
        if id.hasPrefix("dir:") {
            folder = URL(fileURLWithPath: String(id.dropFirst(4)))
            return
        }
        openTab(id)
        shownOne = true
        // New sessions start next to the one you're looking at.
        if let cwd = sessions.first(where: { $0.id == id })?.here { folder = URL(fileURLWithPath: cwd) }
        attention.remove(id)
        unseenDone.remove(id)
        terminals[id]?.requestFocus()
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
    /// clears), so a focused pane would swallow dino's. Hand those keys back to the menu, over
    /// whatever the user's Ghostty config binds them to.
    static let menuKeys = ((["d", "alt+d", "shift+d", "w", "k", "j", "o", "n", "t", "shift+n", "alt+n", "comma", "shift+backspace", "s", "shift+o", "alt+p", "alt+t"]
        + (1 ... 9).flatMap { ["\($0)", "digit_\($0)"] })
        .map { "super+\($0)" }
        // Ctrl+Tab cycles sessions, ⌘/ lists shortcuts, ⇧⌘A archives, ⇧⌘F finds sessions.
        + ["ctrl+tab", "ctrl+shift+tab", "super+slash", "super+shift+a", "super+shift+f"])
        .map { "keybind = \($0)=unbind" }.joined(separator: "\n")

    static let terminals: TerminalController = {
        let c = TerminalController(configSource: .generated(menuKeys))
        GhosttyConfig.apply(to: c, overrides: menuKeys)
        return c
    }()

    /// Picks up edits to the Ghostty config, as Ghostty does when told to reload.
    private func watchGhosttyConfig() {
        // Read now, not at the first pane: Settings says what's in effect.
        _ = Self.terminals
        Timer.scheduledTimer(withTimeInterval: 2, repeats: true) { _ in
            MainActor.assumeIsolated {
                if GhosttyConfig.changed { GhosttyConfig.apply(to: Self.terminals, overrides: Self.menuKeys) }
            }
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
                      var settings = try? conn.settings(), let there = settings.terminal else { return }
                let (here, agreed) = await MainActor.run { (DinoSettings.Terminal.mirrored, self.agreedTerminal) }
                // First look: an app that had these before dinod kept them hands them over once.
                let fromApp = agreed.map { here != $0 && there == $0 } ?? (there == .defaults && here != .defaults)
                if fromApp {
                    settings.terminal = here
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
        Timer.scheduledTimer(withTimeInterval: 5, repeats: true) { _ in check() }
    }

    func terminal(for id: String) -> TerminalViewState {
        if let t = terminals[id] { return t }
        let t = TerminalViewState(controller: Self.terminals)
        t.configuration = TerminalSurfaceOptions(
            backend: .exec,
            envVars: ["PATH": DinoEnvironment.loginPath, "DINO_HOME": DinoEnvironment.home],
            command: "\(DinoEnvironment.dinoBinary) attach --fresh \(id)",
            waitAfterCommand: false
        )
        // ⌘-clicked paths and local URLs open in dino's side pane; dropped files paste as paths.
        t.makePlatformView = { [weak self] in
            LinkTerminalView(local: { self?.sessions.first { $0.id == id }?.host == nil }) { self?.openLink($0, from: id) }
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
    func newSession(_ launcher: LauncherInfo, worktree: Bool = false, controls: Controls = Controls(), host: String? = nil, remoteFolder: String = "", in dir: String? = nil, line: String? = nil, label: String? = nil, route: ProviderRoute? = nil) {
        guard let conn = connection else { return }
        var body: [String: Any] = [
            "type": "new", "launcher": launcher.short, "args": [], "cwd": dir ?? folder.path, "cols": 120, "rows": 40,
            "worktree": worktree, "controls": controls.json,
        ]
        if let line { body["prompt"] = line }
        if let route { body["route"] = ["provider": route.provider, "model": route.model] }
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

    /// A new shell where you are, like a new Ghostty tab (⌘T): the folder the selected shell has
    /// moved to, when it says, else the current folder. False if none could start.
    @discardableResult
    func newShell() -> Bool {
        guard connection != nil, let l = launchers.first(where: { $0.short == "shell" }) else { return false }
        newSession(l, in: selectedSession.flatMap { $0.host == nil ? $0.shell_cwd : nil })
        return true
    }

    /// Folders sessions recently started in on `host`, newest first. Kept by the app, not in
    /// settings.toml: they're history, not configuration.
    func recentFolders(on host: String) -> [String] {
        UserDefaults.standard.stringArray(forKey: "recentFolders.\(host)") ?? []
    }

    private func rememberFolder(_ folder: String, on host: String) {
        let list = [folder] + recentFolders(on: host).filter { $0 != folder }
        UserDefaults.standard.set(Array(list.prefix(8)), forKey: "recentFolders.\(host)")
    }

    /// Keep "On this Mac" fresh: sessions running in other terminals come and go. Only running
    /// ones are polled; finished conversations load when the browser opens.
    private func watchElsewhere() {
        Task.detached {
            while true {
                if let conn = try? DinoConnection(path: DinoEnvironment.socketPath), let running = try? conn.found(cloud: false, runningOnly: true) {
                    let ended = await MainActor.run { () -> Bool in
                        let before = Set(self.elsewhere.map(\.session_id))
                        let now = Set(running.map(\.session_id))
                        let rest = self.found.filter { $0.source != "running" && !now.contains($0.session_id) }
                        if running + rest != self.found { self.found = running + rest }
                        // One that stopped is a finished conversation now.
                        return self.showContinue && !before.subtracting(now).isEmpty
                    }
                    if ended { await self.loadFound(cloud: false) }
                }
                try? await Task.sleep(for: .seconds(3))
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
    /// The tabs along the top, by session id, in order (see Tabs.swift).
    @Published var tabs: [String] = UserDefaults.standard.stringArray(forKey: "tabs") ?? [] {
        didSet { if tabs != oldValue { UserDefaults.standard.set(tabs, forKey: "tabs") } }
    }
    /// Shells whose tab was put away without ending them (dropped from a split): not reopened.
    var knownTabless = Set<String>()
    /// Opens the main window again when it was closed; set once the first one has appeared.
    var showWindow: (() -> Void)?
    /// Reveals up to here are handled: ones from before the app started (and opened it) count too.
    private var revealedUpTo = UInt64(Date().timeIntervalSince1970 * 1000) - 10_000

    /// Continue the agent started by hand in shell `s` as a dino session, in the same row.
    func takeOver(_ s: SessionInfo) {
        guard let f = s.inside else { return }
        moving = f
        let id = s.id
        Task.detached {
            do {
                // Own connection: it waits for the agent's turn to end.
                try DinoConnection(path: DinoEnvironment.socketPath).takeOver(session: id)
                await MainActor.run { self.moving = nil }
            } catch {
                await MainActor.run {
                    self.moving = nil
                    self.error = error.localizedDescription
                }
            }
        }
    }

    /// Diff sizes need git, so these refresh slower than session state. Launchers too: keys and
    /// policies change which agents can start.
    private func watchGroups() {
        Task.detached {
            while true {
                if let conn = try? DinoConnection(path: DinoEnvironment.socketPath), let list = try? conn.groups() {
                    let launchers = try? conn.request(["type": "launchers"]).launchers
                    let tasks = try? conn.scheduleList()
                    let archived = try? conn.archived()
                    await MainActor.run {
                        if let tasks { self.applySchedule(tasks) }
                        if let archived, archived != self.archived { self.archived = archived }
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
            // An answer for a folder we've since left (on launch: home, before the first session
            // is selected) would show that folder until the next tick.
            await MainActor.run { if [self.folder.path] == folders, list != self.repos { self.repos = list } }
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

    /// Models this Mac's sessions of `agent` have answered with, for the model menus.
    func seenModels(_ agent: String) -> [String] {
        Array(Set(sessions.filter { $0.agent_id == agent }.compactMap(\.last_model))).sorted()
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

    // MARK: Scheduled tasks

    /// A new task as the editor opens it: daily at 9 in the current folder, with the first agent.
    func newTask() {
        var t = ScheduledTask()
        t.cwd = folder.path
        t.launcher = launchers.first { $0.agent_id != "shell" }?.short ?? launchers.first?.short ?? ""
        editingTask = t
    }

    func launcherLabel(_ short: String) -> String {
        launchers.first { $0.short == short }?.label ?? short
    }

    /// Say what happened to runs nobody asked for just now: missed times made up, skips, failures.
    private func applySchedule(_ tasks: [ScheduledTask]) {
        for t in tasks {
            guard let prev = scheduled.first(where: { $0.id == t.id }) else { continue }
            let seen = prev.history.last?.at ?? 0
            for run in t.history where run.at > seen && run.due != nil {
                let when = run.due.map(whenText) ?? ""
                switch run.outcome {
                case "failed":
                    Notifier.post(key: "schedule-\(t.id)", title: "\(t.name) couldn't run", body: run.reason ?? "")
                case "skipped":
                    Notifier.post(key: "schedule-\(t.id)", title: "Skipped \(t.name) (\(when))", body: run.reason ?? "")
                case _ where run.catch_up:
                    Notifier.post(key: "schedule-\(t.id)", title: "Running \(t.name)", body: "It was due \(when), while your Mac was asleep.", session: run.session)
                default:
                    break
                }
            }
        }
        if tasks != scheduled { scheduled = tasks }
    }

    /// Throws dinod's reason the task can't run as set up (untrusted folder, no repo for a worktree, …).
    func saveTask(_ task: ScheduledTask) async throws {
        let tasks = try await Task.detached { try DinoConnection(path: DinoEnvironment.socketPath).schedulePut(task) }.value
        scheduled = tasks
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
                await MainActor.run { self.scheduled = tasks }
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
                    if let tasks { self.scheduled = tasks }
                    self.pendingSelect = id
                }
            } catch {
                let tasks = try? DinoConnection(path: DinoEnvironment.socketPath).scheduleList()
                await MainActor.run {
                    if let tasks { self.scheduled = tasks }
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
