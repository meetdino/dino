import AppKit
import DinoGhostty
import SwiftUI

@main
struct DinoApp: App {
    @NSApplicationDelegateAdaptor private var delegate: AppDelegate
    @StateObject private var model = DinoModel()
    @AppStorage(QuitChoice.key) private var quitChoice = ""
    @AppStorage(Appearance.key) private var appearance = Appearance.system.rawValue
    @Environment(\.openWindow) private var openWindow

    init() {
        // Before anything starts a program: a build for an isolated dino passes its $DINO_HOME on.
        _ = DinoEnvironment.home
    }

    var body: some Scene {
        WindowGroup("dino", id: "main") {
            ContentView()
                .environmentObject(model)
                .frame(minWidth: 820, minHeight: 480)
                .onAppear {
                    delegate.model = model
                    delegate.reopenWindow = { openWindow(id: "main") }
                    model.showWindow = { openWindow(id: "main") }
                    SyncStore.shared.showAccount = {
                        SettingsPane.account.select()
                        openWindow(id: SettingsView.windowID)
                    }
                    Notifier.onOpenSession = { id in
                        // The quick terminal's own shell isn't a tab: it drops down instead.
                        if id == QuickTerminal.shared.sessionID { QuickTerminal.shared.show() } else { model.select(id) }
                    }
                    Notifier.setUp()
                    model.start()
                    // Back in a reopened window: the session you had keeps the keyboard.
                    if let id = model.selected { model.select(id) }
                }
        }
        .windowStyle(.hiddenTitleBar)
        // The first time; after that SwiftUI opens it as you left it (it saves the frame itself).
        .defaultSize(width: 1280, height: 800)
        .commands {
            CommandGroup(replacing: .appSettings) {
                Button("Settings…") { openWindow(id: SettingsView.windowID) }
                    .keyboardShortcut(",")
                SecureInputCommand()
                Button("Usage Stats…") { openWindow(id: StatsView.windowID) }
                    .keyboardShortcut("u", modifiers: [.command, .shift])
                UpdateMenuItem()
                Button("Ask Before Quitting") { quitChoice = "" }
                    .disabled(quitChoice.isEmpty)
                    .help("Show the keep-running question again when you quit")
                Button("Install Command Line Tool…") { CommandLineTool.install() }
                    .disabled(DinoEnvironment.bundledDino == nil)
                    .help("Put the dino command this app carries on your PATH")
            }
            // One window: ⌘N starts a session rather than opening a second window.
            CommandGroup(replacing: .newItem) {}
            CommandGroup(replacing: .saveItem) {
                CloseCommand().environmentObject(model)
                SaveCommand().environmentObject(model)
                Button("Open File…") { model.chooseFile() }
                    .keyboardShortcut("o", modifiers: [.command, .shift])
            }
            // The terminal's own: Clear, Reset Terminal, Find.
            CommandGroup(after: .pasteboard) {
                TerminalEditItems(model: model)
            }
            CommandMenu("Session") {
                Button(CommandPalette.paletteTitle) { model.showPalette = true }
                    .keyboardShortcut("p", modifiers: [.command, .shift])
                Divider()
                Button("New Tab") { model.newShell() }
                    .keyboardShortcut("t")
                    .disabled(model.launchers.isEmpty)
                Button("New Project…") { model.showNewProject = true }
                    .keyboardShortcut("n", modifiers: [.command, .option, .shift])
                    .disabled(model.launchers.isEmpty)
                // Its shortcut works from any app (Settings → Terminal); a menu key would only work here.
                Button("Quick Terminal") { QuickTerminal.shared.toggle() }
                    .disabled(model.launchers.isEmpty)
                // ⌘N: the default agent where you are, like a new tab; no questions.
                Button("New Session") { model.newSessionHere() }
                    .keyboardShortcut("n")
                    .disabled(model.launchers.isEmpty)
                // Anywhere: where (here, recent, GitHub, a URL, a new project), then which agent.
                Button("New Session…") { model.startSession() }
                    .disabled(model.launchers.isEmpty)
                Button("New Session in Worktree…") { model.startSession(worktree: true) }
                    .keyboardShortcut("n", modifiers: [.command, .option])
                    .disabled(model.launchers.isEmpty)
                Button("New Session with Options…") { model.showNewSession = true }
                    .keyboardShortcut("n", modifiers: [.command, .control])
                // Experimental: only while it's on (Settings → Experimental).
                if model.fanoutOn {
                    Button("Fan Out…") { model.showFanout = true }
                        .keyboardShortcut("n", modifiers: [.command, .shift])
                }
                Button("New Automation…") { model.newTask() }
                Button("Continue a Session…") {
                    model.loadFound()
                    model.showContinue = true
                }
                .keyboardShortcut("k")
                Button("Open Folder…") { model.openFolderToStart() }
                    .keyboardShortcut("o")
                Divider()
                SplitMenuItems().environmentObject(model)
                Divider()
                Button("Find Sessions…") { model.findingSessions = true }
                    .keyboardShortcut("f", modifiers: [.command, .shift])
                Button("Jump to Session Needing You") { model.jumpToAttention() }
                    .keyboardShortcut("j")
                Button("Next Tab") { model.cycleTabs(by: 1) }
                    .keyboardShortcut("]", modifiers: [.command, .shift])
                    .disabled(model.shownTabs.count < 2)
                Button("Previous Tab") { model.cycleTabs(by: -1) }
                    .keyboardShortcut("[", modifiers: [.command, .shift])
                    .disabled(model.shownTabs.count < 2)
                Button("Next Session") { model.cycle(by: 1) }
                    .keyboardShortcut(.tab, modifiers: .control)
                    .disabled(model.sidebarSessions.count < 2)
                Button("Previous Session") { model.cycle(by: -1) }
                    .keyboardShortcut(.tab, modifiers: [.control, .shift])
                    .disabled(model.sidebarSessions.count < 2)
                Button(model.showReview ? "Hide Changes" : "Review Changes") { model.showReview.toggle() }
                    .keyboardShortcut("d", modifiers: [.command, .shift])
                    .disabled(!model.showReview && model.selectedSession?.host != nil)
                Button(model.sidePane == .preview ? "Hide Preview" : "Show Preview") { model.togglePreview() }
                    .keyboardShortcut("p", modifiers: [.command, .option])
                    .disabled(model.sidePane != .preview && model.selectedSession?.host != nil)
                Button(model.sidePane == .tasks ? "Hide Tasks" : "Show Tasks") { model.toggleTasks() }
                    .keyboardShortcut("t", modifiers: [.command, .option])
                    .disabled(model.sidePane != .tasks && !(model.selectedSession?.reportsTasks ?? false))
                Button("Ask About This Session…") { model.askingAbout = model.selectedSession }
                    .keyboardShortcut(";", modifiers: [.command, .shift])
                    .disabled(model.selectedSession == nil)
                Button("Create Pull Request…") { model.showCreatePR = true }
                    .disabled(model.selectedSession.map { $0.host != nil || model.pr(of: $0) != nil } ?? true)
                OpenInMenuItems().environmentObject(model)
                Divider()
                ControlMenuItems().environmentObject(model)
                Divider()
                // ⌘1…⌘9: the tabs, as in Ghostty and browsers.
                ForEach(Array(model.shownTabs.prefix(9).enumerated()), id: \.element) { i, id in
                    if let s = model.sessions.first(where: { $0.id == id }) {
                        Button("\(i + 1)  \(s.display)") { model.select(id) }
                            .keyboardShortcut(KeyEquivalent(Character("\(i + 1)")))
                    }
                }
                Divider()
                Button(model.selectedSession?.pinned == true ? "Unpin" : "Pin") {
                    if let s = model.selectedSession { model.pin(s.id, s.pinned != true) }
                }
                .disabled(model.selectedSession == nil)
                Button("Mark as Unread") { if let id = model.selectedSession?.id { model.markUnread(id) } }
                    .disabled(model.selectedSession == nil || model.selectedSession?.exited == true)
                Button("Rename…") { if let id = model.selectedSession?.id { model.renaming = Renaming(id: id, place: .toolbar) } }
                    .disabled(model.selectedSession == nil)
                Button("Archive") { if let id = model.selectedSession?.id { model.archive(id) } }
                    .keyboardShortcut("a", modifiers: [.command, .shift])
                    .disabled(!model.canArchive(model.selectedSession?.id ?? ""))
                Button("Show Archived") { model.showArchived() }
                Button("Close Session") { if let id = model.selected { model.closeSession(id) } }
                    .keyboardShortcut(.delete, modifiers: [.command, .shift])
                    .disabled(model.selected == nil)
                // No key: ⌘⌫ is the terminal's (Ghostty deletes to the start of the line with it).
                Button("Delete Session…") { if let id = model.selectedSession?.id { model.confirmDelete(id) } }
                    .disabled(model.selectedSession == nil)
            }
            CommandGroup(after: .toolbar) {
                Picker("Appearance", selection: Binding(get: { appearance }, set: { (Appearance(rawValue: $0) ?? .system).choose() })) {
                    ForEach(Appearance.allCases) { Text($0.label).tag($0.rawValue) }
                }
                .pickerStyle(.inline)
            }
            CommandGroup(replacing: .help) {
                Button("Keyboard Shortcuts") { model.showShortcuts = true }
                    .keyboardShortcut("/")
                Button("Show Welcome") { model.showWelcome = true }
            }
        }
        Window("Settings", id: SettingsView.windowID) {
            SettingsView()
                .environmentObject(model)
        }
        .windowResizability(.contentSize)
        .windowToolbarStyle(.unified)
        Window("Usage Stats", id: StatsView.windowID) {
            StatsView()
        }
        .defaultSize(width: 1040, height: 760)
        .windowToolbarStyle(.unified)
    }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationWillFinishLaunching(_: Notification) {
        // Before the first window draws: no flash of the Mac's look when dino is set otherwise.
        Appearance.current.apply()
        // Opening the app shows the sessions, even if it quit while looking at the archive.
        if UserDefaults.standard.string(forKey: "sidebar.filter") == SessionFilter.archived.rawValue {
            UserDefaults.standard.set(SessionFilter.all.rawValue, forKey: "sidebar.filter")
        }
    }

