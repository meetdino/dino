import AppKit
import DinoGhostty
import SwiftUI

/// The app, or, run by dinod's launch agent, what keeps dinod (LaunchAgent.swift: `DinodHost`).
@main
enum Launch {
    static func main() {
        DinodHost.runIfAsked()
        // The app is dino's (it quits with a sheet up): made first, as SwiftUI's own setup asks
        // for `NSApp` before reading Info.plist's `NSPrincipalClass`.
        _ = DinoApplication.shared
        DinoApp.main()
    }
}

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
                    .help("Ask again whether to keep agents running when you quit dino")
                Button("Install Command Line Tool…") { CommandLineTool.install() }
                    .disabled(DinoEnvironment.bundledDino == nil)
                    .help("Install the dino command so you can run it from any terminal")
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
                // Beside Preview's ⌥⌘P and Tasks' ⌥⌘T; ⇧⌘D is Split Down, as in Ghostty.
                Button(model.showReview ? "Hide Changes" : "Review Changes") { model.showReview.toggle() }
                    .keyboardShortcut("c", modifiers: [.command, .option])
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
                // One action, no questions: the fork opens in a tab beside it, waiting for a prompt.
                Button("Fork Session") { if let s = model.selectedSession { model.forkNow(s) } }
                    .keyboardShortcut("b", modifiers: [.command, .option])
                    .disabled(!(model.selectedSession.map(model.canFork) ?? false))
                // A name, a worktree of its own, a first prompt.
                Button("Fork with Options…") { model.forking = model.selectedSession }
                    .keyboardShortcut("b", modifiers: [.command, .option, .control])
                    .disabled(!(model.selectedSession.map(model.canFork) ?? false))
                Button("Create Pull Request…") { model.showCreatePR = true }
                    .disabled(model.selectedSession.map { $0.host != nil || model.pr(of: $0) != nil } ?? true)
                OpenInMenuItems().environmentObject(model)
                Divider()
                if let s = model.selectedSession { UsingMenuItems(session: s).environmentObject(model) }
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
                Button("Rename…") { if let id = model.selectedSession?.id { model.renaming = Renaming(id: id, place: .tab) } }
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
        // The quit Apple Event (macOS's Quit & Reopen, logout, `osascript … quit`): AppKit's own
        // handler refuses it while a sheet is up without calling `terminate`, so it's dino's.
        NSAppleEventManager.shared().setEventHandler(
            self, andSelector: #selector(quitEvent(_:withReplyEvent:)),
            forEventClass: AEEventClass(kCoreEventClass), andEventID: AEEventID(kAEQuitApplication))
        // Before the first window draws: no flash of the Mac's look when dino is set otherwise.
        Appearance.current.apply()
        // Opening the app shows the sessions, even if it quit while looking at the archive.
        if UserDefaults.standard.string(forKey: "sidebar.filter") == SessionFilter.archived.rawValue {
            UserDefaults.standard.set(SessionFilter.all.rawValue, forKey: "sidebar.filter")
        }
    }

    func applicationDidFinishLaunching(_: Notification) {
        // Run as a regular app with a Dock icon and menu bar even when launched from a binary, and
        // come forward then, as nothing else brings it. Opened by Launch Services (Finder, the Dock,
        // `open`; launchd is its parent), coming forward is Launch Services' call: `open -g`, and
        // dino reopening after an update while you're elsewhere (`Updates.reopenAfterQuit`), stay
        // behind your windows.
        NSApp.setActivationPolicy(.regular)
        if getppid() != 1 { NSApp.activate(ignoringOtherApps: true) }
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
            guard let model = self?.model, let w = e.window, !(w is NSPanel), w.attachedSheet == nil,
                  w.identifier?.rawValue != SettingsView.windowID,
                  Self.desktopKey(e, model: model) else { return e }
            return nil
        }
    }

    private var desktopKeys: Any?
    private let services = ServiceProvider()

    /// Back at the app: settings may have changed on another Mac, so sync looks now rather than at
    /// its next minute; and the app may have been rebuilt in place (Restart to Update).
    func applicationDidBecomeActive(_: Notification) {
        Task.detached { _ = try? DinoConnection(path: DinoEnvironment.socketPath).send(["type": "sync", "action": "now"]) }
        Updates.shared.checkRebuilt()
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

    /// Quits as ⌘Q does. Still running after it, the quit was called off: its sender hears so, as
    /// from AppKit's own handler.
    @objc private func quitEvent(_: NSAppleEventDescriptor, withReplyEvent reply: NSAppleEventDescriptor) {
        NSApp.terminate(nil)
        reply.setParam(NSAppleEventDescriptor(int32: Int32(userCanceledErr)), forKeyword: AEKeyword(keyErrorNumber))
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
        alert.informativeText = (working > 0 ? (count == 1 ? "It's working right now. " : "\(working) \(working == 1 ? "is" : "are") working right now. ") : "")
            + "If you keep \(count == 1 ? "it" : "them") running, \(count == 1 ? "it continues" : "they continue") while dino is closed."
            + " If you stop \(count == 1 ? "it" : "them"), \(count == 1 ? "it resumes" : "they resume") the next time you open dino."
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
        .sheet(isPresented: $model.showNewSession) { NewSessionSheet() }
        .sheet(item: $model.startRequest) { StartSessionSheet(request: $0) }
        .sheet(item: $model.tmuxLook) { TmuxLook(session: $0) }
        .sheet(isPresented: $model.showNewProject) { NewProjectSheet() }
        .sheet(isPresented: $model.showShortcuts) { ShortcutSheet() }
        .sheet(isPresented: $model.showPalette) { CommandPalette() }
        .sheet(item: $model.askingAbout) { AskSheet(session: $0) }
        .sheet(item: $model.forking) { ForkSheet(session: $0) }
        .sheet(item: $model.editingTask) { ScheduleSheet(task: $0) }
        .alert(
            "Delete “\(model.deletingTask?.name ?? "")”?",
            isPresented: Binding(get: { model.deletingTask != nil }, set: { if !$0 { model.deletingTask = nil } }),
            presenting: model.deletingTask
        ) { t in
            Button("Delete", role: .destructive) { model.deleteTask(t) }
            Button("Cancel", role: .cancel) {}
        } message: { _ in
            Text("This automation won't run again. Sessions it already started keep running.")
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
                ? "The changes are applied to the checkout the worktree came from, uncommitted, so you can review them. Its sessions stop, and the worktree and its branch are removed."
                : "Its sessions stop. The worktree, its branch and all of its changes are removed.")
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
                ? "It's working right now. When its current turn ends, dino closes it in \(f.terminal ?? "the other terminal") and continues the conversation here."
                : "dino closes it in \(f.terminal ?? "the other terminal") and continues the conversation here, with its full history.")
        }
        .alert(
            "dino can't tell which conversation this \(model.unsureMove?.agentName ?? "agent") is on",
            isPresented: Binding(get: { model.unsureMove != nil }, set: { if !$0 { model.unsureMove = nil } }),
            presenting: model.unsureMove
        ) { _ in
            Button("OK", role: .cancel) {}
        } message: { f in
            Text("\(f.unsure?.why ?? "") So it can't continue it here. It keeps running in \(f.terminal ?? "the other terminal").")
        }
        .background(WelcomeCard())
    }
}

