import AppKit
import DinoGhostty
import SwiftUI

/// A session's terminal whose ⌘-clicked links open in dino: files in the file pane, local web
/// pages in the preview. Any agent works: Ghostty finds the links (paths, URLs, OSC 8).
/// Files and images dropped on it are pasted as paths (see `TerminalDrop`).
final class LinkTerminalView: TerminalView {
    private let onOpen: (String) -> Void
    /// Whether the session runs on this Mac: a dropped path means nothing on an SSH host.
    private let local: () -> Bool
    /// What Ghostty calls: the state's callbacks plus link clicks. The view holds it; Ghostty's
    /// reference is weak.
    private var forwarder: LinkForwarder?
    private let highlight = DropHighlight()

    init(local: @escaping () -> Bool, onOpen: @escaping (String) -> Void) {
        self.local = local
        self.onOpen = onOpen
        super.init(frame: .zero)
        highlight.frame = bounds
        addSubview(highlight)
        registerForDraggedTypes(TerminalDrop.types)
        appearance = GhosttyConfig.paneAppearance.flatMap(NSAppearance.init(named:))
        Self.all.add(self)
    }

    /// Every pane's view, for a change of Ghostty's `window-theme`.
    private static let all = NSHashTable<LinkTerminalView>.weakObjects()

    /// Ghostty's `window-theme` (see `GhosttyConfig.paneAppearance`) on every pane: its theme's
    /// light or dark half, and what it tells programs that ask, follow the pane's look.
    static func paneAppearanceChanged() {
        let look = GhosttyConfig.paneAppearance.flatMap(NSAppearance.init(named:))
        for view in all.allObjects where view.appearance?.name != look?.name {
            view.appearance = look
        }
    }

    override func draggingEntered(_ sender: any NSDraggingInfo) -> NSDragOperation {
        guard local(), TerminalDrop.accepts(sender.draggingPasteboard) else { return [] }
        highlight.isHidden = false
        return .copy
    }

    override func draggingUpdated(_: any NSDraggingInfo) -> NSDragOperation {
        highlight.isHidden ? [] : .copy
    }

    override func draggingExited(_: (any NSDraggingInfo)?) {
        highlight.isHidden = true
    }

    override func draggingEnded(_: any NSDraggingInfo) {
        highlight.isHidden = true
    }

    override func performDragOperation(_ sender: any NSDraggingInfo) -> Bool {
        highlight.isHidden = true
        guard local() else { return false }
        TerminalDrop.text(from: sender.draggingPasteboard) { [weak self] text in
            guard let self else { return }
            paste(text: text)
            // A drop usually starts a prompt: typing carries on where it landed.
            acquireProgrammaticFocus()
        }
        return true
    }

    /// ⌘V when the clipboard holds only an image: its staged path goes in through the paste path.
    /// False to let the terminal paste as usual.
    func pasteClipboardImage() -> Bool {
        guard local(), let path = TerminalDrop.clipboardImage() else { return false }
        paste(text: path)
        return true
    }

    @available(*, unavailable)
    required init?(coder _: NSCoder) { fatalError() }

    /// Reads back as the state: the package looks for it here (focus replay, color scheme).
    override var delegate: (any TerminalSurfaceViewDelegate)? {
        get { forwarder?.state }
        set {
            guard let state = newValue as? TerminalViewState else {
                super.delegate = newValue
                return
            }
            let f = LinkForwarder(state: state, view: self, onOpen: onOpen)
            forwarder = f
            super.delegate = f
        }
    }

    /// The pane's terminal state, for Ghostty's binding actions.
    var terminalState: TerminalViewState? { forwarder?.state }

    /// Typing in the pane has seen its bell (`bell-features` title and border), as in Ghostty.
    override func keyDown(with event: NSEvent) {
        PaneSignals.typed(in: forwarder?.state)
        super.keyDown(with: event)
    }

    /// The keyboard back in the terminal, from the find bar.
    func focusTerminal() {
        window?.makeFirstResponder(self)
    }

    // MARK: Find (Ghostty's start_search, search_total, search_selected, end_search)

    private(set) var findBar: FindBar?