    func applicationDidFinishLaunching(_: Notification) {
        // Run as a regular app with a Dock icon and menu bar even when launched from a binary.
        NSApp.setActivationPolicy(.regular)
        NSApp.activate(ignoringOtherApps: true)
        NSApp.servicesProvider = services
        // Sparkle schedules its daily check from here (a release build only).
        _ = Updates.shared
        NSUpdateDynamicServices()
        QuickTerminal.shared.registerKey()
        desktopKeys = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] e in
            if e.window is QuickPanel { return QuickTerminal.shared.key(e) ? nil : e }
            // Esc closes the toolbar's pickers wherever the keyboard is: their rows are buttons,
            // which don't take Esc themselves.
            if e.keyCode == 53, let model = self?.model, model.controlPicker != nil || model.showPR || model.showPalette {
                model.controlPicker = nil
                model.showPR = false
                model.showPalette = false
                return nil
            }
            // Esc while the session in front waits to continue in dino stops the wait, rather than
            // reaching its agent (where it would interrupt the turn being waited on).
            if e.keyCode == 53, e.modifierFlags.intersection([.command, .option, .control, .shift]).isEmpty,
               let model = self?.model, let w = e.window, !(w is NSPanel), w.attachedSheet == nil,
               w.identifier?.rawValue != SettingsView.windowID,
               let s = model.selectedSession, model.isTakingOver(s) {
                model.cancelTakeOver(s.id)
                return nil
            }
            guard let model = self?.model, let w = e.window, !(w is NSPanel), w.attachedSheet == nil,
                  w.identifier?.rawValue != SettingsView.windowID,
                  Self.desktopKey(e, model: model) else { return e }
            return nil
        }
    }

    private var desktopKeys: Any?
    private let services = ServiceProvider()

    /// Back at the app: settings may have changed on another Mac, so sync looks now rather than at
    /// its next minute.
    func applicationDidBecomeActive(_: Notification) {
        Task.detached { _ = try? DinoConnection(path: DinoEnvironment.socketPath).send(["type": "sync", "action": "now"]) }
    }

    /// Folders, scripts and man-page links opened with dino (Finder, `open -a`, a default
    /// terminal's files).
    func application(_: NSApplication, open urls: [URL]) {
        guard let model else {
            Opening.pending += urls
            return
        }
        model.open(urls)
    }

    /// Claude desktop's keys. Some are aliases for what the menus have under dino's own keys (a menu
    /// item shows one); the rest are in the Split menu too, but handled here so they work even when
    /// SwiftUI hasn't brought the menu's enabled state up to date. Seen before the terminal, which
    /// would otherwise take ⇧⌘] and ⇧⌘[ for tabs.
    private static func desktopKey(_ e: NSEvent, model: DinoModel) -> Bool {
        let mods = e.modifierFlags.intersection([.command, .shift, .option, .control])
        switch (mods, e.charactersIgnoringModifiers ?? "") {
        case ([.control], "`"):
            guard model.selectedSession != nil else { return false }
            model.toggleTerminal()
        case ([.command], "\\"):
            guard model.shownSplit != nil || model.sidePane != nil else { return false }
            model.closeFocusedPane()
        case ([.command, .shift], "]"), ([.command, .shift], "}"):
            guard model.sessions.count > 1 else { return false }
            model.cycle(by: 1)
        case ([.command, .shift], "["), ([.command, .shift], "{"):
            guard model.sessions.count > 1 else { return false }
            model.cycle(by: -1)
        case ([.command, .shift], "b"), ([.command, .shift], "B"):
            guard model.sidePane == .preview || model.selectedSession?.host == nil else { return false }
            model.togglePreview()
        case ([.command], ";"):
            guard let s = model.selectedSession else { return false }
            model.askingAbout = s
        case ([.command, .shift], "p"), ([.command, .shift], "P"):
            model.showPalette = true
        // The terminal pastes text only; an image on its own is written to a file and pasted as
        // its path, which Claude Code and Codex attach.
        case ([.command], "v"):
            guard let view = e.window?.firstResponder as? LinkTerminalView else { return false }
            return view.pasteClipboardImage()
        // The shell's AI line (`dino init`): ⌘I asks, ⌘⏎ hands the line to an agent. Only a shell
        // at its prompt gets them; an agent's own terminal keeps its keys.
        case ([.command], "i"), ([.command], "\r"):
            guard let shell = model.shellAtPrompt else { return false }
            model.sendKeys(shell, e.charactersIgnoringModifiers == "i" ? "\u{1b}[57300~" : "\u{1b}[57301~")
        default:
            return false
        }
        return true
    }

    // Closing the window (⌘W with nothing else to close) hides it, as in Ghostty: dino keeps running
    // and clicking its Dock icon brings the window back. Quitting is ⌘Q.
    func applicationShouldTerminateAfterLastWindowClosed(_: NSApplication) -> Bool { false }

    /// Opens the main window again; set once the first one has appeared.
    var reopenWindow: (() -> Void)?

    /// Clicking the Dock icon with the window closed brings it back.
    func applicationShouldHandleReopen(_: NSApplication, hasVisibleWindows visible: Bool) -> Bool {
        if !visible { reopenWindow?() }
        return true
    }

    weak var model: DinoModel? {
        didSet {
            services.model = model
            QuickTerminal.shared.model = model
            Updates.shared.model = model
        }
    }

    /// Agents run in dinod, not in the app, so quitting leaves them running unless you say otherwise.
    /// Quitting to install an update restarts dinod instead, into the new dino (Updates.swift).
    func applicationShouldTerminate(_: NSApplication) -> NSApplication.TerminateReply {
        if let model, let quit = Updates.shared.quitForUpdate(model) {
            return quit ? .terminateNow : .terminateCancel
        }
        guard let model, !model.daemonDown, !model.sessions.isEmpty, !QuitChoice.systemIsGoingDown else {
            return .terminateNow
        }
        let choice = QuitChoice(rawValue: UserDefaults.standard.string(forKey: QuitChoice.key) ?? "") ?? ask(model)
        switch choice {
        case .keep: return .terminateNow
        case .stop:
            model.stopDaemon()
            return .terminateNow
        case .cancel: return .terminateCancel
        }
    }

    private func ask(_ model: DinoModel) -> QuitChoice {
        let count = model.sessions.count
        let working = model.sessions.filter { [.thinking, .working, .waiting, .needsYou].contains(model.status(of: $0)) }.count
        // Shells aren't agents: "your 2 agents" for one agent and a shell said what isn't there.
        let agents = model.sessions.filter { !$0.plainShell }.count
        let shells = count - agents
        let what = agents == 0 ? (shells == 1 ? "shell" : "\(shells) shells")
            : shells == 0 ? (agents == 1 ? "agent" : "\(agents) agents")
            : "\(count) sessions"
        let alert = NSAlert()
        alert.messageText = "Keep your \(what) running?"
        alert.informativeText = (working > 0 ? "\(working) \(working == 1 ? "is" : "are") working right now. " : "")
            + "They carry on in the background while dino is closed; open dino to pick up where you left off."
            + " Stopping pauses them, and they resume the next time dino starts."
        alert.addButton(withTitle: "Keep Running")
        alert.addButton(withTitle: count == 1 ? "Stop It" : "Stop All")
        alert.addButton(withTitle: "Cancel")
        alert.showsSuppressionButton = true
        alert.suppressionButton?.title = "Don't ask again"
        let choice: QuitChoice = switch alert.runModal() {
        case .alertFirstButtonReturn: .keep
        case .alertSecondButtonReturn: .stop
        default: .cancel
        }
        // Remember the button, not just "don't ask": the next quit does the same thing.
        if choice != .cancel, alert.suppressionButton?.state == .on {
            UserDefaults.standard.set(choice.rawValue, forKey: QuitChoice.key)
        }
        return choice
    }
}

