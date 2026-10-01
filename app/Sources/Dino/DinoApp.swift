import AppKit
import GhosttyTerminal
import SwiftUI

@main
struct DinoApp: App {
    @NSApplicationDelegateAdaptor private var delegate: AppDelegate
    @StateObject private var model = DinoModel()
    @AppStorage(QuitChoice.key) private var quitChoice = ""
    @Environment(\.openWindow) private var openWindow

    var body: some Scene {
        WindowGroup("dino") {
            ContentView()
                .environmentObject(model)
                .frame(minWidth: 820, minHeight: 480)
                .onAppear {
                    delegate.model = model
                    Notifier.onOpenSession = { model.select($0) }
                    Notifier.setUp()
                    model.start()
                }
        }
        .windowStyle(.hiddenTitleBar)
        .commands {
            CommandGroup(replacing: .appSettings) {
                Button("Settings…") { openWindow(id: SettingsView.windowID) }
                    .keyboardShortcut(",")
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
            CommandMenu("Session") {
                Button("New Shell") { model.newShell() }
                    .keyboardShortcut("t")
                    .disabled(model.launchers.isEmpty)
                // Its shortcut works from any app (Settings → General); a menu key would only work here.
                Button("Quick Terminal") { QuickTerminal.shared.toggle() }
                    .disabled(model.launchers.isEmpty)
                Menu("New Session") {
                    // dinod lists the default agent first: ⌘N starts it.
                    ForEach(model.launchers) { l in
                        if l == model.launchers.first {
                            Button(l.label) { model.newSession(l) }.keyboardShortcut("n")
                        } else {
                            Button(l.label) { model.newSession(l) }
                        }
                    }
                }
                Menu("New Session in Worktree") {
                    ForEach(model.launchers) { l in
                        if l == model.launchers.first {
                            Button(l.label) { model.newSession(l, worktree: true) }.keyboardShortcut("n", modifiers: [.command, .option])
                        } else {
                            Button(l.label) { model.newSession(l, worktree: true) }
                        }
                    }
                }
                Button("New Session…") { model.showNewSession = true }
                    .keyboardShortcut("n", modifiers: [.command, .control])
                Button("Fan Out…") { model.showFanout = true }
                    .keyboardShortcut("n", modifiers: [.command, .shift])
                Button("New Scheduled Task…") { model.newTask() }
                Button("Continue a Session…") {
                    model.loadFound()
                    model.showContinue = true
                }
                .keyboardShortcut("k")
                Button("Choose Folder…") { model.chooseFolder() }
                    .keyboardShortcut("o")
                Divider()
                SplitMenuItems().environmentObject(model)
                Divider()
                Button("Find Sessions…") { model.findingSessions = true }
                    .keyboardShortcut("f", modifiers: [.command, .shift])
                Button("Jump to Session Needing You") { model.jumpToAttention() }
                    .keyboardShortcut("j")
                Button("Next Session") { model.cycle(by: 1) }
                    .keyboardShortcut(.tab, modifiers: .control)
                    .disabled(model.sessions.count < 2)
                Button("Previous Session") { model.cycle(by: -1) }
                    .keyboardShortcut(.tab, modifiers: [.control, .shift])
                    .disabled(model.sessions.count < 2)
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
                Divider()
                ControlMenuItems().environmentObject(model)
                Divider()
                ForEach(Array(model.sessions.prefix(9).enumerated()), id: \.element.id) { i, s in
                    Button("\(i + 1)  \(s.display)") { model.select(s.id) }
                        .keyboardShortcut(KeyEquivalent(Character("\(i + 1)")))
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
                Button("Kill Session") { if let id = model.selected { model.kill(id) } }
                    .keyboardShortcut(.delete, modifiers: [.command, .shift])
                    .disabled(model.selected == nil)
            }
            CommandGroup(replacing: .help) {
                Button("Keyboard Shortcuts") { model.showShortcuts = true }
                    .keyboardShortcut("/")
            }
        }
        Window("Settings", id: SettingsView.windowID) {
            SettingsView()
                .environmentObject(model)
        }
        .windowResizability(.contentSize)
        .windowToolbarStyle(.unified)
    }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationWillFinishLaunching(_: Notification) {
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
        NSUpdateDynamicServices()
        QuickTerminal.shared.registerKey()
        desktopKeys = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] e in
            if e.window is QuickPanel { return QuickTerminal.shared.key(e) ? nil : e }
            guard let model = self?.model, let w = e.window, !(w is NSPanel), w.attachedSheet == nil,
                  w.identifier?.rawValue != SettingsView.windowID,
                  Self.desktopKey(e, model: model) else { return e }
            return nil
        }
    }