// MARK: - Terminals

struct Terminals: View {
    @EnvironmentObject var model: DinoModel
    @ObservedObject private var chrome = SplitChrome.shared
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
                    // Every session stays mounted; only the selected one (and the rest of its split) draws.
                    ForEach(model.sessions) { s in
                        let rect = layout.surface(s.id)
                        let state = model.terminal(for: s.id)
                        TerminalPane(model: model, state: state, id: s.id, visible: rect != nil, focused: s.id == model.selected)
                            // A new state (after reconnecting) must mean a new surface.
                            .id(ObjectIdentifier(state))
                            .overlay {
                                // The rest of a split sits back a little, or as Ghostty's
                                // `unfocused-split-opacity` and `unfocused-split-fill` say (SplitChrome).
                                if split != nil, s.id != model.selected, chrome.dim > 0 {
                                    Color(nsColor: chrome.fill).opacity(chrome.dim).allowsHitTesting(false)
                                }
                            }
                            // Last: an offset moves only the drawing, so anything added after it would sit unmoved.
                            .placed(rect ?? CGRect(origin: .zero, size: geo.size))
                            .opacity(rect != nil ? 1 : 0)
                            .allowsHitTesting(rect != nil)
                    }
                    if let split {
                        ForEach(split.shownPanes, id: \.self) { id in
                            if let s = model.sessions.first(where: { $0.id == id }), let f = layout.frame(id) {
                                PaneHeader(session: s, split: split, focused: id == model.selected)
                                    .placed(CGRect(x: f.minX, y: f.minY, width: f.width, height: PaneLayout.header))
                            }
                        }
                        ForEach(layout.dividers, id: \.path) { d in
                            // Wider than the line, to grab.
                            let hit = d.vertical ? d.line.insetBy(dx: 0, dy: -3) : d.line.insetBy(dx: -3, dy: 0)
                            SplitDivider(divider: d).placed(hit)
                        }
                    }
                }
                .coordinateSpace(name: Self.space)
                .onAppear { model.paneArea = geo.size }
                .onChange(of: geo.size) { _, size in model.paneArea = size }
            }
        }
        .toolbar {
            // The toolbar reads left to right as what the session is set to, then what you can
            // open: leading, over the tabs, the selected session's mode, model and effort (in a
            // split, the focused pane's); trailing, the panes. Its name is its tab's, and its
            // pane's in a split, not said again here; a plain shell leaves the toolbar bare.
            ToolbarItem(placement: .navigation) {
                if let s = model.sessions.first(where: { $0.id == model.selected }) {
                    // Its settings come from its own observable: a change there redraws this, not
                    // the terminals.
                    Live(s) { s in SessionToolbarItem(session: s) }
                }
            }
            // Pinned to the trailing edge on every screen: on macOS 26 and later the toolbar lays its
            // primary actions out after the principal item, so with no session (an empty principal)
            // they slid over to the sidebar toggle. The flexible space holds them at the right.
            TrailingToolbarSpace()
            ToolbarItem(placement: .primaryAction) { PRToolbarButton() }
            ToolbarItem(placement: .primaryAction) { SidePanePicker() }
        }
        .overlay(alignment: .bottomTrailing) {
            if let s = model.selectedSession, let u = s.local_url, model.offered[s.id] != u, model.sidePane != .preview {
                PreviewOffer(session: s, url: u)
            } else if !model.leftovers.isEmpty {
                LeftoversOffer(leftovers: model.leftovers)
            } else {
                BuildCacheOfferSlot()
            }
        }
        .animation(.spring(duration: 0.3), value: model.selectedSession?.local_url)
    }
}