/// What opening dino shows: the session you left (a shell if there's none), or always a new shell.
enum StartWith: String {
    case last, shell
    static let key = "startWith"
    static var current: StartWith { UserDefaults.standard.string(forKey: key).flatMap(StartWith.init) ?? .last }
}

/// The app's look, in the app and its terminal panes alike (they follow the window's appearance):
/// the Mac's, or light or dark whatever the Mac is set to.
enum Appearance: String, CaseIterable, Identifiable {
    case system, light, dark
    static let key = "appearance"
    static var current: Appearance { UserDefaults.standard.string(forKey: key).flatMap(Appearance.init) ?? .system }
    var id: String { rawValue }

    var label: String {
        switch self {
        case .system: "System"
        case .light: "Light"
        case .dark: "Dark"
        }
    }

    /// Chosen in the View menu or Settings: kept, and applied at once.
    @MainActor func choose() {
        UserDefaults.standard.set(rawValue, forKey: Self.key)
        apply()
    }

    /// Every window, sheet and pane at once; System hands it back to the Mac (and follows it live).
    @MainActor func apply() {
        let name: NSAppearance.Name? = switch self {
        case .system: nil
        case .light: .aqua
        case .dark: .darkAqua
        }
        guard NSApp.appearance?.name != name else { return }
        NSApp.appearance = name.flatMap(NSAppearance.init(named:))
    }
}

enum QuitChoice: String {
    case keep, stop, cancel
    static let key = "quitChoice"

    /// Logout, restart and shutdown quit apps too; never hold those up with a question.
    static var systemIsGoingDown: Bool {
        guard let event = NSAppleEventManager.shared().currentAppleEvent,
              let reason = event.attributeDescriptor(forKeyword: kAEQuitReason)?.enumCodeValue
        else { return false }
        return [kAEShutDown, kAERestart, kAEReallyLogOut].map { OSType($0) }.contains(reason)
    }
}

struct ContentView: View {
    @EnvironmentObject var model: DinoModel

    var body: some View {
        NavigationSplitView {
            Sidebar()
                .navigationSplitViewColumnWidth(min: 230, ideal: 260, max: 340)
        } detail: {
            HSplitView {
                VStack(spacing: 0) {
                    TabStrip()
                    TmuxSuggestion()
                    ComputerUseBanner()
                    TakeOverBanner()
                    UpdateBanner()
                    Terminals()
                }
                if let pane = model.sidePane {
                    SidePaneView(pane: pane)
                        .frame(minWidth: 320, idealWidth: 520, maxWidth: .infinity)
                }
                if model.showReview, let s = model.sessions.first(where: { $0.id == model.selected }) {
                    ReviewPanel(session: s)
                        .frame(minWidth: 340, idealWidth: 480, maxWidth: 900)
                }
            }
        }
        .sheet(isPresented: $model.showContinue) { ContinueSheet() }
        .sheet(isPresented: $model.showFanout) { FanoutSheet() }
        .sheet(isPresented: $model.showNewSession) { NewSessionSheet() }
        .sheet(item: $model.startRequest) { StartSessionSheet(request: $0) }
        .sheet(item: $model.tmuxLook) { TmuxLook(session: $0) }
        .sheet(isPresented: $model.showNewProject) { NewProjectSheet() }
        .sheet(isPresented: $model.showShortcuts) { ShortcutSheet() }
        .sheet(isPresented: $model.showPalette) { CommandPalette() }
        .sheet(item: $model.askingAbout) { AskSheet(session: $0) }
        .sheet(item: $model.editingTask) { ScheduleSheet(task: $0) }
        .alert(
            "Delete “\(model.deletingTask?.name ?? "")”?",
            isPresented: Binding(get: { model.deletingTask != nil }, set: { if !$0 { model.deletingTask = nil } }),
            presenting: model.deletingTask
        ) { t in
            Button("Delete", role: .destructive) { model.deleteTask(t) }
            Button("Cancel", role: .cancel) {}
        } message: { _ in
            Text("It won't run again. Sessions it already started keep running.")
        }
        .alert(
            "Delete “\(model.deleting.map { model.tabName($0.session) } ?? "")”?",
            isPresented: Binding(get: { model.deleting != nil }, set: { if !$0, model.deleting != nil { model.deleting = nil } }),
            presenting: model.deleting
        ) { plan in
            Button("Delete", role: .destructive) { model.delete(plan) }
            Button("Cancel", role: .cancel) {}
        } message: { plan in
            Text(plan.message)
        }
        .modifier(ArchiveWhileWorking(model: model))
        .sheet(isPresented: $model.showCreatePR) {
            if let s = model.selectedSession { CreatePRSheet(session: s) }
        }
        .alert(
            "Keep \(model.confirmKeep?.launcher ?? "")'s changes?",
            isPresented: Binding(get: { model.confirmKeep != nil }, set: { if !$0 { model.confirmKeep = nil } }),
            presenting: model.confirmKeep
        ) { m in
            Button("Keep") { model.keep(m) }
                .keyboardShortcut(.defaultAction)
            Button("Cancel", role: .cancel) {}
        } message: { _ in
            Text("They're applied to your checkout, uncommitted, for you to review. The other agents stop and every worktree of this fan-out is removed.")
        }
        .alert(
            model.closingWorktree?.apply == true ? "Apply \(model.closingWorktree?.label ?? "")'s changes?" : "Discard \(model.closingWorktree?.label ?? "")?",
            isPresented: Binding(get: { model.closingWorktree != nil }, set: { if !$0 { model.closingWorktree = nil } }),
            presenting: model.closingWorktree
        ) { w in
            if w.apply {
                Button("Apply and Close") { model.closeWorktree(w) }
                    .keyboardShortcut(.defaultAction)
            } else {
                Button("Discard", role: .destructive) { model.closeWorktree(w) }
            }
            Button("Cancel", role: .cancel) {}
        } message: { w in
            Text(w.apply
                ? "They're applied to the checkout it came from, uncommitted, for you to review. Its sessions stop, and the worktree and its branch are removed."
                : "Its sessions stop, and the worktree, its branch and every change in it are removed.")
        }
        .modifier(CleanUpAlert(model: model))
        .alert(
            "Something went wrong",
            isPresented: Binding(get: { model.error != nil && !model.sessions.isEmpty }, set: { if !$0 { model.error = nil } })
        ) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(model.error ?? "")
        }
        .alert(
            "Continue “\(model.confirmMove?.title ?? "")” in dino?",
            isPresented: Binding(get: { model.confirmMove != nil }, set: { if !$0 { model.confirmMove = nil } }),
            presenting: model.confirmMove
        ) { f in
            Button("Continue in dino") { model.adopt(f) }
                .keyboardShortcut(.defaultAction)
            Button("Cancel", role: .cancel) {}
        } message: { f in
            Text(f.isBusy
                ? "It's working right now. dino waits for the current turn to finish, closes it in \(f.terminal ?? "the other terminal") and continues the conversation here."
                : "dino closes it in \(f.terminal ?? "the other terminal") and continues the same conversation here, with its history.")
        }
        .background(WelcomeCard())
    }
}

// MARK: - Terminals

struct Terminals: View {
    @EnvironmentObject var model: DinoModel
    static let space = "terminals"
    /// Why the selected session can't use what needs its checkout on this Mac, if it can't.
    private var remoteReason: String? { model.selectedSession?.remoteReason }

