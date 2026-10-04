import AppKit
import GhosttyTerminal
import SwiftUI

/// Two sessions in one view: an agent and a shell next to it, or two agents. The pair belongs to
/// neither: selecting either session shows both, with that one focused.
struct Split: Codable, Equatable {
    var first: String
    var second: String
    /// Stacked rather than side by side.
    var vertical = false
    /// How much of the view `first` takes.
    var fraction = 0.5
    /// A shell dino started for this split (⌘D): it goes when its pane closes.
    var helper: String?

    func contains(_ id: String?) -> Bool { first == id || second == id }
    func other(_ id: String) -> String { first == id ? second : first }

    private static let key = "splits"

    /// Splits outlive the app, like the sessions in them.
    static func saved() -> [Split] {
        guard let data = UserDefaults.standard.data(forKey: key) else { return [] }
        return (try? JSONDecoder().decode([Split].self, from: data)) ?? []
    }

    static func save(_ splits: [Split]) {
        UserDefaults.standard.set(try? JSONEncoder().encode(splits), forKey: key)
    }
}

extension DinoModel {
    /// The split on screen: the selected session's, once both of its sessions are running.
    var shownSplit: Split? {
        guard let s = splits.first(where: { $0.contains(selected) }),
              [s.first, s.second].allSatisfy({ id in sessions.contains { $0.id == id } })
        else { return nil }
        return s
    }

    /// The session you're looking at, if it's a session (not a folder or a fan-out).
    var selectedSession: SessionInfo? { sessions.first { $0.id == selected } }

    /// ⌘D: a shell in the selected session's folder, next to it.
    func splitWithShell(vertical: Bool) {
        guard let s = selectedSession else { return }
        let cwd = s.here ?? folder.path
        // A shell beside a session on an SSH host runs on that host too.
        var request: [String: Any] = ["type": "new", "launcher": "shell", "args": [String](), "cwd": cwd, "cols": 120, "rows": 40]
        if let host = s.host { request["host"] = host }
        Task {
            do {
                guard let conn = connection else { return }
                let resp = try await Task.detached {
                    try conn.request(request)
                }.value
                guard let id = resp.id else { return }
                awaited.insert(id)
                pair(s.id, id, vertical: vertical, helper: id)
                pendingSelect = id
            } catch {
                self.error = error.localizedDescription
            }
        }
    }

    /// Show `id` next to the selected session.
    func openBeside(_ id: String, vertical: Bool = false) {
        guard let s = selectedSession, s.id != id else { return }
        pair(s.id, id, vertical: vertical, helper: nil)
        select(id)
    }

    /// Sessions a shell's AI line just handed its request to (⌘⏎, `dino ai agent`): each goes
    /// under that shell when you're looking at it. Called with the sessions dinod reports before
    /// they replace `sessions`.
    func placeHandedOff(_ next: [SessionInfo]) {
        for s in next where !sessions.contains(where: { $0.id == s.id }) {
            guard let by = s.started_by, by == selected,
                  sessions.first(where: { $0.id == by })?.agent_id == "shell",
                  !splits.contains(where: { $0.contains(by) })
            else { continue }
            pair(by, s.id, vertical: true, helper: nil)
            pendingSelect = s.id
        }
    }

    /// The shell whose terminal has the keyboard, on this Mac, with no agent running in it: the
    /// one the AI line's keys go to.
    var shellAtPrompt: String? {
        guard let id = focusedTerminal, NSApp.keyWindow?.firstResponder is LinkTerminalView,
              let s = sessions.first(where: { $0.id == id }),
              s.agent_id == "shell", !s.exited, s.inside == nil, s.host == nil
        else { return nil }
        return id
    }

    /// Keys for a session's terminal, as if typed.
    func sendKeys(_ id: String, _ text: String) {
        guard let conn = connection else { return }
        Task.detached { try? conn.sendKeys(session: id, text: text) }
    }

    /// A session is in one split at most: pairing it again ends its old pair.
    private func pair(_ a: String, _ b: String, vertical: Bool, helper: String?) {
        splits.removeAll { $0.contains(a) || $0.contains(b) }
        splits.append(Split(first: a, second: b, vertical: vertical, helper: helper))
    }

