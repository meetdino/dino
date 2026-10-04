import AppKit
import Carbon.HIToolbox
import DinoGhostty
import SwiftUI

/// The quick terminal: one shell that drops down from the top of the screen on a shortcut that
/// works from any app, as Ghostty's does, and goes away again. Its shell lives in dinod like any
/// other, so it keeps its state while hidden and across restarts; it isn't a session in the
/// sidebar.
@MainActor
final class QuickTerminal: NSObject, NSWindowDelegate {
    static let shared = QuickTerminal()

    /// The shortcut, from any app. Carbon's hot keys need no Accessibility permission (Ghostty's
    /// global keybinds use an event tap, which does).
    enum Key: String, CaseIterable, Identifiable {
        case off, commandGrave = "cmd-grave", optionSpace = "opt-space", controlOptionSpace = "ctrl-opt-space"
        static let storageKey = "quickTerminal.key"
        static var current: Key { UserDefaults.standard.string(forKey: storageKey).flatMap(Key.init) ?? .commandGrave }
        var id: String { rawValue }

        var label: String {
            switch self {
            case .off: "Off"
            case .commandGrave: "⌘`"
            case .optionSpace: "⌥Space"
            case .controlOptionSpace: "⌃⌥Space"
            }
        }

        /// Carbon's key code and modifiers.
        var carbon: (UInt32, UInt32)? {
            switch self {
            case .off: nil
            case .commandGrave: (UInt32(kVK_ANSI_Grave), UInt32(cmdKey))
            case .optionSpace: (UInt32(kVK_Space), UInt32(optionKey))
            case .controlOptionSpace: (UInt32(kVK_Space), UInt32(controlKey | optionKey))
            }
        }
    }

    static let autohideKey = "quickTerminal.autohide"
    /// Hide it when you click into another window, as Ghostty does by default.
    static var autohide: Bool { UserDefaults.standard.object(forKey: autohideKey) as? Bool ?? true }

    weak var model: DinoModel?
    /// Its shell's session, kept per dinod.
    var sessionID: String? {
        get { UserDefaults.standard.string(forKey: "quickTerminal.session.\(DinoEnvironment.home)") }
        set { UserDefaults.standard.set(newValue, forKey: "quickTerminal.session.\(DinoEnvironment.home)") }
    }
    /// Its shell is still running, as of the last poll.
    var sessionAlive = false
    /// Whether the shortcut could be claimed; another app may hold it.
    private(set) var registered = false

    private var panel: QuickPanel?
    private var state: TerminalViewState?
    /// Its terminal, once shown: Ghostty's actions from it aren't a tab's.
    var surfaceState: TerminalViewState? { state }
    private var hotKey: EventHotKeyRef?
    private var handler: EventHandlerRef?
    /// Its shell is being started.
    private var starting = false
    /// The app that was in front, to go back to when it hides.
    private var previous: NSRunningApplication?
    var isShown: Bool { panel?.isVisible ?? false }

    /// Claim the shortcut in Settings (again, after it changes).
    func registerKey() {
        if let hotKey { UnregisterEventHotKey(hotKey) }
        hotKey = nil
        registered = false
        guard let (code, mods) = Key.current.carbon else { return }
        if handler == nil {
            var spec = EventTypeSpec(eventClass: OSType(kEventClassKeyboard), eventKind: UInt32(kEventHotKeyPressed))
            InstallEventHandler(GetApplicationEventTarget(), { _, _, _ in
                MainActor.assumeIsolated { QuickTerminal.shared.toggle() }
                return noErr
            }, 1, &spec, nil, &handler)
        }
        // 'dino'
        let id = EventHotKeyID(signature: OSType(0x6469_6E6F), id: 1)
        registered = RegisterEventHotKey(code, mods, id, GetApplicationEventTarget(), 0, &hotKey) == noErr
    }

    func toggle() {
        isShown ? hide() : show()
    }

    func show() {
        guard let model, let conn = model.connection, !starting else { return }
        if !sessionAlive || sessionID == nil {
            // First use, or its shell ended: a new one in the home folder. Asked off the main
            // thread, like every other dinod call; it shows once dinod answers.
            starting = true
            let home = NSHomeDirectory()
            Task.detached {
                let body: [String: Any] = ["type": "new", "launcher": "shell", "args": [], "cwd": home, "cols": 120, "rows": 30]
                let id = try? conn.request(body).id
                if let id { try? conn.rename(id, to: "Quick terminal") }
                await MainActor.run {
                    self.starting = false
                    guard let id else { return }
                    self.sessionID = id
                    self.sessionAlive = true
                    self.state = nil
                    self.present(id)
                }
            }
            return
        }
        guard let id = sessionID else { return }
        present(id)
    }

