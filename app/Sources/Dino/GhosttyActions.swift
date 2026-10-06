import AppKit
import DinoGhostty

/// Ghostty's own actions, from the user's keybinds or the engine, done the dino way: `new_tab` is a
/// new shell tab, `goto_split` moves between a split's panes, `prompt_surface_title`
/// renames the session. An action dino has no equivalent for yet is logged once and left alone.
@MainActor
enum GhosttyActions {
    static weak var model: DinoModel?

    /// Actions seen without anything to do them, each logged once.
    private static var logged: Set<UInt32> = []

    static func install(on controller: TerminalController) {
        controller.onAction = { handle($0) }
    }

    /// True when dino takes the action. The work itself runs a turn later: these come from inside
    /// Ghostty's key handling, and closing a tab or asking a question there would free or block
    /// the surface the engine is still in.
    static func handle(_ event: TerminalActionEvent) -> Bool {
        // Progress, notifications, a command's end and the bell: what the pane shows of them.
        if event.isPaneSignal { return PaneSignals.handle(event.action, event.state) ?? false }
        // A title, a frame: the wrapper's, and the most frequent by far.
        if event.wrapperHandles { return false }
        let action = event.action
        if let done = pane(action, event.state) { return done }
        guard supports(action) else {
            if logged.insert(event.tag.rawValue).inserted {
                NSLog("dino: Ghostty action \(event.name) isn't supported in dino yet; ignored")
            }
            return false
        }
        // Supported, but nothing to do from here (no split to move in, no tab): Ghostty passes
        // the key on, as it does when its own app can't perform a binding.
        guard let model, let work = perform(action, from: event.state, model: model) else { return false }
        DispatchQueue.main.async { work() }
        return true
    }

    /// What the pane shows over its terminal: the find bar, the pointer hidden while you type, a
    /// key sequence waiting for its next key. Nil for an action that isn't one of those.
    private static func pane(_ action: TerminalHostAction, _ state: TerminalViewState?) -> Bool? {
        let view = state?.attachedPlatformView as? LinkTerminalView
        switch action {
        case .startSearch(let needle):
            guard let view else { return false }
            // A turn later: this comes from inside Ghostty's key handling, and the bar takes the
            // keyboard.
            DispatchQueue.main.async {
                view.startSearch(needle)
                if let needle { FindBar.findText = needle }
            }
            return true
        case .endSearch:
            guard let view else { return false }
            DispatchQueue.main.async { view.endSearch(tellGhostty: false) }
            return true
        case .searchTotal(let total):
            view?.searchTotal(total)
            return view != nil
        case .searchSelected(let selected):
            view?.searchSelected(selected)
            return view != nil
        case .mouseVisibility(let visible):
            NSCursor.setHiddenUntilMouseMoves(!visible)
            return true
        case .keySequence(let key):
            view?.keySequence(key)
            return view != nil
        case .keyTable(let change):
            view?.keyTable(change)
            return view != nil
        default:
            return nil
        }
    }

    /// The inspector, more than one window: not in dino yet. The rest of `.other` are the engine's
    /// notices dino has no use for.
    private static func supports(_ action: TerminalHostAction) -> Bool {
        switch action {
        case .newWindow, .closeAllWindows, .moveTab, .inspector, .presentTerminal,
             .progressReport, .desktopNotification, .commandFinished, .ringBell, .other:
            false
        default:
            true
        }
    }