    /// ⌘W in a split: the pane goes, its session keeps running unless it was the split's own shell.
    func closePane(_ id: String) {
        guard let s = splits.first(where: { $0.contains(id) }) else { return }
        splits.removeAll { $0 == s }
        if s.helper == id { kill(id) }
        select(s.other(id))
    }

    /// ⌘\ (Claude desktop's key): the pane with focus goes, a split's or else the side pane, never the window.
    func closeFocusedPane() {
        if let split = shownSplit, sidePane == nil || split.contains(focusedTerminal) {
            if let id = selected { closePane(id) }
        } else if sidePane != nil {
            closeSidePane()
        }
    }

    /// ⌃` (Claude desktop's terminal toggle): a shell below the session, or its shell gone again.
    func toggleTerminal() {
        if let shell = shownSplit?.helper { closePane(shell) } else { splitWithShell(vertical: true) }
    }

    func updateSplit(_ s: Split, _ change: (inout Split) -> Void) {
        guard let i = splits.firstIndex(of: s) else { return }
        change(&splits[i])
    }
}

// MARK: - Layout

/// Where each session's surface goes. Every surface stays in one ForEach and only its frame
/// changes, so splitting, unsplitting and switching never recreate a Ghostty surface.
struct PaneLayout {
    static let header: CGFloat = 26
    static let gap: CGFloat = 1

    let split: Split?
    let selected: String?
    let size: CGSize

    /// The whole pane, header included; nil when the session isn't on screen.
    func frame(_ id: String) -> CGRect? {
        guard let split else { return id == selected ? CGRect(origin: .zero, size: size) : nil }
        guard split.contains(id) else { return nil }
        let first = id == split.first
        if split.vertical {
            let h = (size.height - Self.gap) * split.fraction
            return first ? CGRect(x: 0, y: 0, width: size.width, height: h)
                : CGRect(x: 0, y: h + Self.gap, width: size.width, height: size.height - h - Self.gap)
        }
        let w = (size.width - Self.gap) * split.fraction
        return first ? CGRect(x: 0, y: 0, width: w, height: size.height)
            : CGRect(x: w + Self.gap, y: 0, width: size.width - w - Self.gap, height: size.height)
    }

    /// The terminal inside a pane: below the pane's header when split.
    func surface(_ id: String) -> CGRect? {
        guard let f = frame(id) else { return nil }
        guard split != nil else { return f }
        return CGRect(x: f.minX, y: f.minY + Self.header, width: f.width, height: max(f.height - Self.header, 0))
    }

    /// The draggable line between the panes.
    var divider: CGRect? {
        guard let split, let f = frame(split.first) else { return nil }
        return split.vertical ? CGRect(x: 0, y: f.maxY - 3, width: size.width, height: Self.gap + 6)
            : CGRect(x: f.maxX - 3, y: 0, width: Self.gap + 6, height: size.height)
    }
}

extension View {
    /// Place a view at `rect` in a top-leading ZStack.
    func placed(_ rect: CGRect) -> some View {
        frame(width: rect.width, height: rect.height).offset(x: rect.minX, y: rect.minY)
    }
}

// MARK: - Views

/// A split pane's title: which session, how it's doing, and a close button.
struct PaneHeader: View {
    @EnvironmentObject var model: DinoModel
    let session: SessionInfo
    let split: Split
    let focused: Bool

    var body: some View {
        let status = model.status(of: session)
        HStack(spacing: 7) {
            StatusDot(status: status)
            Text(session.display).font(.system(.callout, design: .monospaced).weight(.semibold))
                .foregroundStyle(focused ? Brand.green : .secondary)
            if let f = session.inside { AgentBadge(agent: f.agent) }
            if session.label == nil, let t = session.inside?.title ?? session.title, DinoModel.undecorated(t) != session.display { Text(t).font(.callout).foregroundStyle(.secondary).lineLimit(1) }
            Spacer(minLength: 4)
            if let f = session.inside, f.continuable { TakeOverButton(session: session, found: f) }
            Text(status.label).font(.caption).foregroundStyle(status.color)
            Button {
                model.updateSplit(split) { $0.vertical.toggle() }
            } label: {
                Image(systemName: split.vertical ? "rectangle.split.2x1" : "rectangle.split.1x2")
            }
            .help(split.vertical ? "Side by side" : "Stacked")
            Button { model.closePane(session.id) } label: { Image(systemName: "xmark") }
                .help(split.helper == session.id ? "Close this shell (⌘W)" : "Close this pane; the session keeps running (⌘W)")
        }
        .buttonStyle(.borderless)
        .padding(.horizontal, 10)
        .frame(maxHeight: .infinity)
        .background(focused ? Color.primary.opacity(0.07) : Color.primary.opacity(0.03))
        .overlay(alignment: .bottom) {
            Rectangle().fill(focused ? Brand.green : Color.clear).frame(height: 2)
        }
        .contentShape(Rectangle())
        .onTapGesture { model.select(session.id) }
    }
}