    var body: some View {
        ZStack {
            Color(nsColor: .textBackgroundColor).ignoresSafeArea()
            if let ref = model.shownSubagent, !model.daemonDown {
                SubagentPane(ref: ref).id(ref)
            } else if let run = model.archivedRun {
                ArchivedRunPane(session: run).id(run.id)
            } else if model.sessions.isEmpty || model.daemonDown || model.selected == nil
                        || DinoModel.folderPath(model.selected) != nil || model.selected?.hasPrefix("run:") == true {
                EmptyState()
            }
            GeometryReader { geo in
                let split = model.shownSplit
                let layout = PaneLayout(split: split, selected: model.selected, size: geo.size)
                ZStack(alignment: .topLeading) {
                    // Every session stays mounted; only the selected one (and its split partner) draws.
                    ForEach(model.sessions) { s in
                        let rect = layout.surface(s.id)
                        let state = model.terminal(for: s.id)
                        TerminalPane(state: state, id: s.id, visible: rect != nil, focused: s.id == model.selected)
                            // A new state (after reconnecting) must mean a new surface.
                            .id(ObjectIdentifier(state))
                            .overlay {
                                // The other half of a split sits back a little, like Ghostty's.
                                if split != nil, s.id != model.selected {
                                    Color.black.opacity(0.18).allowsHitTesting(false)
                                }
                            }
                            // Last: an offset moves only the drawing, so anything added after it would sit unmoved.
                            .placed(rect ?? CGRect(origin: .zero, size: geo.size))
                            .opacity(rect != nil ? 1 : 0)
                            .allowsHitTesting(rect != nil)
                    }
                    if let split {
                        ForEach([split.first, split.second], id: \.self) { id in
                            if let s = model.sessions.first(where: { $0.id == id }), let f = layout.frame(id) {
                                PaneHeader(session: s, split: split, focused: id == model.selected)
                                    .placed(CGRect(x: f.minX, y: f.minY, width: f.width, height: PaneLayout.header))
                            }
                        }
                        if let d = layout.divider {
                            SplitDivider(split: split, size: geo.size).placed(d)
                        }
                    }
                }
                .coordinateSpace(name: Self.space)
            }
            if let g = model.groups.first(where: { "group:\($0.id)" == model.selected }) {
                CompareView(group: g)
            }
        }
        .toolbar {
            ToolbarItem(placement: .principal) {
                if let s = model.sessions.first(where: { $0.id == model.selected }) {
                    HStack(spacing: 8) {
                        // The name gives way first: the chips beside it are what you click.
                        SessionName(session: s, place: .toolbar, font: .body.weight(.semibold))
                            .layoutPriority(-1)
                            .help(s.inside?.title ?? s.title ?? s.display)
                        Group {
                            if let f = s.inside { AgentBadge(agent: f.agent) }
                            if let f = s.inside, f.continuable { TakeOverButton(session: s, found: f) }
                            if let host = s.host { HostChip(host: host) }
                            if s.exited {
                                ResumeButton(session: s).padding(.leading, 4)
                            } else if s.agent_id != "shell" {
                                // A plain shell has no mode or model to pick.
                                SessionControlsBar(session: s).padding(.leading, 4)
                            }
                        }
                        .fixedSize()
                    }
                }
            }
            ToolbarItem(placement: .primaryAction) { SidePanePicker() }
            ToolbarItem(placement: .primaryAction) { PRToolbarButton() }
        }
        .overlay(alignment: .bottomTrailing) {
            if let s = model.selectedSession, let u = s.local_url, model.offered[s.id] != u, model.sidePane != .preview {
                PreviewOffer(session: s, url: u)
            }
        }
        .animation(.spring(duration: 0.3), value: model.selectedSession?.local_url)
    }
}

/// Changes, Preview and Tasks as one control: each shows its pane in place of the others.
struct SidePanePicker: View {
    @EnvironmentObject var model: DinoModel

    var body: some View {
        let session = model.selectedSession
        let remote = session?.remoteReason
        let running = session.map { $0.exited ? 0 : $0.tasks?.running ?? 0 } ?? 0
        ControlGroup {
            Button { model.showReview.toggle() } label: {
                Label("Changes", systemImage: model.showReview ? "plusminus.circle.fill" : "plusminus.circle")
            }
            .help(remote ?? "Review this session's changes; click a line to comment for the agent (⇧⌘D)")
            .disabled(session == nil || remote != nil)
            .accessibilityValue(model.showReview ? "Shown" : "Hidden")
            Button { model.togglePreview() } label: {
                Label("Preview", systemImage: model.sidePane == .preview ? "globe.americas.fill" : "globe.americas")
            }
            .help(remote ?? "Preview this session's dev server or any local page (⌥⌘P)")
            .disabled(remote != nil && model.sidePane != .preview)
            .accessibilityValue(model.sidePane == .preview ? "Shown" : "Hidden")
            Button { model.toggleTasks() } label: {
                Label(running > 0 ? "Tasks, \(running) running" : "Tasks",
                      systemImage: model.sidePane == .tasks ? "checklist.checked" : "checklist")
            }
            .help(running > 0
                ? "\(running) running in the background: subagents and commands (⌥⌘T)"
                : session?.reportsTasks == true ? "The agent's task list, subagents and background commands (⌥⌘T)" : "This session doesn't report tasks")
            .disabled(!(session?.reportsTasks ?? false) && model.sidePane != .tasks)
            .accessibilityValue(model.sidePane == .tasks ? "Shown" : "Hidden")
        }
        .controlGroupStyle(.navigation)
    }
}

struct TerminalPane: View {
    @EnvironmentObject var model: DinoModel
    @ObservedObject var state: TerminalViewState
    let id: String
    let visible: Bool
    let focused: Bool

    var body: some View {
        TerminalSurfaceView(context: state)
            .onAppear {
                state.isSurfaceVisible = visible
                // A new session, or the app opening, selects it before its pane exists: the
                // request waits until the pane is in the window.
                if focused, !model.selectingFromSidebarKeys { state.requestFocus() }
            }
            .onChange(of: visible) { _, v in state.isSurfaceVisible = v }
            .onChange(of: focused) { _, f in if f, !model.selectingFromSidebarKeys { state.requestFocus() } }
            // What its programs say beside their text: progress along the top, the bell's border,
            // the lock while Secure Keyboard Entry is on.
            .overlay(alignment: .top) { PaneProgressBar(signal: PaneSignals.of(id)) }
            .overlay { PaneBellBorder(signal: PaneSignals.of(id)) }
            .overlay(alignment: .topTrailing) { SecureInputMark(id: id, focused: state.isFocused) }
            // Clicking into the other half of a split selects that session.
            .onChange(of: state.isFocused) { _, f in
                if f {
                    PaneSignals.seen(id)
                    model.focusedTerminal = id
                } else if model.focusedTerminal == id {
                    model.focusedTerminal = nil
                }
                if f, visible, !focused, model.shownSplit?.contains(id) == true { model.select(id) }
            }
    }
}

/// The sidebar's +: the new-session picker (⌘N starts the default agent here without it).
struct NewSessionButton: View {
    @EnvironmentObject var model: DinoModel

    var body: some View {
        Button { model.startSession() } label: {
            Image(systemName: "plus").font(.body.weight(.medium))
        }
        .buttonStyle(.plain)
        .foregroundStyle(.secondary)
        .help("New session: pick a folder or repository, then an agent (⌘N starts the default agent here)")
        .accessibilityLabel("New Session")
        .disabled(model.launchers.isEmpty)
    }
}

struct EmptyState: View {
    @EnvironmentObject var model: DinoModel

    var body: some View {
        VStack(spacing: 18) {
            DinoMark(size: 34)
            Text("One place for every agent on this machine").foregroundStyle(.secondary)
            if let error = model.error {
                Text(error).foregroundStyle(SessionStatus.exited.color).font(.callout)
            }
            if model.daemonDown {
                Text("dinod isn't running. Your sessions are saved and resume when it starts.")
                    .foregroundStyle(.secondary).font(.callout)
                Button("Start dinod") { model.startDaemon() }.controlSize(.large)
            }
            if !model.daemonDown {
                Button {
                    model.loadFound()
                    model.showContinue = true
                } label: {
                    Label("Continue a session…", systemImage: "arrow.uturn.forward").frame(width: 240)
                }
                .controlSize(.large)
                .buttonStyle(.borderedProminent)
                .tint(Brand.green)
                Text("or start a new one").font(.caption).foregroundStyle(.tertiary)
            }
            if !model.daemonDown {
                Button { model.startSession() } label: {
                    Label("New Session…", systemImage: "plus").frame(width: 240)
                }
                .controlSize(.large)
                .help("Pick a folder, a recent repository, one of yours on GitHub or a URL to clone, then an agent (⌘N starts the default agent here)")
            }
        }
        .padding(40)
    }
}

struct DinoMark: View {
    let size: CGFloat

    var body: some View {
        Text("dino").foregroundStyle(Brand.green)
            .font(.system(size: size, weight: .bold, design: .monospaced))
    }
}

// MARK: - Sidebar

struct Sidebar: View {
    @EnvironmentObject var model: DinoModel
    /// Tree nodes the user closed, newline-joined (SceneStorage can't hold a Set).
    @SceneStorage("sidebar.collapsed") private var collapsedIDs = ""
    @AppStorage("sidebar.filter") private var filter = SessionFilter.all
    /// "On this Mac" shows all its agents, not just the newest few.
    @State private var allElsewhere = false
    /// "On this Mac" closed, among the collapsed nodes.
    static let elsewhereKey = "section:elsewhere"
    /// How many of its agents "On this Mac" shows before "N more…".
    static let elsewhereShown = 5

    private var collapsed: Binding<Set<String>> {
        Binding(
            get: { Set(collapsedIDs.split(separator: "\n").map(String.init)) },
            // Only a real change: a write of the same value redraws the whole sidebar.
            set: {
                let ids = $0.sorted().joined(separator: "\n")
                if ids != collapsedIDs { collapsedIDs = ids }
            }
        )
    }

