import AppKit
import DinoGhostty
import SwiftUI

/// Ghostty's split settings, as the user's config last said.
@MainActor
final class SplitChrome: ObservableObject {
    static let shared = SplitChrome()

    /// `unfocused-split-opacity` when the config doesn't set it: much lighter than Ghostty's 0.7,
    /// which is all that marks the focused split in Ghostty. Here its header is marked in green, so
    /// the rest only sits back a little and stays easy to read.
    static let unfocusedOpacity = 0.9

    /// How far an unfocused pane sits back: 1 − `unfocused-split-opacity` (0.15…1).
    @Published private(set) var dim = 1 - unfocusedOpacity
    /// What it sits back under: `unfocused-split-fill`, else the pane's background.
    @Published private(set) var fill = NSColor.black
    /// `split-divider-color`, else a shade of the background, as Ghostty picks it.
    @Published private(set) var divider = NSColor.separatorColor
    /// `focus-follows-mouse`: the pane under the pointer takes the keyboard.
    private(set) var followsMouse = false
    /// `split-preserve-zoom = navigation`: moving to another pane zooms that one instead.
    private(set) var zoomFollowsNavigation = false
    /// `split-inherit-working-directory`: a new pane starts where the one it splits is.
    private(set) var inheritDirectory = true

    func read(_ c: TerminalController, background: NSColor) {
        let opacity = GhosttyConfig.sets("unfocused-split-opacity")
            ? min(max(c.configNumber("unfocused-split-opacity") ?? 0.7, 0.15), 1) : Self.unfocusedOpacity
        let fill = c.configColor("unfocused-split-fill").map(NSColor.init(ghostty:)) ?? background
        let divider = c.configColor("split-divider-color").map(NSColor.init(ghostty:)) ?? NSColor(name: nil) { look in
            // Ghostty's: a little darker on a light background, much darker on a dark one.
            var bg = background
            look.performAsCurrentDrawingAppearance { bg = background.usingColorSpace(.sRGB) ?? background }
            return bg.darkened(by: bg.isLight ? 0.08 : 0.4)
        }
        if 1 - opacity != dim { dim = 1 - opacity }
        if fill != self.fill { self.fill = fill }
        if divider != self.divider { self.divider = divider }
        followsMouse = c.configFlag("focus-follows-mouse") ?? false
        zoomFollowsNavigation = (c.configBits("split-preserve-zoom") ?? 0) & 1 != 0
        inheritDirectory = c.configFlag("split-inherit-working-directory") ?? true
    }
}

extension NSColor {
    convenience init(ghostty c: (red: UInt8, green: UInt8, blue: UInt8)) {
        self.init(srgbRed: CGFloat(c.red) / 255, green: CGFloat(c.green) / 255, blue: CGFloat(c.blue) / 255, alpha: 1)
    }

    /// Ghostty's `isLightColor`.
    var isLight: Bool {
        guard let c = usingColorSpace(.sRGB) else { return false }
        return 0.299 * c.redComponent + 0.587 * c.greenComponent + 0.114 * c.blueComponent > 0.5
    }

    /// Ghostty's `darken(by:)`: the same hue, `amount` less bright.
    func darkened(by amount: CGFloat) -> NSColor {
        guard let c = usingColorSpace(.sRGB) else { return self }
        var h: CGFloat = 0, s: CGFloat = 0, b: CGFloat = 0, a: CGFloat = 0
        c.getHue(&h, saturation: &s, brightness: &b, alpha: &a)
        return NSColor(hue: h, saturation: s, brightness: min(b * (1 - amount), 1), alpha: a)
    }
}

extension SplitTree.NewDirection {
    init(_ d: TerminalHostAction.SplitDirection) {
        self = switch d {
        case .right: .right
        case .down: .down
        case .left: .left
        case .up: .up
        }
    }
}

extension DinoModel {
    /// The split tree session `id` is in.
    func split(of id: String?) -> SplitTree? {
        guard let id else { return nil }
        return splits.first { $0.contains(id) }
    }