/// Drag to resize the split.
struct SplitDivider: View {
    @EnvironmentObject var model: DinoModel
    let split: Split
    let size: CGSize
    @State private var start: Double?

    var body: some View {
        ZStack {
            Color.clear.contentShape(Rectangle())
            Rectangle().fill(Color(nsColor: .separatorColor))
                .frame(width: split.vertical ? nil : PaneLayout.gap, height: split.vertical ? PaneLayout.gap : nil)
        }
        .onHover { inside in
            if inside {
                (split.vertical ? NSCursor.resizeUpDown : NSCursor.resizeLeftRight).push()
            } else {
                NSCursor.pop()
            }
        }
        .gesture(DragGesture(minimumDistance: 1, coordinateSpace: .named(Terminals.space)).onChanged { g in
            let length = split.vertical ? size.height : size.width
            let at = split.vertical ? g.location.y : g.location.x
            model.updateSplit(split) { $0.fraction = min(max(Double(at / length), 0.15), 0.85) }
        })
    }
}

/// A session row's context menu.
struct SessionMenu: View {
    @EnvironmentObject var model: DinoModel
    let session: SessionInfo

    var body: some View {
        if let current = model.selectedSession, current.id != session.id {
            Button("Open Beside \(current.name)") { model.openBeside(session.id) }
            Button("Open Below \(current.name)") { model.openBeside(session.id, vertical: true) }
        }
        if model.splits.contains(where: { $0.contains(session.id) }) {
            Button("Close Pane") { model.closePane(session.id) }
        }
        ForEach(session.servers ?? [], id: \.self) { server in
            Button("Stop Server \(server.where_)") { model.stopServer(session.id, server) }
                .help(server.command)
        }
        if session.exited {
            Button(session.agent_id == "shell" ? "Restart" : "Resume") { model.resume(session.id) }
        }
        Divider()
        let pinned = session.pinned == true
        Button(pinned ? "Unpin" : "Pin") { model.pin(session.id, !pinned) }
            .help(pinned ? "Let it sort with the others again" : "Keep it at the top of its group; dino won't archive it on its own")
        if session.agent_id == "shell", session.host == nil {
            let keep = session.keep_terminal == true
            Button(keep ? "Let Agents Here Report to dino" : "Keep as Terminal") { model.keepTerminal(session.id, !keep) }
                .help(keep
                    ? "An agent you start in this shell shows in the sidebar with its turns and questions again"
                    : "An agent you start in this shell stays a plain program: dino doesn't follow its turns")
        }
        Button("Mark as Unread") { model.markUnread(session.id) }
            .disabled(session.exited)
            .help("Show it under Needs you until you look at it again")
        Button("Rename…") { model.renaming = Renaming(id: session.id, place: .sidebar) }
        if model.canArchive(session.id) {
            Button("Archive") { model.archive(session.id) }
                .help("Stop it and keep it in Archived, to pick up again later")
        }
        Button("Close Session", role: .destructive) { model.kill(session.id) }
        Divider()
        Button("Delete…", role: .destructive) { model.confirmDelete(session.id) }
            .help(session.agent_id == "shell" ? "Close it and remove it from dino" : "Stop it, remove it from dino, and remove the worktree dino made for it")
    }
}

/// The Split menu: in the Session menu, and in the toolbar without shortcuts (a toolbar menu
/// answers its shortcuts too, and ⌘D would start two shells).
struct SplitMenuItems: View {
    @EnvironmentObject var model: DinoModel
    var shortcuts = true