    private var list: some View {
        List(selection: Binding(get: { model.selected }, set: { tag in
            // Rows outside "Agents" carry a `move:` tag: ask before handing that session over.
            // Anything else that isn't a session (or deselecting) leaves the selection alone.
            guard let tag else { return }
            // Arrowed onto: the keyboard stays in the list. Clicked: the terminal takes it.
            let keys = NSApp.currentEvent?.type == .keyDown
            if tag.hasPrefix("tmux:") || tag.hasPrefix("move:") {
                // Another terminal's agent: a click acts on it; arrowing past it only highlights
                // it (Return acts, see primaryAction below), so no question pops up on the way.
                if !keys { actOnElsewhere(tag) }
            } else if tag.hasPrefix("task:") {
                // A task's row shows what it did: a click opens its runs under it; arrowed onto, it
                // waits for Return (primaryAction below). Edit is in its menu. After the table's own
                // selection callback: rows coming and going from inside it is a reentrant update
                // NSTableView can crash on.
                let task = String(tag.dropFirst(5))
                if !keys { DispatchQueue.main.async { model.toggleRuns(task) } }
            } else if tag.hasPrefix("run:") {
                model.openRun(String(tag.dropFirst(4)), keepKeyboard: keys)
            } else {
                model.select(tag, keepKeyboard: keys)
            }
        })) {
            if filter == .archived {
                ArchivedSection()
            } else {
                let narrowed = model.sidebarNarrowed
                let tree = filter == .all && !narrowed
                    ? SessionTree.build(repos: model.repos, sessions: model.sidebarSessions, groups: model.groups)
                    : SessionTree.build(repos: model.repos, sessions: model.sidebarSessions, groups: model.groups) {
                        filter.passes(model.status(of: $0)) && model.sidebarShows($0)
                    }
                // Headings are plain rows, not List section headers: when the sidebar's height
                // changed (the usage panel, the filter bar) while sections came and went, the table
                // tied a header to a row of another section and threw, quitting the app.
                SidebarHeading(title: "Workspaces")
                Group {
                    // Shells live in the tabs: a folder with only those (or nothing) left in it
                    // would be a workspace row with nothing under it.
                    ForEach(tree.repos.filter { $0.worthShowing(here: model.folder.path) }) { node in
                        RepoRows(node: node, filter: filter, collapsed: collapsed)
                    }
                    let remote = Dictionary(grouping: tree.unfiled.filter { $0.host != nil }) { $0.host ?? "" }
                    ForEach(remote.keys.sorted(), id: \.self) { host in
                        HostRows(host: host, sessions: remote[host] ?? [], collapsed: collapsed)
                    }
                    ForEach(tree.unfiled.filter { $0.host == nil }) { s in
                        SessionRow(session: s, index: 0)
                            .tag(s.id)
                            .contextMenu { SessionMenu(session: s) }
                    }
                    if filter != .all || narrowed, tree.repos.isEmpty, tree.unfiled.isEmpty {
                        let q = model.sidebarQuery.trimmingCharacters(in: .whitespaces)
                        Text(!q.isEmpty ? "No session matches “\(q)”"
                            : narrowed ? "No sessions here"
                            : filter == .needsYou ? "Nothing needs you"
                            : filter == .done ? "Nothing new has finished" : "No \(filter.label.lowercased()) sessions")
                            .font(.callout).foregroundStyle(.tertiary)
                    }
                }
                // Other terminals' sessions aren't dino's to sort by status.
                if !model.elsewhere.isEmpty, filter == .all, !narrowed {
                    let elsewhere = model.elsewhere
                    let open = !collapsed.wrappedValue.contains(Self.elsewhereKey)
                    ElsewhereHeading(count: elsewhere.count, open: Binding(
                        get: { open },
                        set: { o in
                            var set = collapsed.wrappedValue
                            if o { set.remove(Self.elsewhereKey) } else { set.insert(Self.elsewhereKey) }
                            collapsed.wrappedValue = set
                        }
                    ))
                    if open {
                        // The newest few (the list comes newest first), the rest on asking.
                        ForEach(allElsewhere ? elsewhere : Array(elsewhere.prefix(Self.elsewhereShown))) { f in
                            // In a tmux pane: a click shows it there (tmux keeps it); elsewhere it moves to dino.
                            ElsewhereRow(session: f).tag(f.tmux != nil ? "tmux:\(f.id)" : "move:\(f.id)")
                        }
                        if elsewhere.count > Self.elsewhereShown {
                            Button { allElsewhere.toggle() } label: {
                                Text(allElsewhere ? "Show fewer" : "\(elsewhere.count - Self.elsewhereShown) more…")
                            }
                            .buttonStyle(.plain)
                            .font(.callout)
                            .foregroundStyle(.secondary)
                            .selectionDisabled()
                        }
                    }
                }
                if filter == .all, !narrowed {
                    SidebarHeading(title: "Automations") {
                        if !model.scheduled.isEmpty {
                            Button { model.newTask() } label: { Image(systemName: "plus") }
                                .buttonStyle(.plain)
                                .help("New Automation")
                                .accessibilityLabel("New Automation")
                        }
                    }
                    ForEach(model.scheduled) { t in
                        ScheduledRow(task: t).tag("task:\(t.id)")
                        if model.openTasks.contains(t.id) {
                            TaskRuns(task: t)
                        }
                    }
                    if model.scheduled.isEmpty {
                        Button { model.newTask() } label: {
                            Label("Automate something…", systemImage: "bolt")
                        }
                        .buttonStyle(.plain)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                    }
                }
            }
        }
        // Return on a session opens it and gives its terminal the keyboard, as a click does;
        // double-clicking it renames it (so does its menu). The list's own primary action, not a
        // gesture on the name: that swallowed the single click meant to select the row. The rows
        // keep their own menus; this adds none.
        .contextMenu(forSelectionType: String.self, menu: { _ in EmptyView() }, primaryAction: { tags in
            let returnKey = NSApp.currentEvent?.type == .keyDown
            if tags.count == 1, let tag = tags.first, tag.hasPrefix("tmux:") || tag.hasPrefix("move:") {
                actOnElsewhere(tag)
            } else if tags.count == 1, let tag = tags.first, tag.hasPrefix("task:") {
                // Return; a click already opened it (see the selection above).
                let task = String(tag.dropFirst(5))
                if returnKey { DispatchQueue.main.async { model.toggleRuns(task) } }
            } else if tags.count == 1, let tag = tags.first, tag.hasPrefix("run:") {
                if returnKey { model.openRun(String(tag.dropFirst(4))) }
            } else if tags.count == 1, let id = tags.first, model.sessions.contains(where: { $0.id == id }) {
                if returnKey { model.select(id) } else { model.renaming = Renaming(id: id, place: .sidebar) }
            }
        })
        // Not rebuilt when rows come or go (that replaced every row, and froze the window for
        // up to seconds while agents made worktrees): rows keep unique tags and stable
        // identities instead, so the list's own diff stays right.
        .listStyle(.sidebar)
        // → opens the selected row's group and ← closes it, as in an outline.
        .onKeyPress(.rightArrow) { openSelected(true) }
        .onKeyPress(.leftArrow) { openSelected(false) }
    }

    private func openSelected(_ open: Bool) -> KeyPress.Result {
        guard filter != .archived, let tag = model.selected else { return .ignored }
        let tree = SessionTree.build(repos: model.repos, sessions: model.sidebarSessions, groups: model.groups)
        guard let (key, startsOpen) = tree.repos.lazy.compactMap({ $0.opening(tag) }).first else { return .ignored }
        var set = collapsed.wrappedValue
        let isOpen = startsOpen != set.contains(key)
        guard isOpen != open else { return .ignored }
        if open == startsOpen { set.remove(key) } else { set.insert(key) }
        collapsed.wrappedValue = set
        return .handled
    }

    /// An agent running in another terminal: shown where tmux keeps it, or offered to move to dino.
    private func actOnElsewhere(_ tag: String) {
        if tag.hasPrefix("tmux:") {
            if let f = model.elsewhere.first(where: { "tmux:\($0.id)" == tag }) { model.showInTmux(f) }
        } else {
            model.confirmMove = model.elsewhere.first { "move:\($0.id)" == tag }
        }
    }

    var body: some View {
        VStack(spacing: 0) {
            // Built once dinod's first answers are in, whole: grown instead by two batches of rows
            // at once (the sessions, and the scheduled tasks below them) while it was still laying
            // out its first rows, the table re-entered its own row-height cache, which AppKit warns
            // will become an assertion.
            if model.sidebarReady || model.daemonDown {
                list
            } else {
                Spacer()
            }
            UsagePanel()
        }
        .safeAreaInset(edge: .top) {
            VStack(alignment: .leading, spacing: 8) {
                if filter == .archived {
                    // A view of its own: a title and the way back, no status filters.
                    HStack(spacing: 6) {
                        Button { filter = .all } label: {
                            Label("Sessions", systemImage: "chevron.backward").labelStyle(.titleAndIcon)
                        }
                        .buttonStyle(.plain)
                        .foregroundStyle(.secondary)
                        .font(.callout)
                        .help("Back to every session")
                        Spacer()
                    }
                    HStack(alignment: .firstTextBaseline) {
                        Text("Archived").font(.title3.weight(.semibold))
                        Text("\(model.archived.count)").font(.callout.monospacedDigit()).foregroundStyle(.secondary)
                    }
                    .accessibilityElement(children: .combine)
                    .accessibilityAddTraits(.isHeader)
                } else {
                    HStack {
                        DinoMark(size: 15)
                        Spacer()
                        if !model.sessions.isEmpty {
                            Button { model.findingSessions = true } label: { Image(systemName: "magnifyingglass") }
                                .buttonStyle(.plain)
                                .foregroundStyle(.secondary)
                                .help("Find sessions (⇧⌘F)")
                                .accessibilityLabel("Find sessions")
                            ScopeMenu()
                        }
                        ArchiveToggle(filter: $filter)
                        // Starting work lives with the sessions it makes: here, not in the toolbar.
                        NewSessionButton()
                    }
                }
                if filter != .archived, model.findingSessions || !model.sidebarQuery.isEmpty {
                    SessionSearchField()
                }
                if filter != .archived, model.sidebarScope != nil {
                    ScopeChip()
                }
                if filter != .archived, !model.sessions.isEmpty || filter != .all {
                    FilterBar(filter: $filter)
                }
            }
            .padding(.horizontal, 14)
            .padding(.top, 6)
        }
        // The archive has its own search.
        .onChange(of: model.findingSessions) { _, on in if on, filter == .archived { filter = .all } }
    }
}

