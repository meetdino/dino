import AppKit
import Carbon.HIToolbox
import DinoGhostty
import SwiftUI

/// The quick terminal: one shell that drops down from the top of the screen on a shortcut that
/// works from any app, as Ghostty's does, and goes away again. Its shell lives in dinod like any
/// other, so it keeps its state while hidden and across restarts; it isn't a session in the
/// sidebar. Where it shows, its size and how it moves follow the user's Ghostty config
/// (`quick-terminal-position`, `-size`, `-screen`, `-animation-duration`, `-space-behavior`,
/// `-autohide`), and a `global:` keybind for `toggle_quick_terminal` works beside dino's own
/// shortcut.
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
    /// Hide it when you click into another window, as Ghostty does by default: the Ghostty
    /// config's `quick-terminal-autohide` when it sets one, else Settings → Terminal's.
    static var autohide: Bool {
        shared.place.autohide ?? UserDefaults.standard.object(forKey: autohideKey) as? Bool ?? true
    }

    /// The Ghostty config's say on where and how it shows.
    struct Placement: Equatable {
        var position = "top"
        var primary: TerminalController.QuickTerminalSize?
        var secondary: TerminalController.QuickTerminalSize?
        var screen = "main"
        var duration = 0.2
        var space = "move"
        /// Set in the Ghostty config; nil leaves it to dino's setting.
        var autohide: Bool?
        /// A `global:` keybind for `toggle_quick_terminal`: Carbon's key code and modifiers.
        var globalKey: (UInt32, UInt32)?

        static func == (a: Placement, b: Placement) -> Bool {
            a.position == b.position && a.primary == b.primary && a.secondary == b.secondary && a.screen == b.screen
                && a.duration == b.duration && a.space == b.space && a.autohide == b.autohide
                && a.globalKey?.0 == b.globalKey?.0 && a.globalKey?.1 == b.globalKey?.1
        }
    }

    private(set) var place = Placement()
    /// Its shell reads a password, as of the last state (Secure Keyboard Entry follows).
    var passwordPrompt = false

    /// Read with the rest of the Ghostty config, each time it's applied.
    func readConfig(_ c: TerminalController) {
        var p = Placement()
        p.position = c.configText("quick-terminal-position") ?? "top"
        (p.primary, p.secondary) = c.configQuickTerminalSize()
        p.screen = c.configText("quick-terminal-screen") ?? "main"
        p.duration = max(c.configNumber("quick-terminal-animation-duration") ?? 0.2, 0)
        p.space = c.configText("quick-terminal-space-behavior") ?? "move"
        p.autohide = GhosttyConfig.sets("quick-terminal-autohide") ? c.configFlag("quick-terminal-autohide") : nil
        p.globalKey = GhosttyConfig.bindsGlobally("toggle_quick_terminal") ? c.configTrigger("toggle_quick_terminal").map { ($0.keyCode, $0.carbonModifiers) } : nil
        guard p != place else { return }
        let keyChanged = p.globalKey?.0 != place.globalKey?.0 || p.globalKey?.1 != place.globalKey?.1
        place = p
        panel?.collectionBehavior = behavior
        if keyChanged { registerGhosttyKey() }
    }

    /// Ghostty's `quick-terminal-space-behavior`: `move` follows you to every Space, `remain` stays
    /// on the one it showed on.
    private var behavior: NSWindow.CollectionBehavior {
        place.space == "remain" ? [.fullScreenAuxiliary, .ignoresCycle] : [.canJoinAllSpaces, .fullScreenAuxiliary, .ignoresCycle]
    }

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
    /// The Ghostty config's `global:` keybind, beside dino's own.
    private var ghosttyHotKey: EventHotKeyRef?
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
        installHandler()
        // 'dino'
        let id = EventHotKeyID(signature: OSType(0x6469_6E6F), id: 1)
        registered = RegisterEventHotKey(code, mods, id, GetApplicationEventTarget(), 0, &hotKey) == noErr
    }

    /// The Ghostty config's `global:…=toggle_quick_terminal`, as a Carbon hot key too: no
    /// Accessibility permission needed, unlike Ghostty's event tap. The same keys as dino's own
    /// shortcut are already taken by it.
    private func registerGhosttyKey() {
        if let ghosttyHotKey { UnregisterEventHotKey(ghosttyHotKey) }
        ghosttyHotKey = nil
        guard let (code, mods) = place.globalKey, (code, mods) != (Key.current.carbon ?? (0, 0)) else { return }
        installHandler()
        let id = EventHotKeyID(signature: OSType(0x6469_6E6F), id: 2)
        if RegisterEventHotKey(code, mods, id, GetApplicationEventTarget(), 0, &ghosttyHotKey) != noErr {
            NSLog("dino: the Ghostty config's toggle_quick_terminal key is taken by another app")
        }
    }

    private func installHandler() {
        guard handler == nil else { return }
        var spec = EventTypeSpec(eventClass: OSType(kEventClassKeyboard), eventKind: UInt32(kEventHotKeyPressed))
        InstallEventHandler(GetApplicationEventTarget(), { _, _, _ in
            MainActor.assumeIsolated { QuickTerminal.shared.toggle() }
            return noErr
        }, 1, &spec, nil, &handler)
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
            panel.contentView = NSHostingView(rootView: QuickTerminalView(state: state, id: id))
        }
        guard let area = screen?.visibleFrame else { return }
        // `remain`: shown again from another Space, it comes to this one.
        if place.space == "remain", !panel.isOnActiveSpace { panel.orderOut(nil) }
        let shown = frame(in: area)
        panel.setFrame(offscreen(shown), display: false)
        panel.alphaValue = place.position == "center" ? 0 : 1
        panel.makeKeyAndOrderFront(nil)
        NSAnimationContext.runAnimationGroup { ctx in
            ctx.duration = duration
            ctx.timingFunction = CAMediaTimingFunction(name: .easeOut)
            panel.animator().setFrame(shown, display: true)
            panel.animator().alphaValue = 1
        }
        state?.requestFocus()
    }

    /// `quick-terminal-animation-duration`; none when the Mac reduces motion.
    private var duration: TimeInterval {
        NSWorkspace.shared.accessibilityDisplayShouldReduceMotion ? 0 : place.duration
    }

    /// `quick-terminal-screen`: the one with the keyboard (`main`), the mouse, or the menu bar.
    private var screen: NSScreen? {
        switch place.screen {
        case "mouse": NSScreen.screens.first { NSMouseInRect(NSEvent.mouseLocation, $0.frame, false) } ?? NSScreen.main
        case "macos-menu-bar": NSScreen.screens.first ?? NSScreen.main
        default: NSScreen.main
        }
    }

    /// Where it shows in `area`, by `quick-terminal-position` and `-size` as Ghostty works them out:
    /// the first size is along its edge's axis (height at the top or bottom, width at the sides, the
    /// longer side in the center), the second across it (the whole screen if not given, but in
    /// the center). Without a size: 45% of the screen (dino's own), half each way in the center.
    func frame(in area: NSRect) -> NSRect {
        func length(_ size: TerminalController.QuickTerminalSize?, of whole: CGFloat, otherwise: CGFloat) -> CGFloat {
            switch size {
            case .percent(let p): (whole * p / 100).rounded()
            case .pixels(let px): min(px, whole)
            case nil: otherwise
            }
        }
        let p = place
        var w: CGFloat, h: CGFloat
        switch p.position {
        case "left", "right":
            w = length(p.primary, of: area.width, otherwise: (area.width * 0.45).rounded())
            h = length(p.secondary, of: area.height, otherwise: area.height)
        case "center":
            if area.width >= area.height {
                h = length(p.primary, of: area.height, otherwise: (area.height * 0.5).rounded())
                w = length(p.secondary, of: area.width, otherwise: (area.width * 0.5).rounded())
            } else {
                w = length(p.primary, of: area.width, otherwise: (area.width * 0.5).rounded())
                h = length(p.secondary, of: area.height, otherwise: (area.height * 0.5).rounded())
            }
        default:
            h = length(p.primary, of: area.height, otherwise: (area.height * 0.45).rounded())
            w = length(p.secondary, of: area.width, otherwise: area.width)
        }
        w = max(min(w, area.width), 200)
        h = max(min(h, area.height), 100)
        let x: CGFloat, y: CGFloat
        switch p.position {
        case "bottom": (x, y) = (area.midX - w / 2, area.minY)
        case "left": (x, y) = (area.minX, area.midY - h / 2)
        case "right": (x, y) = (area.maxX - w, area.midY - h / 2)
        case "center": (x, y) = (area.midX - w / 2, area.midY - h / 2)
        default: (x, y) = (area.midX - w / 2, area.maxY - h)
        }
        return NSRect(x: x.rounded(), y: y.rounded(), width: w, height: h)
    }

    /// Where it slides in from: past its edge (the center fades instead).
    private func offscreen(_ f: NSRect) -> NSRect {
        switch place.position {
        case "bottom": f.offsetBy(dx: 0, dy: -f.height)
        case "left": f.offsetBy(dx: -f.width, dy: 0)
        case "right": f.offsetBy(dx: f.width, dy: 0)
        case "center": f
        default: f.offsetBy(dx: 0, dy: f.height)
        }
    }

    /// `restoringFocus`: back to the app that was in front before it showed. Not when it hides
    /// because you clicked elsewhere: you've already gone where you meant to.
    func hide(restoringFocus: Bool = true) {
        guard let panel, panel.isVisible else { return }
        let gone = offscreen(panel.frame)
        let fades = place.position == "center"
        NSAnimationContext.runAnimationGroup { ctx in
            ctx.duration = duration
            ctx.timingFunction = CAMediaTimingFunction(name: .easeIn)
            panel.animator().setFrame(gone, display: true)
            if fades { panel.animator().alphaValue = 0 }
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
        p.collectionBehavior = behavior
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
    let id: String

    var body: some View {
        TerminalSurfaceView(context: state)
            .onAppear { state.isSurfaceVisible = true }
            .overlay(alignment: .bottom) { Divider() }
            .overlay(alignment: .top) { PaneProgressBar(signal: PaneSignals.of(id)) }
            .overlay { PaneBellBorder(signal: PaneSignals.of(id)) }
            .overlay(alignment: .topTrailing) { SecureInputMark(id: id, focused: state.isFocused) }
            .onChange(of: state.isFocused) { _, f in
                if f { PaneSignals.seen(id) }
                SecureInput.shared.update()
            }
    }
}