    /// Opens the find bar, or puts the keyboard back in it; `needle` (⌘E's selection) replaces
    /// what it searches for. A new bar starts with the Mac's find text.
    func startSearch(_ needle: String?) {
        if let findBar {
            if let needle { findBar.state.needle = needle }
            findBar.focusField()
            return
        }
        let bar = FindBar(terminal: self, needle: needle ?? FindBar.findText ?? "")
        findBar = bar
        addSubview(bar)
    }

    /// Closes the find bar; `tellGhostty` when the bar closed it, rather than Ghostty (Esc in the
    /// terminal, `end_search`). The keyboard goes back to the terminal if it was in the bar.
    func endSearch(tellGhostty: Bool) {
        guard let bar = findBar else { return }
        findBar = nil
        let hadKeyboard = (window?.firstResponder as? NSView)?.isDescendant(of: bar) == true
        bar.removeFromSuperview()
        if tellGhostty { terminalState?.performBindingAction("end_search") }
        if hadKeyboard { focusTerminal() }
    }

    /// The terminal's own key handling doesn't look at its subviews: the find bar's keys first.
    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        if let findBar, findBar.performKeyEquivalent(with: event) { return true }
        return super.performKeyEquivalent(with: event)
    }

    func searchTotal(_ total: Int?) {
        if let state = findBar?.state, state.total != total { state.total = total }
    }

    func searchSelected(_ selected: Int?) {
        if let state = findBar?.state, state.selected != selected { state.selected = selected }
    }

    /// Edit › Find, as in Ghostty's menu. Its keys reach Ghostty first while the terminal has
    /// the keyboard (⌘F, ⌘G, ⇧⌘G, ⌘E are Ghostty's keybinds); the menu does the same elsewhere.
    func find(_ command: FindCommand) {
        switch command {
        case .find: terminalState?.performBindingAction("start_search")
        case .next, .previous:
            if let findBar {
                findBar.navigate(next: command == .next)
            } else {
                terminalState?.performBindingAction(command == .next ? "navigate_search:next" : "navigate_search:previous")
            }
        case .hide: endSearch(tellGhostty: true)
        case .useSelection: terminalState?.performBindingAction("search_selection")
        case .jumpToSelection: terminalState?.performBindingAction("scroll_to_selection")
        }
    }

    enum FindCommand { case find, next, previous, hide, useSelection, jumpToSelection }

    /// The pane a menu command is for: the one with the keyboard (or its find bar), else the
    /// selected session's.
    static func current(_ model: DinoModel) -> LinkTerminalView? {
        var responder = NSApp.keyWindow?.firstResponder as? NSView
        while let view = responder {
            if let pane = view as? LinkTerminalView { return pane }
            responder = view.superview
        }
        // From the sidebar, say; not from Settings or another window.
        let shown = model.selected.flatMap { model.terminals[$0]?.attachedPlatformView as? LinkTerminalView }
        guard let shown, NSApp.keyWindow == nil || NSApp.keyWindow === shown.window else { return nil }
        return shown
    }

    // MARK: Clear and reset (Edit menu, context menu)

    /// Ghostty's `clear_screen`, its ⌘K: the screen and scrollback cleared, the prompt kept.
    /// dino's ⌘K is Continue a Session, so it's ⌥⌘K here (Edit › Clear).
    @objc func clearScreen(_: Any?) {
        terminalState?.performBindingAction("clear_screen")
    }

    /// Ghostty's `reset`: the terminal back to its first state, as `reset` in a shell.
    @objc func resetTerminal(_: Any?) {
        terminalState?.performBindingAction("reset")
    }

    // MARK: Scrollbar (Ghostty's scrollbar action)

    private var scroller: PaneScroller?

    /// Ghostty's numbers for the scrollbar, with each change of the scrollback or the view.
    func scrollbarChanged(_ bar: TerminalScrollbar) {
        guard GhosttyConfig.showsScrollbar else {
            scroller?.removeFromSuperview()
            scroller = nil
            return
        }
        if scroller == nil {
            let s = PaneScroller(terminal: self) { [weak self] row in
                self?.terminalState?.performBindingAction("scroll_to_row:\(row)")
            }
            let width = PaneScroller.width
            s.frame = NSRect(x: bounds.width - width, y: 0, width: width, height: bounds.height)
            // Below the find bar and the key indicator.
            addSubview(s, positioned: .below, relativeTo: nil)
            scroller = s
        }
        if scroller?.appearance !== GhosttyConfig.scrollerAppearance { scroller?.appearance = GhosttyConfig.scrollerAppearance }
        scroller?.update(bar)
    }

    /// The pointer moved over this pane while another has the keyboard (Ghostty's
    /// `focus-follows-mouse`, in the key window only).
    var pointerEntered: (() -> Void)?

    override func mouseMoved(with event: NSEvent) {
        super.mouseMoved(with: event)
        if SplitChrome.shared.followsMouse, window?.isKeyWindow == true, terminalState?.isFocused == false {
            pointerEntered?()
        }
        guard let scroller else { return }
        let x = convert(event.locationInWindow, from: nil).x
        if x >= bounds.width - PaneScroller.width { scroller.pointerAtEdge() }
    }

    // MARK: Key sequences and tables (Ghostty's key_sequence, key_table)

    private var keys = KeyState()
    private var keyIndicator: NSHostingView<KeyIndicator>?

    /// A key of a sequence pressed (more to come), or the sequence over (nil).
    func keySequence(_ key: String?) {
        if let key { keys.sequence.append(key) } else { keys.sequence = [] }
        showKeys()
    }

    func keyTable(_ change: TerminalHostAction.KeyTable) {
        switch change {
        case .activate(let name): keys.tables.append(name)
        case .deactivate: _ = keys.tables.popLast()
        case .deactivateAll: keys.tables = []
        }
        showKeys()
    }

    private func showKeys() {
        guard !keys.sequence.isEmpty || !keys.tables.isEmpty else {
            keyIndicator?.removeFromSuperview()
            keyIndicator = nil
            return
        }
        let view = keyIndicator ?? NSHostingView(rootView: KeyIndicator(keys: keys))
        view.rootView = KeyIndicator(keys: keys)
        let size = view.fittingSize
        view.frame = NSRect(x: (bounds.width - size.width) / 2, y: 8, width: size.width, height: size.height)
        view.autoresizingMask = [.minXMargin, .maxXMargin, .maxYMargin]
        if keyIndicator == nil {
            addSubview(view)
            keyIndicator = view
        }
    }

    // MARK: Context menu (right-click-action = context-menu)

    override func contextMenu() -> NSMenu? {
        let menu = NSMenu()
        let state = terminalState
        let selection = state?.surface?.hasSelection() == true ? state?.surface?.readSelection() : nil
        func add(_ title: String, _ symbol: String?, _ action: Selector, enabled: Bool = true) {
            let item = NSMenuItem(title: title, action: enabled ? action : nil, keyEquivalent: "")
            item.target = self
            if let symbol { item.image = NSImage(systemSymbolName: symbol, accessibilityDescription: nil) }
            menu.addItem(item)
        }
        // A link Ghostty selected under the pointer (or a selected path or URL): where ⌘-click goes.
        if let selection, let link = linkTarget(selection) {
            menuLink = selection
            add(link, "arrow.up.forward.square", #selector(openSelectedLink))
            menu.addItem(.separator())
        }
        if selection?.isEmpty == false { add("Copy", "doc.on.doc", #selector(copy(_:))) }
        add("Paste", "doc.on.clipboard", #selector(pasteFromMenu))
        add("Select All", "selection.pin.in.out", #selector(selectAll(_:)))
        menu.addItem(.separator())
        add("Find…", "magnifyingglass", #selector(findFromMenu))
        add("Clear", "eraser", #selector(clearScreen(_:)))
        add("Reset Terminal", "arrow.trianglehead.2.clockwise", #selector(resetTerminal(_:)))
        // The session's own: splitting it, renaming it, asking about it. Not in the quick terminal.
        if let state {
            let right = GhosttyActions.work(.newSplit(.right), from: state)
            let down = GhosttyActions.work(.newSplit(.down), from: state)
            let renames = GhosttyActions.work(.promptTitle(.surface), from: state)
            if right != nil || renames != nil { menu.addItem(.separator()) }
            if right != nil { add("Split Right", "rectangle.righthalf.inset.filled", #selector(splitRight)) }
            if down != nil { add("Split Down", "rectangle.bottomhalf.inset.filled", #selector(splitDown)) }
            if renames != nil { add("Rename…", "pencil.line", #selector(rename)) }
            if GhosttyActions.canAsk(from: state) { add("Ask About This Session…", "questionmark.bubble", #selector(askAbout)) }
        }
        return menu
    }

    private var menuLink: String?

    /// What opening `text` as a link would do, as a menu title; nil when it isn't one.
    private func linkTarget(_ text: String) -> String? {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, !text.contains("\n") else { return nil }
        switch LinkTarget.resolve(text, cwd: terminalState?.workingDirectory) {
        case let .file(path, _):
            // A path on an SSH host means nothing here.
            guard local() else { return nil }
            return "Open \((path as NSString).lastPathComponent)"
        case .web, .elsewhere: return "Open Link"
        case nil: return nil
        }
    }

    @objc private func openSelectedLink() {
        if let menuLink { onOpen(menuLink) }
    }

    /// As ⌘V: an image on its own goes in as its staged path.
    @objc private func pasteFromMenu() {
        guard !pasteClipboardImage() else { return }
        terminalState?.performBindingAction("paste_from_clipboard")
    }

    @objc private func findFromMenu() {
        terminalState?.performBindingAction("start_search")
    }

    @objc private func splitRight() { menuAction(.newSplit(.right)) }
    @objc private func splitDown() { menuAction(.newSplit(.down)) }
    @objc private func rename() { menuAction(.promptTitle(.surface)) }
    @objc private func askAbout() {
        if let state = terminalState { GhosttyActions.ask(from: state) }
    }

    private func menuAction(_ action: TerminalHostAction) {
        guard let state = terminalState, let work = GhosttyActions.work(action, from: state) else { return }
        work()
    }
}

/// The keys of a sequence pressed so far, and the key tables in effect, innermost last.
struct KeyState: Equatable {
    var sequence: [String] = []
    var tables: [String] = []
}

/// Ghostty's key state pill at the bottom of the pane: the key tables in effect and a key
/// sequence waiting for its next key.
struct KeyIndicator: View {
    let keys: KeyState

    var body: some View {
        HStack(spacing: 8) {
            if !keys.tables.isEmpty {
                HStack(spacing: 5) {
                    Image(systemName: "keyboard.badge.ellipsis").font(.system(size: 13)).foregroundStyle(.secondary)
                    ForEach(Array(keys.tables.enumerated()), id: \.offset) { i, table in
                        if i > 0 {
                            Image(systemName: "chevron.right").font(.system(size: 10, weight: .semibold)).foregroundStyle(.tertiary)
                        }
                        Text(verbatim: table).font(.system(size: 13, weight: .medium, design: .rounded))
                    }
                }
            }
            if !keys.tables.isEmpty, !keys.sequence.isEmpty { Divider().frame(height: 14) }
            if !keys.sequence.isEmpty {
                HStack(spacing: 4) {
                    ForEach(Array(keys.sequence.enumerated()), id: \.offset) { _, key in
                        Text(verbatim: key)
                            .font(.system(size: 12, weight: .medium, design: .rounded))
                            .padding(.horizontal, 5)
                            .padding(.vertical, 2)
                            .background(RoundedRectangle(cornerRadius: 4).fill(Color.primary.opacity(0.1)))
                    }
                    Text(verbatim: "…").foregroundStyle(.secondary)
                }
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
        .background {
            Capsule().fill(.regularMaterial)
                .overlay { Capsule().strokeBorder(Color.primary.opacity(0.15), lineWidth: 1) }
                .shadow(color: .black.opacity(0.2), radius: 8, y: 2)
        }
        .padding(10)
        .help(keys.tables.isEmpty
            ? "A key sequence is waiting for its next key"
            : "A key table is in effect: keys are read with its bindings until it's deactivated")
    }
}

@MainActor
private final class LinkForwarder:
    TerminalSurfaceTitleDelegate,
    TerminalSurfaceGridResizeDelegate,
    TerminalSurfaceFocusDelegate,
    TerminalSurfaceCloseDelegate,
    TerminalSurfaceBellDelegate,
    TerminalSurfaceDesktopNotificationDelegate,
    TerminalSurfacePwdDelegate,
    TerminalSurfaceScrollbarDelegate,
    TerminalSurfaceCommandFinishedDelegate,
    TerminalSurfaceLifecycleDelegate,
    TerminalSurfaceTextSelectionRequestDelegate,
    TerminalSurfaceClipboardConfirmationDelegate,
    TerminalSurfaceOpenURLDelegate,
    TerminalSurfaceStateDelegate
{
    weak var state: TerminalViewState?
    weak var view: LinkTerminalView?
    var viewState: TerminalViewState? { state }
    let onOpen: (String) -> Void

    init(state: TerminalViewState, view: LinkTerminalView, onOpen: @escaping (String) -> Void) {
        self.state = state
        self.view = view
        self.onOpen = onOpen
    }

    func terminalDidRequestOpenURL(_ url: String, kind _: TerminalOpenURLKind) { onOpen(url) }

    func terminalDidChangeTitle(_ title: String) { state?.terminalDidChangeTitle(title) }
    func terminalDidResize(_ size: TerminalGridMetrics) { state?.terminalDidResize(size) }
    func terminalDidChangeFocus(_ focused: Bool) { state?.terminalDidChangeFocus(focused) }
    func terminalDidClose(processAlive: Bool) { state?.terminalDidClose(processAlive: processAlive) }
    func terminalDidRingBell() { state?.terminalDidRingBell() }
    func terminalDidRequestDesktopNotification(title: String, body: String) { state?.terminalDidRequestDesktopNotification(title: title, body: body) }
    func terminalDidChangeWorkingDirectory(_ path: String) { state?.terminalDidChangeWorkingDirectory(path) }
    // To the pane's own scroller, not the state: as published state it changed with every line
    // of output, making SwiftUI look at the pane again each time.
    func terminalDidUpdateScrollbar(_ bar: TerminalScrollbar) { view?.scrollbarChanged(bar) }
    func terminalDidFinishCommand(exitCode: Int?, durationNanos: UInt64) { state?.terminalDidFinishCommand(exitCode: exitCode, durationNanos: durationNanos) }
    func terminalDidRequestTextSelection(_ request: TerminalTextSelectionRequest) { state?.terminalDidRequestTextSelection(request) }
    func terminalDidRequestClipboardConfirmation(_ request: TerminalClipboardConfirmationRequest) { state?.terminalDidRequestClipboardConfirmation(request) }
    func terminalDidAttachSurface(_ surface: TerminalSurface) { state?.terminalDidAttachSurface(surface) }
    func terminalDidDetachSurface() { state?.terminalDidDetachSurface() }
}

/// Where a clicked link goes.
enum LinkTarget: Equatable {
    case file(path: String, line: Int?)
    case web(URL)
    case elsewhere(URL)

    /// `link` as Ghostty found it: a URL, or a path (absolute, `~/…`, or relative to `cwd`),
    /// maybe with `:line[:column]` after it. Nil when it names nothing that exists.
    static func resolve(_ link: String, cwd: String?) -> LinkTarget? {
        let link = link.trimmingCharacters(in: .whitespacesAndNewlines)
        // `main.rs:12` parses as a URL with scheme "main.rs": only `scheme://` and mailto count.
        if link.contains("://") || link.lowercased().hasPrefix("mailto:"), let url = URL(string: link), let scheme = url.scheme?.lowercased() {
            switch scheme {
            case "file":
                return file(url.path, line: url.fragment.flatMap(lineFragment), cwd: nil) ?? .elsewhere(url)
            case "http", "https":
                return isLocal(url) ? .web(url) : .elsewhere(url)
            default:
                return .elsewhere(url)
            }
        }
        return file(link, line: nil, cwd: cwd)
    }

    static func isLocal(_ url: URL) -> Bool {
        ["localhost", "127.0.0.1", "0.0.0.0", "::1", "[::1]"].contains(url.host?.lowercased() ?? "")
    }

    /// `#L12` or `#12`, as some tools write a line into a file URL.
    private static func lineFragment(_ f: String) -> Int? {
        Int(f.hasPrefix("L") ? String(f.dropFirst()) : f)
    }

    private static func file(_ raw: String, line: Int?, cwd: String?) -> LinkTarget? {
        // Prose around a path: "(see src/app.ts:12)." and quotes.
        var path = raw.trimmingCharacters(in: CharacterSet(charactersIn: "\"'`()[]<>,;"))
        while path.hasSuffix(".") || path.hasSuffix(":") { path.removeLast() }
        var line = line
        // `path:12` and `path:12:5`; a file named like that is rare enough not to check first.
        let parts = path.split(separator: ":", omittingEmptySubsequences: false)
        if parts.count >= 2, let n = Int(parts[parts.count - 1]) {
            if parts.count >= 3, let l = Int(parts[parts.count - 2]) {
                line = l
                path = parts.dropLast(2).joined(separator: ":")
            } else {
                line = n
                path = parts.dropLast().joined(separator: ":")
            }
        }
        path = (path as NSString).expandingTildeInPath
        if !path.hasPrefix("/") {
            guard let cwd else { return nil }
            path = (cwd as NSString).appendingPathComponent(path)
        }
        path = (path as NSString).standardizingPath
        guard FileManager.default.fileExists(atPath: path) else { return nil }
        return .file(path: path, line: line)
    }
}

extension DinoModel {
    /// A link ⌘-clicked in session `id`'s terminal.
    func openLink(_ link: String, from id: String) {
        let session = sessions.first { $0.id == id }
        switch LinkTarget.resolve(link, cwd: session?.cwd) {
        case .file where session?.host != nil:
            // The path is on the SSH host, not on this Mac.
            NSSound.beep()
        case let .file(path, line):
            var isDir: ObjCBool = false
            if FileManager.default.fileExists(atPath: path, isDirectory: &isDir), isDir.boolValue {
                NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: path)])
            } else {
                openFile(path, line: line, session: id)
            }
        case let .web(url):
            openPreview(session: id, url: url)
        case let .elsewhere(url):
            LinkPolicy.openElsewhere(url)
        case nil:
            // A path that doesn't exist (yet, or any more), or text that only looked like one.
            NSSound.beep()
        }
    }
}

/// Where links from outside dino (terminal output, agent text, pages, PDFs) may go. Web and mail
/// links open as usual; files show in dino or the Finder and are never run (the system runs a
/// `.command` or a script and launches an app, and files from git carry no quarantine); anything
/// else, another app's scheme, opens only once you've seen the whole URL and said so.
@MainActor
enum LinkPolicy {
    static func isWeb(_ url: URL) -> Bool {
        ["http", "https", "mailto"].contains(url.scheme?.lowercased() ?? "")
    }

    /// A link dino has no viewer for here: a file (there or not) is revealed in the Finder.
    static func openElsewhere(_ url: URL) {
        if url.isFileURL {
            reveal(url.path)
        } else if isWeb(url) || confirm(url) {
            NSWorkspace.shared.open(url)
        }
    }

    /// A link as Ghostty or Markdown gives it, where there's no file pane or preview: files are
    /// revealed in the Finder, local pages open in the browser.
    static func open(_ link: String, cwd: String?) {
        switch LinkTarget.resolve(link, cwd: cwd) {
        case let .file(path, _):
            reveal(path)
        case let .web(url), let .elsewhere(url):
            openElsewhere(url)
        case nil:
            NSSound.beep()
        }
    }

    /// A Markdown link's URL as `LinkTarget.resolve` takes it: `[x](src/my%20app.rs)` is a path.
    static func link(_ url: URL) -> String {
        url.scheme == nil ? url.absoluteString.removingPercentEncoding ?? url.absoluteString : url.absoluteString
    }

    /// Selects it in a Finder window; nothing is opened.
    static func reveal(_ path: String) {
        guard FileManager.default.fileExists(atPath: path) else {
            NSSound.beep()
            return
        }
        NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: path)])
    }

    /// Before handing `url` to whichever app claims its scheme: the whole URL, and that app.
    static func confirm(_ url: URL) -> Bool {
        let app = NSWorkspace.shared.urlForApplication(toOpen: url).map { FileManager.default.displayName(atPath: $0.path) }
        let s = url.absoluteString
        let shown = s.count > 2000 ? "\(s.prefix(2000))… (\(s.count) characters)" : s
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = app.map { "Open this link in \($0)?" } ?? "Open this link?"
        alert.informativeText = "\(shown)\n\nA link can make another app act on it. Only open it if you trust where it came from."
        alert.addButton(withTitle: "Open")
        alert.addButton(withTitle: "Cancel")
        NSApp.activate(ignoringOtherApps: true)
        return alert.runModal() == .alertFirstButtonReturn
    }
}