/// A title over a part of the sidebar, as a section header looks, but an ordinary row that can't
/// be selected (see Sidebar for why it isn't a header).
struct SidebarHeading<Trailing: View>: View {
    let title: String
    @ViewBuilder var trailing: Trailing

    init(title: String, @ViewBuilder trailing: () -> Trailing = { EmptyView() }) {
        self.title = title
        self.trailing = trailing()
    }

    var body: some View {
        HStack {
            Text(title)
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(.secondary)
            Spacer()
            trailing.foregroundStyle(.secondary)
        }
        .padding(.top, 8)
        .selectionDisabled()
        .accessibilityAddTraits(.isHeader)
    }
}

struct SessionRow: View {
    @EnvironmentObject var model: DinoModel
    let session: SessionInfo
    let index: Int
    var stat: DiffStat?
    /// The worktree's branch, on the row instead of a header above a single session.
    var branch: String?
    /// The repo or worktree it's filed under: a shell's folder shows only when it's somewhere else.
    var root: String?
    @State private var hovering = false
    @State private var anchor = CostAnchorView()

    var body: some View {
        let status = model.status(of: session)
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 6) {
                StatusDot(status: status)
                    .overlay { RowProgress(signal: PaneSignals.of(session.id)) }
                SessionName(session: session, place: .sidebar, font: .body.weight(.semibold))
                    .layoutPriority(1)
                if let branch {
                    BranchChip(branch: branch)
                        .help("Branch \(branch)\(root.map { "\n\($0)" } ?? "")")
                }
                if let task = session.scheduled {
                    Image(systemName: "bolt")
                        .font(.caption).foregroundStyle(.tertiary)
                        .help("Started by the automation “\(task)”")
                        .accessibilityLabel("Started by the automation \(task)")
                }
                if let reach = session.reach { UsingMark(session: session, reach: reach) }
                if let why = session.instead_of {
                    let asked = model.launchers.first { $0.agent_id == why.agent_id }?.label ?? why.agent_id
                    Image(systemName: "arrow.uturn.right")
                        .font(.caption).foregroundStyle(.tertiary)
                        .help("Started instead of \(asked): \(why.name) was at its limit\(why.resets_at.map { " until \(Clock.short($0))" } ?? "")")
                        .accessibilityLabel("Started instead of \(asked)")
                }
                if let pr = model.pr(of: session) { PRChip(pr: pr, auto: session.auto) }
                if let split = model.splits.first(where: { $0.contains(session.id) }) {
                    let other = model.sessions.first { $0.id == split.other(session.id) }?.display ?? "another session"
                    Image(systemName: split.vertical ? "rectangle.split.1x2" : "rectangle.split.2x1")
                        .font(.caption).foregroundStyle(.tertiary)
                        .help("In a split with \(other)")
                        .accessibilityLabel("In a split with \(other)")
                }
                Spacer(minLength: 4)
                if session.pinned == true {
                    Image(systemName: "pin.fill")
                        .font(.caption2).foregroundStyle(.tertiary)
                        .rotationEffect(.degrees(45))
                        .help("Pinned: at the top of its group, and dino won't archive it on its own")
                        .accessibilityLabel("Pinned")
                }
                if hovering, model.canArchive(session.id) {
                    // Where Claude desktop has it: on the row, under the pointer.
                    Button { model.archive(session.id) } label: { Image(systemName: "archivebox") }
                        .buttonStyle(.plain)
                        .foregroundStyle(.secondary)
                        .help("Archive: stop it and keep it under Archived, to pick up again (⇧⌘A)")
                        .accessibilityLabel("Archive")
                } else {
                    Text(status.label).font(.caption).foregroundStyle(status.color).lineLimit(1).fixedSize()
                        .help(status.detail ?? status.label)
                }
            }
            if let f = session.inside {
                // The row's name is already the agent's title: the badge says what it is and where.
                HStack(spacing: 5) {
                    AgentBadge(agent: f.agent)
                    if model.isTakingOver(session) {
                        Image(systemName: "hourglass").font(.caption).foregroundStyle(.secondary).accessibilityHidden(true)
                        Text("continues in dino after this turn").font(.caption).foregroundStyle(.secondary).lineLimit(1)
                        Button("Cancel") { model.cancelTakeOver(session.id) }
                            .buttonStyle(.link).font(.caption)
                            .help("Stop waiting: \(f.agentName) goes on in this shell")
                    } else {
                        Text("in a shell").font(.caption).foregroundStyle(.secondary).lineLimit(1)
                    }
                }
                .help("\(f.agentName) started by hand in this shell")
            }
            if detail != nil || modelText != nil {
                HStack(spacing: 6) {
                    if let detail { detail.lineLimit(1).truncationMode(.tail) }
                    Spacer(minLength: 6)
                    if let modelText {
                        Text(modelText).font(.caption).foregroundStyle(.tertiary).lineLimit(1).fixedSize()
                        if let ctx = session.contextUse {
                            ContextRing(used: ctx.used, limit: ctx.limit, size: 10)
                        }
                    }
                }
                .help(usageHelp)
            }
            if let f = session.fallback {
                FallbackLine(fallback: f).help(FallbackChip.detail(f, session.usage_by_route ?? []))
            }
            if let error = session.error {
                ErrorLine(message: error)
            }
            PeerChips(session: session)
            if let stat, stat.files > 0 {
                StatText(stat: stat).font(.caption.monospacedDigit())
            }
        }
        .padding(.vertical, 3)
        .background(CostAnchor(holder: anchor))
        .onHover {
            hovering = $0
            // What it costs the Mac, in a card beside the row; only for what runs on this Mac.
            if !$0 || (session.host == nil && !session.exited) { CostCard.shared.hover(session.id, anchor: anchor.view, on: $0) }
        }
        .onDisappear { CostCard.shared.hover(session.id, anchor: nil, on: false) }
    }

    /// The one thing worth a second line: what it's asking, what it waits on, a server it left
    /// running, or where a shell has gone.
    private var detail: Text? {
        let status = model.status(of: session)
        if let needs = session.needs {
            return Text(needs).font(.caption).foregroundStyle(SessionStatus.needsYou.color)
        }
        if status == .waiting, let on = session.waitingOn {
            return Text("Waiting on \(on)").font(.caption).foregroundStyle(.secondary)
        }
        if status == .idle || status == .done, let ports = session.serving {
            return Text("Serving :\(ports.replacingOccurrences(of: ", ", with: " :"))").font(.caption).foregroundStyle(.secondary)
        }
        if let here = shellPlace {
            let path = Text(here).font(.caption.monospaced()).foregroundStyle(.secondary)
            guard let code = session.last_exit, code != 0 else { return path }
            return path + Text("  exit \(code)").font(.caption.monospaced()).foregroundStyle(SessionStatus.exited.color)
        }
        if let code = session.last_exit, code != 0, session.inside == nil {
            return Text("exit \(code)").font(.caption.monospaced()).foregroundStyle(SessionStatus.exited.color)
        }
        if status == .thinking { return Text("Thinking").font(.caption).foregroundStyle(.secondary) }
        return nil
    }

    /// A shell's folder when it isn't the place it's filed under: relative inside it, else in full.
    private var shellPlace: String? {
        guard session.inside == nil, let here = session.shell_cwd else { return nil }
        let base = root ?? session.cwd
        if let base, here == base { return nil }
        if let base, SessionTree.contains(base, here) { return String(here.dropFirst(base.count + 1)) }
        return NSString(string: here).abbreviatingWithTildeInPath
    }

    private var modelText: String? {
        guard session.requests > 0, session.last_model != nil else { return nil }
        let m = shortModel(session.last_model)
        return session.tier.map { "\($0) → \(m)" } ?? m
    }

    private var usageHelp: String {
        var lines: [String] = []
        if let m = session.last_model { lines.append(m) }
        if session.requests > 0 {
            lines.append("↑\(tokens(session.input_tokens)) in · ↓\(tokens(session.output_tokens)) out, \(session.requests) request\(session.requests == 1 ? "" : "s")")
        }
        if let ctx = session.contextUse {
            lines.append("Context \(tokens(ctx.used)) of \(tokens(ctx.limit))")
        }
        // Answered by more than one route: what each one did.
        if let routes = session.usage_by_route, routes.count > 1 {
            lines += routes.map { "\($0.name): ↑\(tokens($0.input_tokens)) in · ↓\(tokens($0.output_tokens)) out" }
        }
        return lines.joined(separator: "\n")
    }
}

