import AppKit
import GhosttyTerminal

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
            let f = LinkForwarder(state: state, onOpen: onOpen)
            forwarder = f
            super.delegate = f
        }
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
    TerminalSurfaceOpenURLDelegate
{
    weak var state: TerminalViewState?
    let onOpen: (String) -> Void

    init(state: TerminalViewState, onOpen: @escaping (String) -> Void) {
        self.state = state
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
    func terminalDidUpdateScrollbar(_ scrollbar: TerminalScrollbar) { state?.terminalDidUpdateScrollbar(scrollbar) }
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
            NSWorkspace.shared.open(url)
        case nil:
            // A path that doesn't exist (yet, or any more), or text that only looked like one.
            NSSound.beep()
        }
    }
}