    private var desktopKeys: Any?
    private let services = ServiceProvider()

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

    func applicationShouldTerminateAfterLastWindowClosed(_: NSApplication) -> Bool { true }

    weak var model: DinoModel? {
        didSet {
            services.model = model
            QuickTerminal.shared.model = model
        }
    }

    /// Agents run in dinod, not in the app, so quitting leaves them running unless you say otherwise.
    func applicationShouldTerminate(_: NSApplication) -> NSApplication.TerminateReply {
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
        let alert = NSAlert()
        alert.messageText = count == 1 ? "Keep your agent running?" : "Keep your \(count) agents running?"
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
                Terminals()
                    .safeAreaInset(edge: .top, spacing: 0) { ApprovalBanner() }
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
        .sheet(item: $model.approving) { ApproveSheet(request: $0) }
        .sheet(isPresented: $model.showFanout) { FanoutSheet() }
        .sheet(isPresented: $model.showNewSession) { NewSessionSheet() }
        .sheet(isPresented: $model.showShortcuts) { ShortcutSheet() }
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
        .alert(
            "Something went wrong",
            isPresented: Binding(get: { model.error != nil && !model.sessions.isEmpty }, set: { if !$0 { model.error = nil } })
        ) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(model.error ?? "")
        }
        .alert(
            "Move “\(model.confirmMove?.title ?? "")” into dino?",
            isPresented: Binding(get: { model.confirmMove != nil }, set: { if !$0 { model.confirmMove = nil } }),
            presenting: model.confirmMove
        ) { f in
            Button("Move to dino") { model.adopt(f) }
                .keyboardShortcut(.defaultAction)
            Button("Cancel", role: .cancel) {}
        } message: { f in
            Text(f.isBusy
                ? "It's working right now. dino waits for the current turn to finish, closes it in \(f.terminal ?? "the other terminal") and continues the conversation here."
                : "dino closes it in \(f.terminal ?? "the other terminal") and continues the same conversation here, with its history.")
        }
        .overlay { if let f = model.moving { MovingOverlay(session: f) } }
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
            } else if model.sessions.isEmpty || model.daemonDown || model.selected?.hasPrefix("dir:") == true {
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
                        SessionName(session: s, place: .toolbar, font: .system(.body, design: .monospaced).weight(.semibold), color: Brand.green)
                        if let f = s.inside { AgentBadge(agent: f.agent) }
                        if s.label == nil, let t = s.inside?.title ?? s.title { Text(t).foregroundStyle(.secondary).lineLimit(1) }
                        if let f = s.inside, f.continuable { TakeOverButton(session: s, found: f) }
                        if let host = s.host { HostChip(host: host) }
                        if !s.exited {
                            SessionControlsBar(session: s).padding(.leading, 4)
                        } else {
                            ResumeButton(session: s).padding(.leading, 4)
                        }
                    }
                }
            }
            ToolbarItem(placement: .primaryAction) {
                Button {
                    model.loadFound()
                    model.showContinue = true
                } label: {
                    Label("Continue…", systemImage: "arrow.uturn.forward")
                }
                .help("Continue a session from another terminal, your history, or the cloud (⌘K)")
            }
            ToolbarItem(placement: .primaryAction) {
                Button { model.showFanout = true } label: {
                    Label("Fan Out…", systemImage: "arrow.triangle.branch")
                }
                .help("One prompt to several agents, each in its own worktree (⇧⌘N)")
            }
            ToolbarItem(placement: .primaryAction) {
                Menu { SplitMenuItems(shortcuts: false) } label: {
                    Label("Split", systemImage: "rectangle.split.2x1")
                }
                .help("A shell or another session next to this one (⌘D)")
                .disabled(model.selectedSession == nil)
            }
            ToolbarItem(placement: .primaryAction) { NewSessionMenu() }
            ToolbarItem(placement: .primaryAction) {
                Button { model.showReview.toggle() } label: {
                    Label("Changes", systemImage: model.showReview ? "plusminus.circle.fill" : "plusminus.circle")
                }
                .help(remoteReason ?? "Review this session's changes; click a line to comment for the agent (⇧⌘D)")
                .disabled(!model.sessions.contains { $0.id == model.selected } || remoteReason != nil)
            }
            ToolbarItem(placement: .primaryAction) {
                Button { model.togglePreview() } label: {
                    Label("Preview", systemImage: model.sidePane == .preview ? "globe.americas.fill" : "globe.americas")
                }
                .help(remoteReason ?? "Preview this session's dev server or any local page (⌥⌘P)")
                .disabled(remoteReason != nil && model.sidePane != .preview)
            }
            ToolbarItem(placement: .primaryAction) { TasksToolbarButton() }
            ToolbarItem(placement: .primaryAction) { PRToolbarButton() }
            ToolbarItem(placement: .primaryAction) { OpenInMenu() }
        }
        .overlay(alignment: .bottomTrailing) {
            if let s = model.selectedSession, let u = s.local_url, model.offered[s.id] != u, model.sidePane != .preview {
                PreviewOffer(session: s, url: u)
            }
        }
        .animation(.spring(duration: 0.3), value: model.selectedSession?.local_url)
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
                if focused { state.requestFocus() }
            }
            .onChange(of: visible) { _, v in state.isSurfaceVisible = v }
            .onChange(of: focused) { _, f in if f { state.requestFocus() } }
            // Clicking into the other half of a split selects that session.
            .onChange(of: state.isFocused) { _, f in
                if f {
                    model.focusedTerminal = id
                } else if model.focusedTerminal == id {
                    model.focusedTerminal = nil
                }
                if f, visible, !focused, model.shownSplit?.contains(id) == true { model.select(id) }
            }
    }
}