/// For a session whose program ended: start it again in place (Enter in its pane does the same).
struct ResumeButton: View {
    @EnvironmentObject var model: DinoModel
    let session: SessionInfo

    var body: some View {
        let shell = session.agent_id == "shell"
        Button { model.resume(session.id) } label: {
            Label(shell ? "Restart" : "Resume", systemImage: "play.fill")
        }
        .labelStyle(.titleAndIcon)
        .controlSize(.small)
        .help(shell ? "Start a new shell in the same folder (Enter in the pane)" : "Resume the conversation where it left off (Enter in the pane)")
    }
}

struct StatusDot: View {
    let status: SessionStatus

    var body: some View {
        ZStack {
            switch status {
            case .needsYou:
                Image(systemName: "exclamationmark.circle.fill").foregroundStyle(status.color)
            case .done:
                Image(systemName: "checkmark.circle.fill").foregroundStyle(status.color)
            case .exited:
                Image(systemName: "xmark.circle").foregroundStyle(status.color)
            case .ended:
                Image(systemName: "stop.circle").foregroundStyle(status.color)
            case .idle:
                Circle().strokeBorder(.secondary, lineWidth: 1.5).frame(width: 10, height: 10)
            case .thinking, .working:
                Pulse(color: NSColor(status.color), ring: false, period: 0.7).frame(width: 10, height: 10)
            case .waiting:
                // A ring, slower: busy, but not the agent itself.
                Pulse(color: NSColor(status.color), ring: true, period: 1.4).frame(width: 10, height: 10)
            }
        }
        .frame(width: 14, height: 14)
        .help(status.detail ?? status.label)
        .accessibilityElement()
        .accessibilityLabel(status.label)
        .accessibilityValue(status.detail ?? "")
    }
}

/// A dot or ring that fades in and out. Core Animation runs it in the render server: a SwiftUI
/// repeating animation re-renders the whole window on the main thread every frame while any
/// session is busy.
private struct Pulse: NSViewRepresentable {
    let color: NSColor
    let ring: Bool
    let period: Double

    func makeNSView(context: Context) -> PulseView { PulseView() }

    func updateNSView(_ view: PulseView, context: Context) {
        view.set(color: color, ring: ring, period: period)
    }
}

final class PulseView: NSView {
    private let shape = CAShapeLayer()
    private var style: (NSColor, Bool, Double)?

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        layer?.addSublayer(shape)
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    /// Only a picture: a click on it belongs to the row it's in.
    override func hitTest(_: NSPoint) -> NSView? { nil }

    func set(color: NSColor, ring: Bool, period: Double) {
        if let s = style, s.0 == color, s.1 == ring, s.2 == period { return }
        style = (color, ring, period)
        needsLayout = true
        shape.removeAnimation(forKey: "pulse")
        let fade = CABasicAnimation(keyPath: "opacity")
        fade.fromValue = 1
        fade.toValue = 0.35
        fade.duration = period
        fade.autoreverses = true
        fade.repeatCount = .infinity
        fade.timingFunction = CAMediaTimingFunction(name: .easeInEaseOut)
        // Survives the view leaving and rejoining a window, as sidebar rows do.
        fade.isRemovedOnCompletion = false
        shape.add(fade, forKey: "pulse")
    }

    override func layout() {
        super.layout()
        guard let (color, ring, _) = style else { return }
        let resolved = color.usingColorSpace(.deviceRGB)?.cgColor ?? color.cgColor
        let inset: CGFloat = ring ? 1 : 0
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        defer { CATransaction.commit() }
        shape.frame = bounds
        shape.path = CGPath(ellipseIn: bounds.insetBy(dx: inset, dy: inset), transform: nil)
        shape.fillColor = ring ? nil : resolved
        shape.strokeColor = ring ? resolved : nil
        shape.lineWidth = ring ? 2 : 0
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        needsLayout = true
    }
}