/// What pushes the trailing items to the window's edge: a flexible space where the toolbar has
/// one (macOS 26 and later), else nothing, since earlier systems keep primary actions trailing.
struct TrailingToolbarSpace: ToolbarContent {
    var body: some ToolbarContent {
        if #available(macOS 26, *) {
            ToolbarSpacer(.flexible, placement: .primaryAction)
        } else {
            ToolbarItem(placement: .primaryAction) { EmptyView() }
        }
    }
}

/// Changes, Preview and Tasks as one control at the toolbar's trailing edge, like Xcode's inspector
/// toggles: each shows its pane in place of the others, and the open one stays pressed. There only
/// while it has something to act on: a session is selected, or one of its panes is open.
struct SidePanePicker: View {
    @EnvironmentObject var model: DinoModel

    var body: some View {
        let session = model.selectedSession
        let remote = session?.remoteReason
        let running = session.map { $0.exited ? 0 : $0.tasks?.running ?? 0 } ?? 0
        let preview = model.sidePane == .preview, tasks = model.sidePane == .tasks
        if session != nil || preview || tasks || model.showReview {
            ControlGroup {
                Toggle(isOn: Binding(get: { model.showReview }, set: { model.showReview = $0 })) {
                    Label("Changes", systemImage: "plus.forwardslash.minus")
                }
                .help(remote ?? "Changes (⌥⌘C): review what this session changed and leave comments for the agent")
                .disabled(session == nil || (remote != nil && !model.showReview))
                Toggle(isOn: Binding(get: { preview }, set: { _ in model.togglePreview() })) {
                    Label("Preview", systemImage: "globe")
                }
                .help(remote ?? "Preview (⌥⌘P): view this session's dev server or any local page")
                .disabled(remote != nil && !preview)
                Toggle(isOn: Binding(get: { tasks }, set: { _ in model.toggleTasks() })) {
                    Label(running > 0 ? "Tasks, \(running) running" : "Tasks", systemImage: "checklist")
                }
                .help(running > 0
                    ? "Tasks (⌥⌘T): \(running) subagents or commands running in the background"
                    : session?.reportsTasks == true ? "Tasks (⌥⌘T): the agent's to-do list, subagents and background commands" : "Tasks (⌥⌘T): this agent doesn't report its tasks")
                .disabled(!(session?.reportsTasks ?? false) && !tasks)
            }
            .toggleStyle(.button)
            .labelStyle(.iconOnly)
        }
    }
}

struct TerminalPane: View {
    /// Not watched: the pane draws nothing of the model's, so a change there needn't redraw every pane.
    let model: DinoModel
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
            // As Ghostty labels its panes for VoiceOver; the terminal's text is the view's own.
            .accessibilityElement(children: .contain)
            .accessibilityLabel("Terminal pane")
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
        .help("New session: choose a folder or repository, then an agent. ⌘N starts your default agent in the current folder.")
        .accessibilityLabel("New Session")
        .disabled(model.launchers.isEmpty)
    }
}