    /// The panes on screen: the selected session's tree, with the sessions dinod lists, while that
    /// still leaves two or more.
    var shownSplit: SplitTree? {
        guard let t = split(of: selected) else { return nil }
        let live = t.panes.filter { id in sessions.contains { $0.id == id } }
        if live.count == t.panes.count { return t }
        return t.pruned { live.contains($0) }
    }

    /// The sessions on screen, the selected one first.
    var shownSessions: [String] {
        let shown = shownSplit?.shownPanes ?? selected.map { [$0] } ?? []
        return shown.filter { $0 == selected } + shown.filter { $0 != selected }
    }

    /// The session you're looking at, if it's a session (not a folder or a run).
    var selectedSession: SessionInfo? { sessions.first { $0.id == selected } }

    /// ⌘D and Ghostty's `new_split`: a shell next to session `at` (the selected one), in its
    /// folder unless `split-inherit-working-directory` is off.
    func splitWithShell(_ direction: SplitTree.NewDirection, at id: String? = nil) {
        guard let s = id.flatMap({ id in sessions.first { $0.id == id } }) ?? selectedSession else { return }
        var request: [String: Any] = ["type": "new", "launcher": "shell", "args": [String](), "cols": 120, "rows": 40]
        if SplitChrome.shared.inheritDirectory {
            request["cwd"] = s.here ?? folder.path
        } else if s.host == nil {
            request["cwd"] = FileManager.default.homeDirectoryForCurrentUser.path
        }
        // A shell beside a session on an SSH host runs on that host too.
        if let host = s.host { request["host"] = host }
        let req = request
        Task {
            do {
                guard let conn = connection else { return }
                let resp = try await Task.detached { try conn.request(req) }.value
                guard let id = resp.id else { return }
                awaited.insert(id)
                insertPane(id, at: s.id, direction, helper: true)
                pendingSelect = id
            } catch {
                self.error = error.localizedDescription
            }
        }
    }

    /// Show `id` next to the selected session.
    func openBeside(_ id: String, vertical: Bool = false) {
        guard let s = selectedSession, s.id != id else { return }
        insertPane(id, at: s.id, vertical ? .down : .right, helper: false)
        select(id)
    }