/// Usage at the foot of the sidebar: one line with the fullest window, opening to all of them.
struct UsagePanel: View {
    @EnvironmentObject var model: DinoModel
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.openWindow) private var openWindow
    @AppStorage("usage.open") private var open = false
    private static let names = ["anthropic": "Claude", "chatgpt": "Codex"]

    private var windows: [(label: String, window: WindowInfo)] {
        model.quotas.flatMap { q in
            q.windows.filter { $0.name.hasSuffix("h") || $0.name.hasSuffix("d") }
                .map { ("\(Self.names[q.provider] ?? q.provider) \($0.name)", $0) }
        }
    }

    var body: some View {
        let windows = windows
        let fullest = windows.max { $0.window.utilization < $1.window.utilization }
        VStack(alignment: .leading, spacing: 8) {
            AwakeStatus()
            Button { withAnimation(.easeOut(duration: 0.15)) { open.toggle() } } label: {
                HStack(spacing: 6) {
                    Image(systemName: "chevron.right")
                        .font(.caption2.weight(.semibold))
                        .rotationEffect(.degrees(open ? 90 : 0))
                        .foregroundStyle(.tertiary)
                    Text("Usage").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                    if !open, let fullest {
                        // The window closest to its limit is the one that stops work.
                        let pct = Double(fullest.window.utilization)
                        ProgressView(value: min(max(pct, 0), 1)).tint(QuotaBar.color(pct)).controlSize(.mini)
                        Text("\(Int((pct * 100).rounded()))%").font(.caption.monospacedDigit()).foregroundStyle(QuotaBar.color(pct))
                    } else {
                        Spacer(minLength: 0)
                    }
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help(fullest.map { "\($0.label): \(Int((Double($0.window.utilization) * 100).rounded()))% used. Click for every window." } ?? "Your plans' usage windows")
            .accessibilityLabel("Usage")
            .accessibilityValue(fullest.map { "\($0.label) \(Int((Double($0.window.utilization) * 100).rounded())) percent" } ?? "No data yet")
            .accessibilityHint(open ? "Collapses usage" : "Shows every usage window")
            .contextMenu {
                Button("Usage Stats…") { openWindow(id: StatsView.windowID) }
            }
            if open {
                ForEach(windows, id: \.label) { w in
                    QuotaBar(label: w.label, window: w.window)
                }
                if windows.isEmpty {
                    Text("No quota data yet").font(.caption).foregroundStyle(.tertiary)
                }
                let used = model.sessions.reduce(UInt64(0)) { $0 + $1.input_tokens + $1.output_tokens }
                if used > 0 {
                    HStack {
                        Text("Open sessions").foregroundStyle(.secondary)
                        Spacer()
                        Text("\(tokens(used)) tok")
                    }
                    .font(.caption.monospacedDigit())
                    .help("Tokens in and out across the sessions in the sidebar; each row's tooltip has its own")
                }
                let free = model.sessions.filter { $0.tier != nil }.reduce(UInt64(0)) { $0 + $1.input_tokens + $1.output_tokens }
                if free > 0 {
                    HStack {
                        Text("Free models").foregroundStyle(.secondary)
                        Spacer()
                        Text("\(tokens(free)) tok")
                        Text("$0.00").foregroundStyle(Brand.green)
                    }
                    .font(.caption.monospacedDigit())
                }
                Button { openWindow(id: StatsView.windowID) } label: {
                    Label("Usage Stats", systemImage: "chart.bar.xaxis")
                        .font(.caption)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .foregroundStyle(.secondary)
                .help("Tokens, models, agents, projects, routes and speed over time (⇧⌘U)")
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .frame(maxWidth: .infinity, alignment: .leading)
        // Light mode: the sidebar shows through, under a hairline; the bar material there is a white
        // band across the grey sidebar. Dark mode keeps its bar.
        .background(colorScheme == .dark ? AnyShapeStyle(.bar) : AnyShapeStyle(.clear))
        .overlay(alignment: .top) { if colorScheme == .light { Divider() } }
    }
}

struct QuotaBar: View {
    let label: String
    let window: WindowInfo

    var body: some View {
        let pct = Double(window.utilization)
        let color = Self.color(pct)
        VStack(alignment: .leading, spacing: 3) {
            HStack {
                Text(label)
                Spacer()
                Text("\(Int((pct * 100).rounded()))%").foregroundStyle(color)
                if let reset = window.resets_at {
                    Text("· \(resetText(reset))").foregroundStyle(.tertiary)
                }
            }
            .font(.caption.monospacedDigit())
            ProgressView(value: min(max(pct, 0), 1)).tint(color).controlSize(.small)
        }
    }

    static func color(_ pct: Double) -> Color {
        pct < 0.6 ? Color(nsColor: .systemGreen) : pct < 0.85 ? SessionStatus.needsYou.color : SessionStatus.exited.color
    }
}

// MARK: - Formatting

func tokens(_ n: UInt64) -> String {
    switch n {
    case ..<1000: "\(n)"
    case ..<1_000_000: String(format: "%.1fk", Double(n) / 1e3)
    default: String(format: "%.1fM", Double(n) / 1e6)
    }
}

func shortModel(_ m: String?) -> String {
    guard var m else { return "" }
    if m.hasPrefix("claude-") { m.removeFirst(7) }
    return m.split(separator: "-").filter { !($0.count == 8 && $0.allSatisfy(\.isNumber)) }.joined(separator: "-")
}

func resetText(_ at: UInt64) -> String {
    let secs = max(0, Int(at) - Int(Date().timeIntervalSince1970))
    switch secs {
    case ..<3600: return "\(secs / 60)m"
    case ..<86400: return "\(secs / 3600)h\(String(format: "%02d", secs % 3600 / 60))m"
    default: return "\(secs / 86400)d\(secs % 86400 / 3600)h"
    }
}

// MARK: - Continue anything

struct AgentBadge: View {
    let agent: String

    var body: some View {
        Text(AgentNames.short[base] != nil ? base : "claude")
            .font(.system(size: 9, weight: .semibold, design: .monospaced))
            .padding(.horizontal, 4).padding(.vertical, 1)
            .background(RoundedRectangle(cornerRadius: 3).fill(color.opacity(0.18)))
            .foregroundStyle(color)
    }

    /// Free-tier ones ("kimi-free") as their agent.
    private var base: String { agent.hasSuffix("-free") ? String(agent.dropLast(5)) : agent }

    private var color: Color { Self.color(agent) }

    /// Each agent's colour, wherever it's marked (this chip, an agent's tab).
    static func color(_ agent: String) -> Color {
        let base = agent.hasSuffix("-free") ? String(agent.dropLast(5)) : agent
        return switch base {
        case "codex": .blue
        case "qwen": .purple
        case "kimi": .teal
        case "pi": .mint
        case "hermes": .indigo
        case "codewhale": .cyan
        case "opencode": .gray
        case "copilot": .green
        case "cursor": .brown
        case "amp": .pink
        default: Brand.spike
        }
    }
}

/// Continue an agent started by hand in a shell as a dino session: same row, conversation resumed.
struct TakeOverButton: View {
    @EnvironmentObject var model: DinoModel
    let session: SessionInfo
    let found: FoundSession

    var body: some View {
        if model.isTakingOver(session) {
            Button("Cancel Continue") { model.cancelTakeOver(session.id) }
                .controlSize(.small)
                .help("Stop waiting to continue it in dino (Esc): \(found.agentName) goes on in this shell")
        } else {
            Button("Continue in dino") { model.takeOver(session) }
                .controlSize(.small)
                .help("Continue this \(found.agentName) conversation as a dino session, once its turn is over: status, tasks, controls and previews then work. The shell goes.")
        }
    }
}

/// Over the terminals: a shell in view whose agent waits for its turn to end to continue in
/// dino, with Cancel (Esc in its terminal). Only that session waits; the rest of dino goes on.
/// No animation: a turn can take minutes, and a terminal at rest should cost nothing.
struct TakeOverBanner: View {
    @EnvironmentObject var model: DinoModel

    var body: some View {
        let shown = model.shownSplit.map { [$0.first, $0.second] } ?? model.selected.map { [$0] } ?? []
        let ids = shown.filter { $0 == model.selected } + shown.filter { $0 != model.selected }
        if let s = ids.lazy.compactMap({ id in model.sessions.first { $0.id == id && model.isTakingOver($0) } }).first,
           let f = s.inside {
            let title = f.title.isEmpty ? f.agentName : "“\(f.title)”"
            HStack(spacing: 10) {
                Image(systemName: "hourglass")
                    .font(.title3)
                    .foregroundStyle(Brand.green)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 1) {
                    Text(ids.count > 1 && s.id != model.selected ? "Continuing \(title) in dino (\(s.display))" : "Continuing \(title) in dino")
                        .font(.callout.weight(.semibold))
                        .lineLimit(1)
                    Text(f.isBusy
                        ? "Waiting for \(f.agentName)'s turn to end, then the conversation continues here as a dino session."
                        : "\(f.agentName) stops in this shell and the conversation continues here as a dino session.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Spacer(minLength: 8)
                Button("Cancel") { model.cancelTakeOver(s.id) }
                    .controlSize(.regular)
                    .help("Stop waiting (Esc): \(f.agentName) goes on in this shell")
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 7)
            .background(Brand.green.opacity(0.10))
            .overlay(alignment: .bottom) { Divider() }
            .accessibilityElement(children: .contain)
            .accessibilityLabel("Continuing \(title) in dino")
        }
    }
}

/// A session running in another terminal, with a one-click handoff.
/// "On this Mac               12 ⌄": the section's heading, with how many agents it has. A click
/// anywhere on it closes or opens the section.
struct ElsewhereHeading: View {
    let count: Int
    @Binding var open: Bool

    var body: some View {
        Button { open.toggle() } label: {
            HStack(spacing: 4) {
                Text("On this Mac").font(.subheadline.weight(.semibold))
                Spacer()
                Text("\(count)").font(.caption).monospacedDigit()
                Image(systemName: "chevron.right")
                    .font(.caption2.weight(.semibold))
                    .rotationEffect(.degrees(open ? 90 : 0))
                    .frame(width: OpeningRows<EmptyView, EmptyView>.chevron, height: 16)
            }
            .foregroundStyle(.secondary)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .padding(.top, 8)
        .selectionDisabled()
        .help(open ? "Collapse" : "Expand")
        .accessibilityLabel("On this Mac, \(count == 1 ? "1 agent" : "\(count) agents")")
        .accessibilityHint(open ? "Collapse" : "Expand")
        .accessibilityAddTraits(.isHeader)
    }
}

struct ElsewhereRow: View {
    @EnvironmentObject var model: DinoModel
    let session: FoundSession

    var body: some View {
        HStack(spacing: 8) {
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 5) {
                    AgentBadge(agent: session.agent)
                    // One that's still starting has no conversation, so no title, yet.
                    Text(session.title.isEmpty ? session.agentName : session.title).lineLimit(1)
                }
                Text(whereText(session)).font(.caption).foregroundStyle(.secondary).lineLimit(1)
            }
            Spacer()
            if model.adopting.contains(session.id) {
                // Waiting for its turn to end: only this row waits.
                Image(systemName: "hourglass").foregroundStyle(.secondary)
                    .help("Continues in dino once its turn ends")
                    .accessibilityLabel("Continues in dino once its turn ends")
                Button("Cancel") { model.cancelAdopt(session) }
                    .buttonStyle(.link).font(.caption)
                    .help("Stop waiting: it goes on in \(session.terminal ?? "the other terminal")")
            } else if session.asking {
                Image(systemName: "exclamationmark.circle.fill").foregroundStyle(SessionStatus.needsYou.color)
                    .help("Asking for something in tmux: click to go there")
            } else if session.tmux != nil {
                Image(systemName: "rectangle.split.2x1").foregroundStyle(.secondary)
                    .help("Running in tmux, which keeps it: click to show it there")
            } else {
                Image(systemName: "arrow.right.circle").foregroundStyle(Brand.green)
                    .help("Click to continue this session in dino")
            }
        }
        .padding(.vertical, 2)
        .contentShape(Rectangle())
        .contextMenu {
            if session.tmux != nil {
                Button("Show in tmux") { model.showInTmux(session) }
            }
            Button("Continue in dino…") { model.confirmMove = session }
        }
    }
}

func whereText(_ f: FoundSession) -> String {
    var parts: [String] = []
    if let t = f.terminal { parts.append("in \(t)") }
    if let s = f.status { parts.append(s == "needs" ? "needs you" : s) }
    if let cwd = f.cwd { parts.append(shortPath(cwd)) }
    return parts.joined(separator: " · ")
}

func shortPath(_ p: String) -> String {
    let home = FileManager.default.homeDirectoryForCurrentUser.path
    return p.hasPrefix(home) ? "~" + p.dropFirst(home.count) : p
}

func ago(_ secs: UInt64) -> String {
    guard secs > 0 else { return "" }
    let d = max(0, Int(Date().timeIntervalSince1970) - Int(secs))
    switch d {
    case ..<60: return "just now"
    case ..<3600: return "\(d / 60)m ago"
    case ..<86400: return "\(d / 3600)h ago"
    default: return "\(d / 86400)d ago"
    }
}

/// Archiving a session in the middle of a turn stops its agent there, so it asks first.
private struct ArchiveWhileWorking: ViewModifier {
    @ObservedObject var model: DinoModel

    func body(content: Content) -> some View {
        content.alert(
            "Archive “\(model.archiving.map { model.tabName($0) } ?? "")” while it's working?",
            isPresented: Binding(get: { model.archiving != nil }, set: { if !$0, model.archiving != nil { model.archiving = nil } }),
            presenting: model.archiving
        ) { session in
            Button("Archive") { model.archiveNow(session.id) }
            Button("Cancel", role: .cancel) {}
        } message: { _ in
            Text("Its agent stops in the middle of what it's doing. You can pick it up again under Archived.")
        }
    }
}