struct EmptyState: View {
    @EnvironmentObject var model: DinoModel

    var body: some View {
        VStack(spacing: 18) {
            DinoMark(size: 34)
            Text("One place for every agent on your Mac").foregroundStyle(.secondary)
            if let error = model.error {
                Text(error).foregroundStyle(SessionStatus.exited.color).font(.callout)
            }
            if model.daemonDown {
                Text("dino's background service isn't running. Your sessions are saved and resume when it starts.")
                    .foregroundStyle(.secondary).font(.callout)
                Button("Start Background Service") { model.startDaemon() }.controlSize(.large)
            }
            if !model.daemonDown {
                Button {
                    model.loadFound()
                    model.showContinue = true
                } label: {
                    Label("Continue a Session…", systemImage: "arrow.uturn.forward").frame(width: 240)
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
                .help("Choose a folder, a recent repository, one of your GitHub repositories or a URL to clone, then an agent. ⌘N starts your default agent in the current folder.")
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
    /// The row the keyboard is on when it has nothing of its own to show (see the list's
    /// selection): highlighted while the main area keeps the selected session.
    @State private var highlighted: String?

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
        List(selection: Binding(get: { highlighted ?? model.selected }, set: { tag in
            // Rows outside "Agents" carry a `move:` tag: ask before handing that session over.
            // Anything else that isn't a session (or deselecting) leaves the selection alone.
            guard let tag else { return }
            // Arrowed onto: the keyboard stays in the list. Clicked: the terminal takes it.
            let keys = NSApp.currentEvent?.type == .keyDown
            if !model.showsSomething(tag) {
                // A row with nothing of its own to show ("Other worktrees", a repo's, a worktree's
                // over its sessions) never becomes the selection: the session stays on screen.
                // Arrowed onto, it's only highlighted (Return, → and ← open and close it); clicked,
                // it opens or closes, and the highlight goes back to the session. After the table's
                // own selection callback: rows coming and going from inside it is a reentrant
                // update NSTableView can crash on.
                if keys {
                    highlighted = tag
                } else {
                    model.startHere(tag)
                    highlighted = tag
                    DispatchQueue.main.async {
                        setOpen(tag, nil)
                        highlighted = nil
                    }
                }
                return
            }
            highlighted = nil
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
                    ? SessionTree.build(repos: model.repos, sessions: model.sidebarSessions)
                    : SessionTree.build(repos: model.repos, sessions: model.sidebarSessions) {
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
                        Button { model.newTask() } label: { Image(systemName: "plus") }
                            .buttonStyle(.plain)
                            .help("New Automation")
                            .accessibilityLabel("New Automation")
                    }
                    ForEach(model.scheduled) { t in
                        ScheduledRow(task: t).tag("task:\(t.id)")
                        if model.openTasks.contains(t.id) {
                            TaskRuns(task: t)
                        }
                    }
                    if model.scheduled.isEmpty {
                        // A few ready ones to start from, and the rest a click away.
                        ForEach(AutomationTemplate.suggested, id: \.id) { s in
                            if let t = AutomationTemplate.named(s.id) {
                                Button { model.newTask(from: t) } label: {
                                    Label { Text(s.short).lineLimit(1) } icon: { Image(systemName: t.icon).foregroundStyle(.secondary) }
                                }
                                .buttonStyle(.plain)
                                .font(.callout)
                                .foregroundStyle(.secondary)
                                .help(t.blurb)
                            }
                        }
                        Button { model.newTask() } label: {
                            Label { Text("More templates…") } icon: { Image(systemName: "square.grid.2x2").foregroundStyle(.tertiary) }
                        }
                        .buttonStyle(.plain)
                        .font(.callout)
                        .foregroundStyle(.tertiary)
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
            } else if tags.count == 1, let tag = tags.first, !model.showsSomething(tag) {
                // Return opens or closes it, as → and ← do; a click already did (see above).
                if returnKey { DispatchQueue.main.async { setOpen(tag, nil) } }
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
        // A header highlighted from the keyboard gives way to whatever is selected next, and to
        // the terminal when it takes the keyboard back.
        .onChange(of: model.selected) { highlighted = nil }
        .onChange(of: model.focusedTerminal) { _, id in if id != nil { highlighted = nil } }
    }

    private func openSelected(_ open: Bool) -> KeyPress.Result {
        guard filter != .archived, let tag = highlighted ?? model.selected else { return .ignored }
        return setOpen(tag, open)
    }

    /// Opens (true), closes (false) or flips (nil) the row tagged `tag`, if it's one that opens.
    @discardableResult
    private func setOpen(_ tag: String, _ open: Bool?) -> KeyPress.Result {
        let tree = SessionTree.build(repos: model.repos, sessions: model.sidebarSessions)
        guard let (key, startsOpen) = tree.repos.lazy.compactMap({ $0.opening(tag) }).first else { return .ignored }
        var set = collapsed.wrappedValue
        let isOpen = startsOpen != set.contains(key)
        let want = open ?? !isOpen
        guard isOpen != want else { return .ignored }
        if want == startsOpen { set.remove(key) } else { set.insert(key) }
        collapsed.wrappedValue = set
        return .handled
    }

    /// An agent running in another terminal: shown where tmux keeps it, or offered to move to dino.
    private func actOnElsewhere(_ tag: String) {
        if tag.hasPrefix("tmux:") {
            if let f = model.elsewhere.first(where: { "tmux:\($0.id)" == tag }) { model.showInTmux(f) }
        } else {
            if let f = model.elsewhere.first(where: { "move:\($0.id)" == tag }) { model.askToMove(f) }
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
    /// The session as it is now: how it's doing changes here, redrawing this row, not the sidebar.
    @ObservedObject private var live: LiveSession
    let index: Int
    /// The worktree's branch, on the row instead of a header above a single session.
    var branch: String?
    /// The repo or worktree it's filed under: a shell's folder shows only when it's somewhere else.
    var root: String?
    @State private var hovering = false
    @State private var anchor = CostAnchorView()

    init(session: SessionInfo, index: Int, branch: String? = nil, root: String? = nil) {
        _live = ObservedObject(wrappedValue: LiveSessions.of(session))
        self.index = index
        self.branch = branch
        self.root = root
    }

    private var session: SessionInfo { live.info }

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
                if let split = model.split(of: session.id) {
                    let others = split.panes.filter { $0 != session.id }.map { id in model.sessions.first { $0.id == id }?.display ?? "another session" }
                    let other = ListFormatter.localizedString(byJoining: others)
                    Image(systemName: split.panes.count > 2 ? "rectangle.split.3x1" : "rectangle.split.2x1")
                        .font(.caption).foregroundStyle(.tertiary)
                        .help("In a split with \(other)")
                        .accessibilityLabel("In a split with \(other)")
                }
                Spacer(minLength: 4)
                if session.pinned == true {
                    Image(systemName: "pin.fill")
                        .font(.caption2).foregroundStyle(.tertiary)
                        .rotationEffect(.degrees(45))
                        .help("Pinned: stays at the top of its group and is never archived automatically")
                        .accessibilityLabel("Pinned")
                }
                if hovering, model.canArchive(session.id) {
                    // Where Claude desktop has it: on the row, under the pointer.
                    Button { model.archive(session.id) } label: { Image(systemName: "archivebox") }
                        .buttonStyle(.plain)
                        .foregroundStyle(.secondary)
                        .help("Archive (⇧⌘A): stop this session and move it to Archived, where you can resume it later")
                        .accessibilityLabel("Archive")
                } else {
                    Text(status.label).font(.caption).foregroundStyle(status.color).lineLimit(1).fixedSize()
                        .help(status.detail ?? status.label)
                }
            }
            if let from = session.forked_from {
                ForkedFromLine(from: from)
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
            // On the account every other session is on too: the footer says it once.
            if let f = session.fallback, !model.saysInFooter(f) {
                FallbackLine(fallback: f).help(FallbackChip.detail(f, session.usage_by_route ?? []))
            }
            if let error = session.error {
                ErrorLine(message: error)
            }
            PeerChips(session: session)
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

    /// The model it's on: as its agent says (routing off too), else as its last call asked.
    private var modelText: String? {
        guard session.agent_model != nil || session.requests > 0, let now = session.modelNow else { return nil }
        let m = shortModel(now)
        return session.tier.map { "\($0) → \(m)" } ?? m
    }

    private var usageHelp: String {
        var lines: [String] = []
        if let m = session.modelNow { lines.append(m) }
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
/// It answers "can my agents work right now, and on what?": with more than one Claude account and
/// one of them spent, the account Claude Code's calls go to now comes first with its own windows,
/// a spent one is said in a line with when it's back, and which sessions move back when.
struct UsagePanel: View {
    @EnvironmentObject var model: DinoModel
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.openWindow) private var openWindow
    @AppStorage("usage.open") private var open = false
    private static let names = ["anthropic": "Claude", "chatgpt": "Codex"]

    /// The accounts, when they're worth telling apart: one is spent, or sessions are on another.
    private var accounts: [ClaudeAccountInfo]? {
        let a = model.claudeAccounts
        guard a.count > 1 else { return nil }
        let split = a.contains { $0.spent } || a.first { $0.answering }?.number != 1 || model.sessions.contains { $0.fallback?.isAccount == true }
        return split ? a : nil
    }

    /// The windows shown as bars: every provider's, but Claude's by account when they're told apart.
    private func windows(_ accounts: [ClaudeAccountInfo]?) -> [(label: String, window: WindowInfo)] {
        model.quotas.filter { accounts == nil || $0.provider != "anthropic" }.flatMap { q in
            q.windows.filter(\.isWindow).map { ("\(Self.names[q.provider] ?? q.provider) \($0.name)", $0) }
        }
    }

    var body: some View {
        let accounts = accounts
        let answering = accounts?.first { $0.answering }
        let windows = windows(accounts)
        // The window closest to its limit is the one that stops work: of the account in use, and
        // only windows still current (one past its reset hasn't been reported since).
        let current = windows + (answering?.windows ?? []).filter(\.isWindow).map { ("Claude \($0.name)", $0) }
        let fullest = current.filter { !$0.window.isPast }.max { $0.window.utilization < $1.window.utilization }
        let allSpent = accounts != nil && answering == nil
        VStack(alignment: .leading, spacing: 8) {
            AwakeStatus()
            Button { withAnimation(.easeOut(duration: 0.15)) { open.toggle() } } label: {
                HStack(spacing: 6) {
                    Image(systemName: "chevron.right")
                        .font(.caption2.weight(.semibold))
                        .rotationEffect(.degrees(open ? 90 : 0))
                        .foregroundStyle(.tertiary)
                    Text("Usage").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                    if !open {
                        if allSpent, let back = accounts?.compactMap(\.backAt).min() {
                            Spacer(minLength: 0)
                            Text("Claude back \(Clock.short(back))").font(.caption).foregroundStyle(SessionStatus.exited.color).lineLimit(1)
                        } else {
                            if let answering, answering.number != 1 {
                                Text("account \(answering.number)").font(.caption).foregroundStyle(.secondary).lineLimit(1).fixedSize()
                            }
                            if let fullest {
                                let pct = Double(fullest.window.utilization)
                                ProgressView(value: min(max(pct, 0), 1)).tint(QuotaBar.color(pct)).controlSize(.mini)
                                Text("\(Int((pct * 100).rounded()))%").font(.caption.monospacedDigit()).foregroundStyle(QuotaBar.color(pct))
                            } else {
                                Spacer(minLength: 0)
                            }
                        }
                    } else {
                        Spacer(minLength: 0)
                    }
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help(summary(answering: answering, fullest: fullest) + " Click for every window.")
            .accessibilityLabel("Usage")
            .accessibilityValue(summary(answering: answering, fullest: fullest))
            .accessibilityHint(open ? "Collapses usage" : "Shows every usage window")
            .contextMenu {
                Button("Usage Stats…") { openWindow(id: StatsView.windowID) }
            }
            if open {
                if let accounts {
                    ClaudeAccountsUsage(accounts: accounts, sessions: model.sessions)
                }
                ForEach(windows, id: \.label) { w in
                    QuotaBar(label: w.label, window: w.window)
                }
                if windows.isEmpty && accounts == nil {
                    Text("No quota data yet").font(.caption).foregroundStyle(.tertiary)
                }
                SessionTokenLines()
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

    /// The collapsed line in words: "Claude account 2 in use, Claude 5h 12% used".
    private func summary(answering: ClaudeAccountInfo?, fullest: (label: String, window: WindowInfo)?) -> String {
        var parts: [String] = []
        if let answering, answering.number != 1 { parts.append("\(answering.short) in use") }
        if let fullest { parts.append("\(fullest.label) \(Int((Double(fullest.window.utilization) * 100).rounded()))% used") }
        if accounts != nil, answering == nil { parts.append("Every Claude account is at its limit") }
        return parts.isEmpty ? "No data yet." : parts.joined(separator: ", ") + "."
    }
}

/// The usage panel's token totals for the sessions in the sidebar. They tick with every model call,
/// so they're observed on their own (SessionTokens), not through the model.
struct SessionTokenLines: View {
    @ObservedObject private var counts = SessionTokens.shared

    var body: some View {
        // Tokens worked through, not the context read again from the prompt cache on every
        // call: summed over a long session, those re-reads are billions and say little.
        let t = counts.totals
        if t.used > 0 {
            HStack {
                Text("Open sessions").foregroundStyle(.secondary)
                Spacer()
                Text("\(tokens(t.used)) tok")
            }
            .font(.caption.monospacedDigit())
            .help("Tokens in and out for the sessions in the sidebar, across their whole conversations"
                + (t.cached > 0 ? ", not counting the \(tokens(t.cached)) read again from the prompt cache (each call reads the conversation so far again)" : "")
                + ". Only traffic routed through dino is counted, so Copilot, Cursor and Amp aren't included.")
        }
        if t.free > 0 {
            HStack {
                Text("Free models").foregroundStyle(.secondary)
                Spacer()
                Text("\(tokens(t.free)) tok")
                Text("$0.00").foregroundStyle(Brand.green)
            }
            .font(.caption.monospacedDigit())
        }
    }
}

/// The footer's Claude accounts, once one is spent or sessions are on another: the one in use with
/// its windows, each spent one in a line with when it's back, and when sessions move back.
struct ClaudeAccountsUsage: View {
    let accounts: [ClaudeAccountInfo]
    let sessions: [SessionInfo]

    var body: some View {
        let answering = accounts.first { $0.answering }
        VStack(alignment: .leading, spacing: 6) {
            if let answering {
                HStack(spacing: 5) {
                    Text(answering.short).font(.caption.weight(.semibold))
                    Spacer()
                    Text("in use").font(.caption).foregroundStyle(Color(nsColor: .systemGreen))
                }
                .accessibilityElement(children: .combine)
                .help("Claude Code's calls go to \(answering.short) now\(answering.isOwn ? ", the account it signed in with" : "").")
                let windows = (answering.windows ?? []).filter(\.isWindow)
                if windows.isEmpty {
                    // Never a made-up 0%: Anthropic says an account's use on the calls it signs.
                    Text("Its use isn't reported yet").font(.caption).foregroundStyle(.tertiary).lineLimit(1)
                        .help("Anthropic reports an account's 5h and 7d use with the calls it answers; none has reported it to dino yet.")
                } else {
                    ForEach(windows, id: \.name) { w in
                        QuotaBar(label: w.name, window: w)
                    }
                }
            }
            ForEach(accounts.filter { !$0.answering }) { a in
                HStack(spacing: 5) {
                    Circle().fill(a.spent ? Color.orange : Color.secondary.opacity(0.5)).frame(width: 6, height: 6)
                    Text(a.short)
                    Spacer()
                    if a.spent {
                        Text(a.backAt.map { "back at \(Clock.short($0))" } ?? "at its limit").foregroundStyle(.secondary)
                    } else {
                        Text("ready").foregroundStyle(.tertiary)
                    }
                }
                .font(.caption.monospacedDigit())
                .accessibilityElement(children: .combine)
                .help(help(a))
            }
            if let note = note(answering) {
                // At most two lines, never sized to its text: wrapped at the narrowest width the
                // window measures, it would hold the window taller than the screen.
                Text(note).font(.caption).foregroundStyle(.secondary).lineLimit(2).help(note)
            }
        }
    }

    private func help(_ a: ClaudeAccountInfo) -> String {
        guard a.spent else { return "\(a.short) answers when the accounts before it are at their limit." }
        let windows = (a.windows ?? []).filter(\.isWindow).map { "\($0.name) \(Int((Double($0.utilization) * 100).rounded()))%" }
        let back = a.resets_at.map { "Its limit resets \(Clock.short($0))." } ?? a.retry_at.map { "dino tries it again \(Clock.short($0))." } ?? ""
        return (["\(a.short)\(a.isOwn ? ", the one Claude Code signed in with," : "") is at its limit.", back] + (windows.isEmpty ? [] : ["Last reported: " + windows.joined(separator: ", ") + "."])).filter { !$0.isEmpty }.joined(separator: " ")
    }

    /// One line on when sessions move: back to account 1 once it resets, or now, at their next turn.
    private func note(_ answering: ClaudeAccountInfo?) -> String? {
        let elsewhere = sessions.filter { s in s.fallback.map { $0.isAccount && $0.name != answering?.short } ?? false }
        if let answering, !elsewhere.isEmpty {
            let n = elsewhere.count
            return "\(n == 1 ? "1 session" : "\(n) sessions") on another account \(n == 1 ? "moves" : "move") to account \(answering.number) at \(n == 1 ? "its" : "their") next turn."
        }
        guard let answering else {
            // Every one spent: Claude Code's calls are turned down until the first is back.
            let back = accounts.compactMap(\.backAt).min()
            return "Every Claude account is at its limit" + (back.map { ": the first is back at \(Clock.short($0))." } ?? ".")
        }
        guard answering.number != 1, let own = accounts.first(where: \.isOwn), own.spent, let back = own.backAt else { return nil }
        return "Sessions go back to account 1 at their next turn after \(Clock.short(back))."
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
                if window.isPast {
                    // Its window ended: what it used since isn't known until a call reports it.
                    Text("reset · not reported since").foregroundStyle(.tertiary)
                } else {
                    Text("\(Int((pct * 100).rounded()))%").foregroundStyle(color)
                    if let reset = window.resets_at {
                        Text("· \(resetText(reset))").foregroundStyle(.tertiary)
                    }
                }
            }
            .font(.caption.monospacedDigit())
            if !window.isPast {
                ProgressView(value: min(max(pct, 0), 1)).tint(color).controlSize(.small)
            }
        }
        .accessibilityElement(children: .combine)
    }

    static func color(_ pct: Double) -> Color {
        pct < 0.6 ? Color(nsColor: .systemGreen) : pct < 0.85 ? SessionStatus.needsYou.color : SessionStatus.exited.color
    }
}

// MARK: - Formatting

func tokens(_ n: UInt64) -> String {
    switch n {
    case ..<1000: "\(n)"
    case 1_000..<999_950: String(format: "%.1fk", Double(n) / 1e3)
    case 999_950..<999_950_000: String(format: "%.1fM", Double(n) / 1e6)
    default: String(format: "%.2fB", Double(n) / 1e9)
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
                    .help("Stop waiting. The session keeps running in \(session.terminal ?? "the other terminal").")
            } else if session.asking {
                Image(systemName: "exclamationmark.circle.fill").foregroundStyle(SessionStatus.needsYou.color)
                    .help("Needs you in tmux. Click to go there.")
            } else if session.tmux != nil {
                Image(systemName: "rectangle.split.2x1").foregroundStyle(.secondary)
                    .help("Running in tmux. Click to show it there.")
            } else if let unsure = session.unsure {
                Image(systemName: "questionmark.circle").foregroundStyle(.secondary)
                    .help(unsure.why)
                    .accessibilityLabel("dino can't tell which conversation it's on")
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
            Button(session.unsure == nil ? "Continue in dino…" : "Why dino Can't Continue It…") { model.askToMove(session) }
            Divider()
            // Until it ends; the session browser's Show Hidden brings it back sooner.
            Button("Hide") { model.hide(session) }
        }
    }
}

func whereText(_ f: FoundSession) -> String {
    var parts: [String] = []
    if let t = f.terminal { parts.append("in \(t)") }
    if let s = f.status { parts.append(s == "needs" ? "needs you" : s) }
    if f.unsure != nil { parts.append("can't tell which conversation") }
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
            Text("The agent stops in the middle of what it's doing. You can resume the session from Archived.")
        }
    }
}

/// dino's application (Info.plist's `NSPrincipalClass`), which quits with a sheet up.
///
/// AppKit refuses to quit while a window has a sheet attached (the Welcome card, New Session…):
/// `terminate` returns without asking the delegate ("App termination blocked by modal sheet"), so
/// ⌘Q, the menu and macOS's Quit & Reopen after a permission grant did nothing. Asked to quit, it
/// has the sheets taken down first, by their owners where they listen (the Welcome card, which a
/// quit doesn't count as seen), else as Esc takes them down, then quits as asked. A quit called
/// off (Cancel) puts the listening ones back.
@objc(DinoApplication)
final class DinoApplication: NSApplication {
    /// Posted before a quit takes the sheets down, and when it's called off.
    static let quitting = Notification.Name("dino.quitting")
    static let quitCalledOff = Notification.Name("dino.quitCalledOff")
    private var quitting = false

    override func terminate(_ sender: Any?) {
        let sheets = { self.windows.compactMap { w in w.attachedSheet.map { (w, $0) } } }
        guard !quitting, !sheets().isEmpty else { return super.terminate(sender) }
        quitting = true
        defer { quitting = false }
        NotificationCenter.default.post(name: Self.quitting, object: nil)
        // They go as SwiftUI next draws: run till then, so the quit goes on from here (the quit
        // Apple Event still current: a logout's never waits on the question). One whose owner
        // doesn't listen is closed as Esc closes it (ended under SwiftUI, it comes straight back).
        let until = Date.now.addingTimeInterval(1)
        var escaped = false
        while !sheets().isEmpty, Date.now < until {
            RunLoop.current.run(mode: .default, before: .now.addingTimeInterval(0.05))
            if !escaped, Date.now > until.addingTimeInterval(-0.8) {
                escaped = true
                for (_, sheet) in sheets() { (sheet.firstResponder ?? sheet).doCommand(by: #selector(NSResponder.cancelOperation(_:))) }
            }
        }
        super.terminate(sender)
        // Still running: the quit was called off.
        NotificationCenter.default.post(name: Self.quitCalledOff, object: nil)
    }
}