    var body: some View {
        let session = model.selectedSession
        Button("Split Right with Shell") { model.splitWithShell(vertical: false) }
            .keyboardShortcut(shortcuts ? KeyboardShortcut("d") : nil)
            .disabled(session == nil)
        Button("Split Down with Shell") { model.splitWithShell(vertical: true) }
            .keyboardShortcut(shortcuts ? KeyboardShortcut("d", modifiers: [.command, .option]) : nil)
            .disabled(session == nil)
        Menu("Open Beside") {
            ForEach(model.sessions.filter { $0.id != session?.id }) { s in
                Button(s.label == nil ? s.title.map { "\(s.name) — \($0)" } ?? s.name : s.display) { model.openBeside(s.id) }
            }
        }
        .disabled(session == nil || model.sessions.count < 2)
        Button(model.shownSplit?.helper == nil ? "Show Terminal" : "Hide Terminal") { model.toggleTerminal() }
            .keyboardShortcut(shortcuts ? KeyboardShortcut("`", modifiers: .control) : nil)
            .disabled(session == nil)
        Button("Close Pane") { model.closeFocusedPane() }
            .keyboardShortcut(shortcuts ? KeyboardShortcut("\\") : nil)
            .disabled(model.shownSplit == nil && model.sidePane == nil)
    }
}

/// ⌘W closes what you're in, innermost first, like a tab in a terminal: the split pane you're
/// typing in, else the side pane, else the session itself, and the window only when no session is
/// open. dino's sessions are its tabs.
struct CloseCommand: View {
    @EnvironmentObject var model: DinoModel

    var body: some View {
        // Menu commands don't always redraw when the model changes (at launch this one still saw no
        // session), so the title may lag; what it closes is worked out when it's pressed.
        Button(model.closeTarget.title) { model.closeCurrent() }
            .keyboardShortcut("w")
    }
}

/// What ⌘W closes right now: the pane, then a side pane, then the tab; the window only when there's
/// no tab.
enum CloseTarget {
    case pane(String), sidePane(SidePane), tab(SessionInfo), window

    var title: String {
        switch self {
        case .pane: "Close Pane"
        case .sidePane(let p): p == .preview ? "Close Preview" : p == .tasks ? "Close Tasks" : "Close File"
        case .tab(let s): s.tmux != nil ? "Detach" : "Close Tab"
        case .window: "Close Window"
        }
    }
}

extension DinoModel {
    var closeTarget: CloseTarget {
        // Another window in front (Settings, the quick terminal's, a sheet's): ⌘W closes that one,
        // never a tab behind it.
        if let key = NSApp.keyWindow, key.identifier?.rawValue.hasPrefix("main") != true { return .window }
        let split = shownSplit
        if split != nil, sidePane == nil || split?.contains(focusedTerminal) == true, let id = selected { return .pane(id) }
        if let p = sidePane { return .sidePane(p) }
        if let s = selectedSession { return .tab(s) }
        return .window
    }

    func closeCurrent() {
        switch closeTarget {
        case .pane(let id): closePane(id)
        case .sidePane: closeSidePane()
        case .tab(let s): closeTab(s)
        case .window: NSApp.keyWindow?.performClose(nil)
        }
    }

    /// ⌘W on a tab. A shell's tab ends the shell, asking first if something is running in it (an
    /// agent typed into it included). An agent's tab only closes: the agent keeps running in the
    /// sidebar, where archiving it is.
    func closeTab(_ s: SessionInfo) {
        let shell = s.agent_id == "shell"
        if shell, !s.exited, s.running == true || s.inside != nil {
            let what = s.inside.map { "\(launcherLabel($0.agent)) is running in it" } ?? "A command is still running in it"
            let alert = NSAlert()
            alert.messageText = "Close \(tabName(s))?"
            alert.informativeText = "\(what), and closing the tab ends it."
            alert.addButton(withTitle: "Close")
            alert.addButton(withTitle: "Cancel")
            guard alert.runModal() == .alertFirstButtonReturn else { return }
        }
        dropTab(s.id, ending: shell)
    }

    /// What a tab is called: a shell by its folder, as in Ghostty, or by the agent run in it.
    func tabName(_ s: SessionInfo) -> String {
        guard s.label == nil, s.agent_id == "shell" else { return s.display }
        if let t = s.tmux { return t.label }
        if let f = s.inside { return f.title.isEmpty ? launcherLabel(f.agent) : f.title }
        return s.here.map { URL(fileURLWithPath: $0).lastPathComponent } ?? s.display
    }
}