struct NewSessionMenu: View {
    @EnvironmentObject var model: DinoModel

    var body: some View {
        Menu {
            ForEach(model.launchers) { l in
                Button(l.label) { model.newSession(l) }
            }
            Menu("In a New Worktree") {
                ForEach(model.launchers) { l in
                    Button(l.label) { model.newSession(l, worktree: true) }
                }
            }
            .help("Its own worktree and branch: its edits stay off your checkout until you apply them. Ignored files listed in .worktreeinclude, like .env, are copied in")
            Divider()
            Button("New Session…") { model.showNewSession = true }
                .help("Choose the agent, folder, permission mode, model and effort (⌃⌘N)")
            Button("In \(model.folder.lastPathComponent)…") { model.chooseFolder() }
        } label: {
            Label("New Session", systemImage: "plus")
        }
        .help("New session in \(model.folder.path)")
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
            VStack(spacing: 8) {
                ForEach(model.daemonDown ? [] : model.launchers) { l in
                    Button { model.newSession(l) } label: {
                        Text(l.label).frame(width: 240)
                    }
                    .controlSize(.large)
                }
                if !model.daemonDown {
                    Button { model.showFanout = true } label: {
                        Label("Fan Out…", systemImage: "arrow.triangle.branch").frame(width: 240)
                    }
                    .controlSize(.large)
                    .help("One prompt, several agents, each in its own worktree; keep the best (⇧⌘N)")
                }
            }
            Button("In \((model.folder.path as NSString).abbreviatingWithTildeInPath) · Choose Folder…") { model.chooseFolder() }
                .buttonStyle(.link)
                .font(.callout)
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

    private var collapsed: Binding<Set<String>> {
        Binding(
            get: { Set(collapsedIDs.split(separator: "\n").map(String.init)) },
            set: { collapsedIDs = $0.sorted().joined(separator: "\n") }
        )
    }

    var body: some View {
        VStack(spacing: 0) {
            List(selection: Binding(get: { model.selected }, set: { tag in
                // Rows outside "Agents" carry a `move:` tag: ask before handing that session over.
                // Anything else that isn't a session (or deselecting) leaves the selection alone.
                guard let tag else { return }
                if tag.hasPrefix("move:") {
                    model.confirmMove = model.elsewhere.first { "move:\($0.id)" == tag }
                } else if tag.hasPrefix("task:") {
                    model.editingTask = model.scheduled.first { "task:\($0.id)" == tag }
                } else if tag.hasPrefix("repo:") {
                    // A repo's row: its folder, as its main checkout's row.
                    model.select("dir:" + tag.dropFirst(5))
                } else {
                    model.select(tag)
                }
            })) {
                if filter == .archived {
                    ArchivedSection()
                } else {
                    let narrowed = model.sidebarNarrowed
                    let tree = filter == .all && !narrowed
                        ? SessionTree.build(repos: model.repos, sessions: model.sessions, groups: model.groups)
                        : SessionTree.build(repos: model.repos, sessions: model.sessions, groups: model.groups) {
                            filter.passes(model.status(of: $0)) && model.sidebarShows($0)
                        }
                    Section("Workspaces") {
                        ForEach(tree.repos) { node in
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
                                : filter == .needsYou ? "Nothing needs you" : "No \(filter.label.lowercased()) sessions")
                                .font(.callout).foregroundStyle(.tertiary)
                        }
                    }
                    if filter == .all, !narrowed {
                        Section {
                            ForEach(model.scheduled) { t in
                                ScheduledRow(task: t).tag("task:\(t.id)")
                            }
                            if model.scheduled.isEmpty {
                                Button { model.newTask() } label: {
                                    Label("Run a prompt on a schedule…", systemImage: "clock")
                                }
                                .buttonStyle(.plain)
                                .font(.callout)
                                .foregroundStyle(.secondary)
                            }
                        } header: {
                            HStack {
                                Text("Scheduled")
                                Spacer()
                                if !model.scheduled.isEmpty {
                                    Button { model.newTask() } label: { Image(systemName: "plus") }
                                        .buttonStyle(.plain)
                                        .help("New Scheduled Task")
                                }
                            }
                        }
                    }
                    // Other terminals' sessions aren't dino's to sort by status.
                    if !model.elsewhere.isEmpty, filter == .all, !narrowed {
                        Section("On this Mac") {
                            ForEach(model.elsewhere) { f in
                                ElsewhereRow(session: f).tag("move:\(f.id)")
                            }
                        }
                    }
                }
            }
            // Not rebuilt when rows come or go (that replaced every row, and froze the window for
            // up to seconds while agents made worktrees): rows keep unique tags and stable
            // identities instead, so the list's own diff stays right.
            .listStyle(.sidebar)
            UsagePanel()
        }
        .safeAreaInset(edge: .top) {
            VStack(alignment: .leading, spacing: 8) {
                HStack {
                    DinoMark(size: 15)
                    Spacer()
                    if filter != .archived, !model.sessions.isEmpty {
                        Button { model.findingSessions = true } label: { Image(systemName: "magnifyingglass") }
                            .buttonStyle(.plain)
                            .foregroundStyle(.secondary)
                            .help("Find sessions (⇧⌘F)")
                        ScopeMenu()
                    }
                    ArchiveToggle(filter: $filter)
                }
                if filter != .archived, model.findingSessions || !model.sidebarQuery.isEmpty {
                    SessionSearchField()
                }
                if filter != .archived, model.sidebarScope != nil {
                    ScopeChip()
                }
                if !model.sessions.isEmpty || filter != .all {
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

struct SessionRow: View {
    @EnvironmentObject var model: DinoModel
    let session: SessionInfo
    let index: Int
    var stat: DiffStat?
    @State private var hovering = false

    var body: some View {
        let status = model.status(of: session)
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 8) {
                StatusDot(status: status)
                SessionName(session: session, place: .sidebar, font: .system(.body, design: .monospaced).weight(.medium))
                if session.pinned == true {
                    Image(systemName: "pin.fill")
                        .font(.caption2).foregroundStyle(.tertiary)
                        .rotationEffect(.degrees(45))
                        .help("Pinned: at the top of its group, and dino won't archive it on its own")
                }
                if let task = session.scheduled {
                    Image(systemName: "clock")
                        .font(.caption).foregroundStyle(.tertiary)
                        .help("Started by the scheduled task “\(task)”")
                }
                if let pr = model.pr(of: session) { PRChip(pr: pr, auto: session.auto) }
                if let split = model.splits.first(where: { $0.contains(session.id) }) {
                    Image(systemName: split.vertical ? "rectangle.split.1x2" : "rectangle.split.2x1")
                        .font(.caption).foregroundStyle(.tertiary)
                        .help("In a split with \(model.sessions.first { $0.id == split.other(session.id) }?.name ?? "another session")")
                }
                Spacer()
                if hovering, model.canArchive(session.id) {
                    // Where Claude desktop has it: on the row, under the pointer.
                    Button { model.archive(session.id) } label: { Image(systemName: "archivebox") }
                        .buttonStyle(.plain)
                        .foregroundStyle(.secondary)
                        .help("Archive: stop it and keep it under Archived, to pick up again (⇧⌘A)")
                } else if status == .waiting, let on = session.waitingOn {
                    Text("waiting on \(on)").font(.caption).foregroundStyle(status.color).lineLimit(1)
                        .help("Its turn ended while these still run; it carries on when they finish")
                } else if status == .idle || status == .done, let ports = session.serving {
                    Text("serving :\(ports.replacingOccurrences(of: ", ", with: " :"))").font(.caption).foregroundStyle(SessionStatus.working.color).lineLimit(1)
                        .help("Its turn is over; a server it started keeps running. Stop it from the session's menu.")
                } else {
                    Text(status.label).font(.caption).foregroundStyle(status.color)
                }
            }
            if let f = session.inside {
                HStack(spacing: 5) {
                    AgentBadge(agent: f.agent)
                    Text(f.title).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                }
                .help("\(f.agentName) started by hand in this shell")
            } else if let here = session.shell_cwd {
                HStack(spacing: 5) {
                    Text(NSString(string: here).abbreviatingWithTildeInPath).lineLimit(1).truncationMode(.head)
                    if let code = session.last_exit, code != 0 {
                        Text("exit \(code)").foregroundStyle(SessionStatus.exited.color)
                    }
                }
                .font(.caption.monospaced())
                .foregroundStyle(.secondary)
                .help("Where this shell is now, and how its last command ended")
            }
            if let needs = session.needs {
                Label(needs, systemImage: "exclamationmark.triangle.fill")
                    .font(.caption).foregroundStyle(SessionStatus.needsYou.color).lineLimit(1)
            }
            if let error = session.error {
                ErrorLine(message: error)
            }
            PeerChips(session: session)
            if let stat, stat.files > 0 {
                StatText(stat: stat).font(.caption.monospacedDigit())
            }
            if session.requests > 0 {
                HStack(spacing: 6) {
                    if let tier = session.tier {
                        Text("\(tier) → \(shortModel(session.last_model))").foregroundStyle(SessionStatus.done.color)
                    } else {
                        Text(shortModel(session.last_model))
                    }
                    Spacer()
                    if let ctx = session.contextUse {
                        ContextRing(used: ctx.used, limit: ctx.limit, size: 10)
                    }
                    Text("↑\(tokens(session.input_tokens)) ↓\(tokens(session.output_tokens))")
                }
                .font(.caption.monospacedDigit())
                .foregroundStyle(.secondary)
                .lineLimit(1)
            }
        }
        .padding(.vertical, 3)
        .onHover { hovering = $0 }
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

struct UsagePanel: View {
    @EnvironmentObject var model: DinoModel

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if model.power?.holding == true {
                Label("Awake with the lid closed", systemImage: "laptopcomputer")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .help("dino turned system sleep off while agents work; it comes back when they're done (Settings → General)")
            }
            Text("USAGE").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
            let labels = ["anthropic": "Claude", "chatgpt": "Codex"]
            ForEach(model.quotas, id: \.provider) { q in
                ForEach(q.windows.filter { $0.name.hasSuffix("h") || $0.name.hasSuffix("d") }, id: \.name) { w in
                    QuotaBar(label: "\(labels[q.provider] ?? q.provider) \(w.name)", window: w)
                }
            }
            if model.quotas.isEmpty {
                Text("No quota data yet").font(.caption).foregroundStyle(.tertiary)
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
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.bar)
    }
}

struct QuotaBar: View {
    let label: String
    let window: WindowInfo

    var body: some View {
        let pct = Double(window.utilization)
        let color: Color = pct < 0.6 ? Brand.green : pct < 0.85 ? SessionStatus.needsYou.color : SessionStatus.exited.color
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
        Text(["codex", "qwen", "kimi", "pi", "hermes"].contains(base) ? base : "claude")
            .font(.system(size: 9, weight: .semibold, design: .monospaced))
            .padding(.horizontal, 4).padding(.vertical, 1)
            .background(RoundedRectangle(cornerRadius: 3).fill(color.opacity(0.18)))
            .foregroundStyle(color)
    }

    /// Free-tier ones ("kimi-free") as their agent.
    private var base: String { agent.hasSuffix("-free") ? String(agent.dropLast(5)) : agent }

    private var color: Color {
        switch base {
        case "codex": .blue
        case "qwen": .purple
        case "kimi": .teal
        case "pi": .mint
        case "hermes": .indigo
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
        Button("Continue as a \(found.agentName) session") { model.takeOver(session) }
            .controlSize(.small)
            .help("Restart \(found.agentName) under dino with this conversation, once its turn is over: status, tasks, controls and previews then work. The shell goes.")
    }
}

/// A session running in another terminal, with a one-click handoff.
struct ElsewhereRow: View {
    @EnvironmentObject var model: DinoModel
    let session: FoundSession

    var body: some View {
        HStack(spacing: 8) {
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 5) {
                    AgentBadge(agent: session.agent)
                    Text(session.title).lineLimit(1)
                }
                Text(whereText(session)).font(.caption).foregroundStyle(.secondary).lineLimit(1)
            }
            Spacer()
            Image(systemName: "arrow.right.circle").foregroundStyle(Brand.green)
                .help("Click to move this session into dino")
        }
        .padding(.vertical, 2)
        .contentShape(Rectangle())
        .contextMenu { Button("Move to dino…") { model.confirmMove = session } }
    }
}

func whereText(_ f: FoundSession) -> String {
    var parts: [String] = []
    if let t = f.terminal { parts.append("in \(t)") }
    if let s = f.status { parts.append(s) }
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

struct MovingOverlay: View {
    let session: FoundSession

    var body: some View {
        ZStack {
            Color.black.opacity(0.35).ignoresSafeArea()
            VStack(spacing: 12) {
                ProgressView().controlSize(.large)
                Text("Moving “\(session.title)” into dino").font(.headline)
                if session.source == "running" {
                    Text(session.isBusy
                        ? "Waiting for its current turn to finish, then it continues here."
                        : session.terminal == "dino"
                        ? "Restarting it under dino, in the same row."
                        : "Closing it in \(session.terminal ?? "the other terminal") and continuing here.")
                        .foregroundStyle(.secondary)
                }
            }
            .padding(28)
            .background(RoundedRectangle(cornerRadius: 14).fill(.regularMaterial))
        }
    }
}