    /// What dino does for `action` from the surface `state` (nil: app-wide), or nil when there's
    /// nothing to do it to.
    private static func perform(_ action: TerminalHostAction, from state: TerminalViewState?, model: DinoModel) -> (() -> Void)? {
        // The session the keybind was pressed in: the quick terminal's own shell isn't a tab.
        let quick = state != nil && state === QuickTerminal.shared.surfaceState
        let session = quick ? nil : state.flatMap { model.session(showing: $0) } ?? model.selected
        let window = state?.attachedPlatformView?.window ?? NSApp.keyWindow ?? NSApp.mainWindow
        switch action {
        case .quit:
            return { NSApp.terminate(nil) }
        case .newTab:
            guard !quick else { return nil }
            return { model.newShell() }
        case .closeTab(let mode):
            guard let id = session else { return nil }
            return { model.closeTabs(from: id, mode) }
        case .closeWindow:
            guard let window else { return nil }
            return { window.performClose(nil) }
        case .newSplit(let direction):
            guard let id = session else { return nil }
            return {
                if model.selected != id { model.select(id) }
                model.splitWithShell(SplitTree.NewDirection(direction), at: id)
            }
        // The split ones only when there's something to do, so a key bound to one passes on
        // otherwise, as in Ghostty.
        case .gotoSplit(let to):
            let focus: SplitTree.Focus = switch to {
            case .previous: .previous
            case .next: .next
            case .left: .spatial(.left)
            case .right: .spatial(.right)
            case .up: .spatial(.up)
            case .down: .spatial(.down)
            }
            guard let id = session, let t = model.shownSplit, t.contains(id), t.focusTarget(focus, from: id) != nil else { return nil }
            return { model.gotoSplit(focus, from: id) }
        case .resizeSplit(let amount, let direction):
            let way: SplitTree.Spatial = switch direction {
            case .up: .up
            case .down: .down
            case .left: .left
            case .right: .right
            }
            guard let id = session, let t = model.shownSplit, t.resizing(id, by: Double(amount), way, in: model.paneArea) != nil else { return nil }
            return { model.resizeSplit(way, by: Double(amount), from: id) }
        case .equalizeSplits:
            guard let id = session, model.shownSplit?.contains(id) == true else { return nil }
            return { model.equalizeSplits(from: id) }
        case .toggleSplitZoom:
            guard let id = session, model.shownSplit?.contains(id) == true else { return nil }
            return { model.toggleSplitZoom(id) }
        case .gotoTab(let to):
            guard !quick else { return nil }
            let shown = model.shownTabs
            guard !shown.isEmpty else { return nil }
            switch to {
            case .previous: return { model.cycleTabs(by: -1) }
            case .next: return { model.cycleTabs(by: 1) }
            case .last: return { model.select(shown[shown.count - 1]) }
            // Past the last tab: the last, as in Ghostty.
            case .index(let n): return { model.select(shown[min(max(n, 1), shown.count) - 1]) }
            }
        case .toggleFullscreen:
            guard let window, !quick else { return nil }
            return { window.toggleFullScreen(nil) }
        case .toggleMaximize:
            guard let window, !quick else { return nil }
            return { window.zoom(nil) }
        case .toggleQuickTerminal:
            return { QuickTerminal.shared.toggle() }
        case .toggleCommandPalette:
            return { model.showPalette.toggle() }
        case .toggleVisibility:
            return { NSApp.isHidden ? NSApp.unhide(nil) : NSApp.hide(nil) }
        // Soft: the config already loaded, again, so a pane (or the app, for `state` nil) whose
        // light or dark just changed takes that half of a `light:…,dark:…` theme.
        case .reloadConfig(let soft):
            return soft ? { DinoModel.terminals.reapplyConfig(to: state) } : { GhosttyConfig.reload() }
        case .openConfig:
            return { GhosttyConfig.openInEditor() }
        case .promptTitle(let what):
            guard what != .window, let id = session else { return nil }
            return {
                if model.selected != id { model.select(id) }
                model.renaming = Renaming(id: id, place: .tab)
            }
        case .copyTitleToClipboard:
            let title = state.map(\.title).flatMap { $0.isEmpty ? nil : $0 }
                ?? session.flatMap { id in model.sessions.first { $0.id == id } }.map { model.tabName($0) }
            guard let title, !title.isEmpty else { return nil }
            return {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(title, forType: .string)
            }
        case .checkForUpdates:
            guard Updates.shared.available else { return nil }
            return { Updates.shared.checkNow() }
        case .floatWindow(let change):
            guard let window else { return nil }
            return {
                let on = switch change {
                case .on: true
                case .off: false
                case .toggle: window.level == .normal
                }
                window.level = on ? .floating : .normal
            }
        // Ghostty's `toggle_secure_input`: the app menu's Secure Keyboard Entry.
        case .secureInput(let change):
            return {
                let s = SecureInput.shared
                switch change {
                case .on: s.global = true
                case .off: s.global = false
                case .toggle: s.global.toggle()
                }
            }
        // Undo Close Tab or Pane, within `undo-timeout`; nothing to undo passes the key on.
        case .undo:
            guard let manager = model.undoManager, manager.canUndo else { return nil }
            return { manager.undo() }
        case .redo:
            guard let manager = model.undoManager, manager.canRedo else { return nil }
            return { manager.redo() }
        case .newWindow, .closeAllWindows, .moveTab,
             .startSearch, .endSearch, .searchTotal, .searchSelected, .mouseVisibility, .keySequence,
             .keyTable, .inspector, .presentTerminal, .progressReport, .desktopNotification, .commandFinished,
             .ringBell, .other:
            return nil
        }
    }

    /// What `action` would do from the pane `state`, for its context menu: nil when nothing.
    static func work(_ action: TerminalHostAction, from state: TerminalViewState) -> (() -> Void)? {
        guard let model else { return nil }
        return perform(action, from: state, model: model)
    }

    /// Ask About This Session…, from the pane `state`: not the quick terminal's own shell.
    static func canAsk(from state: TerminalViewState) -> Bool {
        state !== QuickTerminal.shared.surfaceState && model?.session(showing: state) != nil
    }

    static func ask(from state: TerminalViewState) {
        guard let model, canAsk(from: state), let id = model.session(showing: state),
              let session = model.sessions.first(where: { $0.id == id }) else { return }
        if model.selected != id { model.select(id) }
        model.askingAbout = session
    }
}

extension DinoModel {
    /// The session whose terminal `state` is, among the tabs' and panes'.
    func session(showing state: TerminalViewState) -> String? {
        terminals.first { $0.value === state }?.key
    }

    /// Ghostty's `close_tab`: the tab `id` is in, the other tabs, or the ones to its right, each as
    /// its close button would (a shell running something asks first).
    func closeTabs(from id: String, _ mode: TerminalHostAction.CloseTabMode) {
        let shown = shownTabs
        guard let tab = tab(of: id), let at = shown.firstIndex(of: tab) else { return }
        let closing: [String] = switch mode {
        case .this: [tab]
        case .others: shown.filter { $0 != tab }
        case .right: Array(shown[(at + 1)...])
        }
        for t in closing {
            guard let s = sessions.first(where: { $0.id == t }) else { continue }
            closeTab(s)
        }
    }
}