    /// Its shell exists: drop the panel down with it.
    private func present(_ id: String) {
        let front = NSWorkspace.shared.frontmostApplication
        previous = front?.processIdentifier == ProcessInfo.processInfo.processIdentifier ? nil : front
        let panel = panel ?? makePanel()
        if state == nil {
            let state = Self.surface(for: id)
            self.state = state
            panel.contentView = NSHostingView(rootView: QuickTerminalView(state: state))
        }
        // The screen you're working on: the one with the mouse.
        let screen = NSScreen.screens.first { NSMouseInRect(NSEvent.mouseLocation, $0.frame, false) } ?? NSScreen.main
        guard let area = screen?.visibleFrame else { return }
        let shown = NSRect(x: area.minX, y: area.maxY - (area.height * 0.45).rounded(), width: area.width, height: (area.height * 0.45).rounded())
        panel.setFrame(shown.offsetBy(dx: 0, dy: shown.height), display: false)
        panel.makeKeyAndOrderFront(nil)
        NSAnimationContext.runAnimationGroup { ctx in
            ctx.duration = NSWorkspace.shared.accessibilityDisplayShouldReduceMotion ? 0 : 0.18
            ctx.timingFunction = CAMediaTimingFunction(name: .easeOut)
            panel.animator().setFrame(shown, display: true)
        }
        state?.requestFocus()
    }

    /// `restoringFocus`: back to the app that was in front before it showed. Not when it hides
    /// because you clicked elsewhere: you've already gone where you meant to.
    func hide(restoringFocus: Bool = true) {
        guard let panel, panel.isVisible else { return }
        let gone = panel.frame.offsetBy(dx: 0, dy: panel.frame.height)
        NSAnimationContext.runAnimationGroup { ctx in
            ctx.duration = NSWorkspace.shared.accessibilityDisplayShouldReduceMotion ? 0 : 0.15
            ctx.timingFunction = CAMediaTimingFunction(name: .easeIn)
            panel.animator().setFrame(gone, display: true)
        } completionHandler: {
            MainActor.assumeIsolated {
                panel.orderOut(nil)
                if restoringFocus { self.previous?.activate() }
                self.previous = nil
            }
        }
    }

    private func makePanel() -> QuickPanel {
        let p = QuickPanel(contentRect: .zero, styleMask: [.nonactivatingPanel, .borderless], backing: .buffered, defer: false)
        p.level = .floating
        p.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
        p.isFloatingPanel = true
        p.hidesOnDeactivate = false
        p.hasShadow = true
        p.backgroundColor = .windowBackgroundColor
        p.delegate = self
        panel = p
        return p
    }

    nonisolated func windowDidResignKey(_: Notification) {
        MainActor.assumeIsolated {
            if Self.autohide { hide(restoringFocus: false) }
        }
    }

    /// Keys typed in it, before its terminal: the AI line's ⌘I and ⌘⏎ go to its shell, and the
    /// menu's session shortcuts (⌘W, ⌘D, ⌘1…) don't reach the main window from here. True if taken.
    func key(_ e: NSEvent) -> Bool {
        let mods = e.modifierFlags.intersection([.command, .shift, .option, .control])
        guard mods.contains(.command), let id = sessionID else { return false }
        switch (mods, e.charactersIgnoringModifiers ?? "") {
        case ([.command], "i"):
            model?.sendKeys(id, "\u{1b}[57300~")
        case ([.command], "\r"):
            model?.sendKeys(id, "\u{1b}[57301~")
        case (_, let c) where "cvaxz=+-0q".contains(c) && !c.isEmpty:
            return false
        default:
            break
        }
        return true
    }

    /// A terminal attached to session `id`, as the main window's are.
    static func surface(for id: String) -> TerminalViewState {
        let t = TerminalViewState(controller: DinoModel.terminals)
        t.configuration = TerminalSurfaceOptions(
            backend: .exec,
            envVars: ["PATH": DinoEnvironment.loginPath, "DINO_HOME": DinoEnvironment.home],
            command: "\(DinoEnvironment.dinoBinary) attach --fresh \(id)",
            waitAfterCommand: false
        )
        ClipboardConfirmation.install(on: t, session: id)
        t.makePlatformView = { [weak t] in
            // No file pane here: files are revealed in the Finder, never opened (a `.command` would run).
            LinkTerminalView(local: { true }) { link in
                LinkPolicy.open(link, cwd: t?.workingDirectory ?? NSHomeDirectory())
            }
        }
        return t
    }
}

/// A borderless panel still takes the keyboard.
final class QuickPanel: NSPanel {
    override var canBecomeKey: Bool { true }
}

private struct QuickTerminalView: View {
    @ObservedObject var state: TerminalViewState

    var body: some View {
        TerminalSurfaceView(context: state)
            .onAppear { state.isSurfaceVisible = true }
            .overlay(alignment: .bottom) { Divider() }
    }
}