    /// Sessions a shell's AI line just handed its request to (⌘⏎, `dino ai agent`): each goes
    /// under that shell when you're looking at it. Called with the sessions dinod reports before
    /// they replace `sessions`.
    func placeHandedOff(_ next: [SessionInfo]) {
        for s in next where !sessions.contains(where: { $0.id == s.id }) {
            guard let by = s.started_by, by == selected,
                  sessions.first(where: { $0.id == by })?.agent_id == "shell"
            else { continue }
            insertPane(s.id, at: by, .down, helper: false)
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

    /// Pane `new` next to pane `at`, in `at`'s tree or a new one. A session is in one split at
    /// most: one already in another leaves it.
    private func insertPane(_ new: String, at: String, _ direction: SplitTree.NewDirection, helper: Bool) {
        guard new != at else { return }
        var trees = splits
        if let i = trees.firstIndex(where: { $0.contains(new) }) {
            if let rest = trees[i].removing(new) { trees[i] = rest } else { trees.remove(at: i) }
        }
        if let i = trees.firstIndex(where: { $0.contains(at) }), let t = trees[i].inserting(new, at: at, direction) {
            trees[i] = t
            if helper { trees[i].helpers.append(new) }
        } else {
            let before = direction == .left || direction == .up
            trees.append(SplitTree(before ? new : at, before ? at : new, vertical: direction == .down || direction == .up,
                                   helpers: helper ? [new] : []))
        }
        splits = trees
    }

    /// ⌘\, a pane's ✕ and ⌃`: the pane goes, its session keeps running unless it was a shell dino
    /// started for the split, which asks first when something runs in it.
    func closePane(_ id: String) {
        guard let t = split(of: id) else { return }
        if t.isHelper(id), let shell = sessions.first(where: { $0.id == id }), !confirmEnding([shell], in: "pane") { return }
        dropPane(id)
    }

    /// ⌘W in a split: the pane closes with what's in it, as in Ghostty: a shell ends, an agent is
    /// archived. Asks first (`confirmClosing`).
    func closePaneAndSession(_ id: String) {
        guard split(of: id) != nil, let s = sessions.first(where: { $0.id == id }) else { return }
        guard confirmClosing([s], in: "pane") else { return }
        dropPane(id, ending: true)
    }

    /// The tree without pane `id`, without asking; ⌘Z puts the pane back. `ending` (⌘W): its
    /// session goes too, as a helper shell's always does: a shell ends (⌘Z brings it back for a
    /// while) and an agent is archived (resumed from Archived; ⌘Z can't). Focus goes where Ghostty
    /// sends it: the pane before, or after the first one.
    private func dropPane(_ id: String, ending: Bool = false) {
        guard let i = splits.firstIndex(where: { $0.contains(id) }) else { return }
        let t = splits[i]
        let next = t.afterClosing(id)
        let shell = sessions.first { $0.id == id }?.agent_id == "shell"
        let endsShell = t.isHelper(id) || ending && shell
        let archives = ending && !shell && sessions.contains { $0.id == id }
        let before = layoutBefore()
        if let rest = t.removing(id) { splits[i] = rest } else { splits.remove(at: i) }
        if ending { leaveTabs(id, splitWith: t) }
        if !archives {
            closed(since: before, members: [id], shells: endsShell ? [id] : [], name: "Close Pane") { [weak self] in
                guard let self, self.split(of: id) != nil else { return }
                self.dropPane(id, ending: ending)
            }
        }
        if endsShell { endShell(id) }
        if selected == id, let next { select(next) }
        if archives { archiveNow(id) }
    }

    /// Session `id` leaves the tabs with its pane. Its split's tab stays where it was: when it
    /// stood under `id`, the next of its panes takes that place.
    private func leaveTabs(_ id: String, splitWith t: SplitTree) {
        guard let at = tabs.firstIndex(of: id) else { return }
        var next = tabs
        next.remove(at: at)
        if let heir = next.firstIndex(where: t.contains), heir >= at {
            next.insert(next.remove(at: heir), at: at)
        }
        tabs = next
    }

    /// ⌘\ (Claude desktop's key): the pane with focus goes, a split's or else the side pane, never the window.
    func closeFocusedPane() {
        if let split = shownSplit, sidePane == nil || split.contains(focusedTerminal) {
            if let id = selected { closePane(id) }
        } else if sidePane != nil {
            closeSidePane()
        }
    }

    /// ⌃` (Claude desktop's terminal toggle): a shell below the session, or the split's shell gone
    /// again (the one you're in, else the newest).
    func toggleTerminal() {
        if let t = shownSplit, let shell = t.isHelper(selected ?? "") ? selected : t.helpers.last(where: t.contains) {
            closePane(shell)
        } else {
            splitWithShell(.down)
        }
    }

    /// The tree session `id` is in, changed.
    func updateSplit(of id: String, _ change: (SplitTree) -> SplitTree?) {
        guard let i = splits.firstIndex(where: { $0.contains(id) }), let t = change(splits[i]), t != splits[i] else { return }
        splits[i] = t
    }

    /// Ghostty's `goto_split` from pane `id`: false when there's no pane that way.
    @discardableResult
    func gotoSplit(_ focus: SplitTree.Focus, from id: String? = nil) -> Bool {
        guard let id = id ?? selected, let t = shownSplit, t.contains(id), let next = t.focusTarget(focus, from: id) else { return false }
        // Zoomed: the zoom goes along (`split-preserve-zoom = navigation`) or ends.
        if t.zoomed != nil {
            updateSplit(of: id) { t in
                var t = t
                t.zoomed = SplitChrome.shared.zoomFollowsNavigation ? next : nil
                return t
            }
        }
        select(next)
        return true
    }

    /// Ghostty's `resize_split` from pane `id`: false when no divider runs that way.
    @discardableResult
    func resizeSplit(_ direction: SplitTree.Spatial, by points: Double, from id: String? = nil) -> Bool {
        guard let id = id ?? selected, let t = shownSplit, t == split(of: id),
              let r = t.resizing(id, by: points, direction, in: paneArea) else { return false }
        updateSplit(of: id) { _ in r }
        return true
    }

    /// Ghostty's `equalize_splits`: every pane of the tab `id` is in at its share.
    @discardableResult
    func equalizeSplits(from id: String? = nil) -> Bool {
        guard let id = id ?? selected, split(of: id) != nil else { return false }
        updateSplit(of: id) { $0.equalized() }
        return true
    }

    /// Ghostty's `toggle_split_zoom`: pane `id` takes the whole tab, or gives it back.
    @discardableResult
    func toggleSplitZoom(_ id: String? = nil) -> Bool {
        guard let id = id ?? selected, shownSplit?.contains(id) == true else { return false }
        updateSplit(of: id) { $0.togglingZoom(id) }
        if selected != id { select(id) } else { terminals[id]?.requestFocus() }
        return true
    }

    /// A divider dragged: the branch at `path` of the tree on screen.
    func setRatio(_ ratio: Double, at path: SplitTree.Path) {
        guard let id = selected, let t = shownSplit, t == split(of: id) else { return }
        updateSplit(of: id) { $0.setting(ratio: ratio, at: path) }
    }

    /// `focus-follows-mouse`: the pointer moved over pane `id`'s terminal.
    func pointerEntered(_ id: String) {
        guard SplitChrome.shared.followsMouse, selected != id, !showPalette, shownSplit?.shownPanes.contains(id) == true else { return }
        select(id)
    }
}

// MARK: - Layout

/// Where each session's surface goes. Every surface stays in one ForEach and only its frame
/// changes, so splitting, unsplitting and switching never recreate a Ghostty surface.
struct PaneLayout {
    static let header: CGFloat = 26
    static let gap: CGFloat = 1

    let split: SplitTree?
    let selected: String?
    let size: CGSize
    /// Each pane on screen, header included.
    let frames: [String: CGRect]
    let dividers: [SplitTree.Divider]

    init(split: SplitTree?, selected: String?, size: CGSize) {
        self.split = split
        self.selected = selected
        self.size = size
        if let split {
            (frames, dividers) = split.layout(in: size, gap: Self.gap)
        } else {
            frames = selected.map { [$0: CGRect(origin: .zero, size: size)] } ?? [:]
            dividers = []
        }
    }

    /// The whole pane, header included; nil when the session isn't on screen.
    func frame(_ id: String) -> CGRect? { frames[id] }

    /// The terminal inside a pane: below the pane's header when split.
    func surface(_ id: String) -> CGRect? {
        guard let f = frame(id) else { return nil }
        guard split != nil else { return f }
        return CGRect(x: f.minX, y: f.minY + Self.header, width: f.width, height: max(f.height - Self.header, 0))
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
    /// The session as it is now: its title and how it's doing change here, redrawing this header.
    @ObservedObject private var live: LiveSession
    let split: SplitTree
    let focused: Bool

    init(session: SessionInfo, split: SplitTree, focused: Bool) {
        _live = ObservedObject(wrappedValue: LiveSessions.of(session))
        self.split = split
        self.focused = focused
    }

    private var session: SessionInfo { live.info }

    var body: some View {
        let status = model.status(of: session)
        let zoomed = split.zoomed == session.id
        HStack(spacing: 7) {
            StatusDot(status: status)
            Text(session.display).font(.system(.callout, design: .monospaced).weight(.semibold))
                .foregroundStyle(focused ? Brand.green : .secondary)
            if session.label == nil, let t = session.title, DinoModel.undecorated(t) != session.display { Text(t).font(.callout).foregroundStyle(.secondary).lineLimit(1) }
            Spacer(minLength: 4)
            Text(status.label).font(.caption).foregroundStyle(status.color)
            if zoomed {
                Text(split.panes.count == 2 ? "1 more pane" : "\(split.panes.count - 1) more panes").font(.caption).foregroundStyle(.secondary)
            } else {
                let stacked = split.stacked(session.id) ?? false
                Button {
                    model.updateSplit(of: session.id) { $0.turning(session.id) }
                } label: {
                    Image(systemName: stacked ? "rectangle.split.2x1" : "rectangle.split.1x2")
                }
                .help(stacked ? "Side by side" : "Stacked")
                .accessibilityLabel(stacked ? "Side by Side" : "Stacked")
            }
            Button { model.toggleSplitZoom(session.id) } label: {
                Image(systemName: zoomed ? "arrow.down.right.and.arrow.up.left" : "arrow.up.left.and.arrow.down.right")
            }
            .help(zoomed ? "Show all panes (⇧⌘↩)" : "Zoom (⇧⌘↩): show only this pane in the tab")
            .accessibilityLabel(zoomed ? "Unzoom Split" : "Zoom Split")
            Button { model.closePane(session.id) } label: { Image(systemName: "xmark") }
                .help(split.isHelper(session.id) ? "Close this shell (⌘W)" : "Close this pane (⌘\\). The session keeps running.")
                .accessibilityLabel("Close Pane")
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

/// The line between a branch's two sides: drag it to resize them, double-click it to give every
/// pane its share again (Ghostty's `equalize_splits`).
struct SplitDivider: View {
    @EnvironmentObject var model: DinoModel
    @ObservedObject var chrome = SplitChrome.shared
    let divider: SplitTree.Divider

    /// Neither side smaller than this while dragging: a pane's header and a line or so.
    private static let least: CGFloat = PaneLayout.header + 14

    var body: some View {
        let d = divider
        ZStack {
            Color.clear.contentShape(Rectangle())
            Rectangle().fill(Color(nsColor: chrome.divider))
                .frame(width: d.vertical ? nil : PaneLayout.gap, height: d.vertical ? PaneLayout.gap : nil)
        }
        .onHover { inside in
            if inside {
                (d.vertical ? NSCursor.resizeUpDown : NSCursor.resizeLeftRight).push()
            } else {
                NSCursor.pop()
            }
        }
        .gesture(DragGesture(minimumDistance: 1, coordinateSpace: .named(Terminals.space)).onChanged { g in
            if let r = d.ratio(at: g.location, least: Self.least, gap: PaneLayout.gap) { model.setRatio(r, at: d.path) }
        })
        .onTapGesture(count: 2) { model.equalizeSplits() }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(d.vertical ? "Vertical split divider" : "Horizontal split divider")
        .accessibilityValue("\(Int((d.ratio * 100).rounded()))%")
        .accessibilityHint(d.vertical ? "Drag to resize the top and bottom panes" : "Drag to resize the left and right panes")
        .accessibilityAdjustableAction { direction in
            switch direction {
            case .increment: model.setRatio(min(d.ratio + 0.025, 0.9), at: d.path)
            case .decrement: model.setRatio(max(d.ratio - 0.025, 0.1), at: d.path)
            @unknown default: break
            }
        }
    }
}

/// A session row's context menu.
struct SessionMenu: View {
    @EnvironmentObject var model: DinoModel
    let session: SessionInfo

    var body: some View {
        UsingMenuItems(session: session)
        if let current = model.selectedSession, current.id != session.id {
            Button("Open Beside \(current.name)") { model.openBeside(session.id) }
            Button("Open Below \(current.name)") { model.openBeside(session.id, vertical: true) }
        }
        if model.split(of: session.id) != nil {
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
            .help(pinned ? "Sort it with the other sessions again" : "Keep it at the top of its group and never archive it automatically")
        if session.agent_id == "shell", session.host == nil {
            let keep = session.keep_terminal == true
            Button(keep ? "Let Agents Here Report to dino" : "Keep as Terminal") { model.keepTerminal(session.id, !keep) }
                .help(keep
                    ? "Agents you start in this shell show in the sidebar again, with their turns and questions"
                    : "Agents you start in this shell run as plain programs, and dino doesn't track their turns")
        }
        Button("Mark as Unread") { model.markUnread(session.id) }
            .disabled(session.exited)
            .help("Show it under Needs you until you look at it again")
        Button("Rename…") { model.renaming = Renaming(id: session.id, place: .sidebar) }
        if session.agent != "shell" {
            Button("Fork Session") { model.forkNow(session) }
                .disabled(!model.canFork(session))
                .help(model.whyNoFork(session) ?? "Fork (⌥⌘B): a new tab with a copy of this conversation, ready for your next prompt. The original doesn't change.")
            Button("Fork with Options…") { model.forking = session }
                .disabled(!model.canFork(session))
                .help(model.whyNoFork(session) ?? "Fork with a name, in a worktree of its own, or with a first prompt (⌃⌥⌘B)")
        }
        if model.canArchive(session.id) {
            Button("Archive") { model.archive(session.id) }
                .help("Stop the session and move it to Archived, where you can resume it later")
        }
        Button("Close Session", role: .destructive) { model.closeSession(session.id) }
        Divider()
        Button("Delete…", role: .destructive) { model.confirmDelete(session.id) }
            .help(session.agent_id == "shell" ? "Close the shell and remove it from dino" : "Stop the agent, remove the session from dino, and delete any worktree dino made for it")
    }
}

/// The Split menu: in the Session menu, and in the toolbar without shortcuts (a toolbar menu
/// answers its shortcuts too, and ⌘D would start two shells). Its keys are Ghostty's own macOS
/// defaults: ⌘D and ⇧⌘D split right and down (handed to these items, see `menuKeys`); for the
/// rest, Ghostty's keybinds (the user's, if rebound) take them first in a pane and come back to
/// dino as the same actions.
struct SplitMenuItems: View {
    @EnvironmentObject var model: DinoModel
    var shortcuts = true

    var body: some View {
        let session = model.selectedSession
        let split = model.shownSplit
        Button("Split Right with Shell") { model.splitWithShell(.right) }
            .keyboardShortcut(shortcuts ? KeyboardShortcut("d") : nil)
            .disabled(session == nil)
        Button("Split Down with Shell") { model.splitWithShell(.down) }
            .keyboardShortcut(shortcuts ? KeyboardShortcut("d", modifiers: [.command, .shift]) : nil)
            .disabled(session == nil)
        Button("Split Left with Shell") { model.splitWithShell(.left) }
            .disabled(session == nil)
        Button("Split Up with Shell") { model.splitWithShell(.up) }
            .disabled(session == nil)
        Menu("Open Beside") {
            ForEach(model.sessions.filter { $0.id != session?.id }) { s in
                // A title changes with every command a shell runs: only its item redraws.
                Live(s) { s in
                    Button(s.label == nil ? s.title.map { "\(s.name) — \($0)" } ?? s.name : s.display) { model.openBeside(s.id) }
                }
            }
        }
        .disabled(session == nil || model.sessions.count < 2)
        Button(split.map { t in t.isHelper(model.selected ?? "") || t.helpers.contains(where: t.contains) } == true ? "Hide Terminal" : "Show Terminal") { model.toggleTerminal() }
            .keyboardShortcut(shortcuts ? KeyboardShortcut("`", modifiers: .control) : nil)
            .disabled(session == nil)
        Button("Close Pane") { model.closeFocusedPane() }
            .keyboardShortcut(shortcuts ? KeyboardShortcut("\\") : nil)
            .disabled(split == nil && model.sidePane == nil)
        Divider()
        Button(split?.zoomed == nil ? "Zoom Split" : "Unzoom Split") { model.toggleSplitZoom() }
            .keyboardShortcut(shortcuts ? KeyboardShortcut(.return, modifiers: [.command, .shift]) : nil)
            .disabled(split == nil)
        Button("Select Previous Split") { model.gotoSplit(.previous) }
            .keyboardShortcut(shortcuts ? KeyboardShortcut("[") : nil)
            .disabled(split == nil)
        Button("Select Next Split") { model.gotoSplit(.next) }
            .keyboardShortcut(shortcuts ? KeyboardShortcut("]") : nil)
            .disabled(split == nil)
        Menu("Select Split") {
            Button("Select Split Above") { model.gotoSplit(.spatial(.up)) }
                .keyboardShortcut(shortcuts ? KeyboardShortcut(.upArrow, modifiers: [.command, .option]) : nil)
            Button("Select Split Below") { model.gotoSplit(.spatial(.down)) }
                .keyboardShortcut(shortcuts ? KeyboardShortcut(.downArrow, modifiers: [.command, .option]) : nil)
            Button("Select Split Left") { model.gotoSplit(.spatial(.left)) }
                .keyboardShortcut(shortcuts ? KeyboardShortcut(.leftArrow, modifiers: [.command, .option]) : nil)
            Button("Select Split Right") { model.gotoSplit(.spatial(.right)) }
                .keyboardShortcut(shortcuts ? KeyboardShortcut(.rightArrow, modifiers: [.command, .option]) : nil)
        }
        .disabled(split == nil)
        Menu("Resize Split") {
            Button("Equalize Splits") { model.equalizeSplits() }
                .keyboardShortcut(shortcuts ? KeyboardShortcut("=", modifiers: [.command, .control]) : nil)
            Divider()
            Button("Move Divider Up") { model.resizeSplit(.up, by: 10) }
                .keyboardShortcut(shortcuts ? KeyboardShortcut(.upArrow, modifiers: [.command, .control]) : nil)
            Button("Move Divider Down") { model.resizeSplit(.down, by: 10) }
                .keyboardShortcut(shortcuts ? KeyboardShortcut(.downArrow, modifiers: [.command, .control]) : nil)
            Button("Move Divider Left") { model.resizeSplit(.left, by: 10) }
                .keyboardShortcut(shortcuts ? KeyboardShortcut(.leftArrow, modifiers: [.command, .control]) : nil)
            Button("Move Divider Right") { model.resizeSplit(.right, by: 10) }
                .keyboardShortcut(shortcuts ? KeyboardShortcut(.rightArrow, modifiers: [.command, .control]) : nil)
        }
        .disabled(split == nil)
    }
}

/// ⌘W closes what you're in, innermost first, like a tab in a terminal: the split pane you're
/// typing in, else the side pane, else the tab, and the window only when no tab is open. What's in
/// a pane or tab closes with it: a shell ends, an agent is archived.
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
        case .pane(let id): closePaneAndSession(id)
        case .sidePane: closeSidePane()
        case .tab(let s): closeTab(s)
        case .window: NSApp.keyWindow?.performClose(nil)
        }
    }

    /// ⌘W on a tab, its ✕ and Ghostty's `close_tab`: the tab closes with every pane in it, as in
    /// Ghostty: a shell ends, an agent is archived. Asks first (`confirmClosing`).
    func closeTab(_ s: SessionInfo) {
        let members = split(of: s.id)?.panes ?? [s.id]
        let inTab = members.compactMap { m in sessions.first { $0.id == m } }
        guard confirmClosing(inTab, in: "tab") else { return }
        dropTab(s.id, archiving: true)
    }

    /// What a tab is called: a shell by its folder, as in Ghostty; one running an agent typed
    /// there as any agent's session is (see `display`).
    func tabName(_ s: SessionInfo) -> String {
        guard s.label == nil, s.agent_id == "shell", s.inside == nil else { return s.display }
        if let t = s.tmux { return t.label }
        return s.here.map { URL(fileURLWithPath: $0).lastPathComponent } ?? s.display
    }
}
