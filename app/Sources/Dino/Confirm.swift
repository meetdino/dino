import AppKit
import DinoGhostty

/// The questions Ghostty asks before something you might not mean: a paste that could run
/// commands, a program reading or writing the clipboard, closing a terminal with a program in it.
/// When to ask is the user's Ghostty config (`clipboard-paste-protection`,
/// `clipboard-paste-bracketed-safe`, `clipboard-read`, `clipboard-write`, `confirm-close-surface`).
@MainActor
enum ClipboardConfirmation {
    /// Asks on `state`'s behalf, for session `id`'s terminal. The engine only asks when the config
    /// says to: an unsafe paste with paste protection on, a read or write set to `ask`.
    static func install(on state: TerminalViewState, session id: String) {
        state.onClipboardConfirmationRequest = { [weak state] request in
            present(request, session: id, window: state?.attachedPlatformView?.window)
        }
    }

    /// The longest text shown; a paste past it says how much more there is.
    private static let shownLimit = 20000

    static func present(_ request: TerminalClipboardConfirmationRequest, session id: String, window: NSWindow?) {
        let model = GhosttyActions.model
        let session = model?.sessions.first { $0.id == id }
        // Who's asking: the program in the foreground now (asked of dinod; the last poll may be a
        // second old), else the tab it's in.
        let tab = session.flatMap { s in model?.tabName(s) }
        let now: ForegroundProcess?? = session?.host == nil ? DinoModel.foregroundNow(id) : nil
        let foreground = now ?? session?.foreground
        let program = session?.inside.flatMap { model?.launcherLabel($0.agent) } ?? foreground?.name
        let who = program.map { "“\($0)”" } ?? "A program"
        let place = tab.map { " in “\($0)”" } ?? ""

        let alert = NSAlert()
        alert.alertStyle = .warning
        let allow: String
        switch request.kind {
        case .paste:
            alert.messageText = "Paste text that may run commands?"
            alert.informativeText = "This text could run commands as soon as it's pasted, because it contains a line break or the program\(place) can't tell pasting from typing. Check it before you paste."
            allow = "Paste"
        case .osc52Read:
            alert.messageText = "Let \(program.map { "“\($0)”" } ?? "this program") read the clipboard?"
            alert.informativeText = "\(who)\(place) wants to read your clipboard, shown below. Allow it only if you trust the program."
            allow = "Allow"
        case .osc52Write:
            alert.messageText = "Let \(program.map { "“\($0)”" } ?? "this program") change the clipboard?"
            alert.informativeText = "\(who)\(place) wants to put the text below on your clipboard."
            allow = "Allow"
        }
        // A program's request can arrive while you type: Return turns it down. A paste is yours,
        // so Return pastes it, as in Ghostty.
        let allowFirst = request.kind == .paste
        if allowFirst {
            alert.addButton(withTitle: allow)
            alert.addButton(withTitle: "Cancel")
        } else {
            alert.addButton(withTitle: "Deny")
            alert.addButton(withTitle: allow)
        }
        alert.buttons[0].keyEquivalent = "\r"
        alert.buttons[1].keyEquivalent = allowFirst ? "\u{1b}" : ""
        alert.accessoryView = textView(request.contents)
        let answer: (NSApplication.ModalResponse) -> Void = { response in
            request.respond(allow: (response == .alertFirstButtonReturn) == allowFirst)
        }
        if let window, window.isVisible {
            alert.beginSheetModal(for: window, completionHandler: answer)
        } else {
            answer(alert.runModal())
        }
    }

    /// The text in question, read-only and scrollable, as it would be pasted or read.
    private static func textView(_ contents: String) -> NSView {
        let scroll = NSTextView.scrollableTextView()
        scroll.frame = NSRect(x: 0, y: 0, width: 460, height: 160)
        scroll.hasVerticalScroller = true
        scroll.borderType = .bezelBorder
        guard let text = scroll.documentView as? NSTextView else { return scroll }
        text.isEditable = false
        text.isSelectable = true
        text.font = .monospacedSystemFont(ofSize: NSFont.smallSystemFontSize, weight: .regular)
        var shown = String(contents.prefix(shownLimit))
        if contents.count > shownLimit { shown += "\n… and \(contents.count - shownLimit) more characters" }
        text.string = shown.isEmpty ? "(nothing)" : shown
        // An accessibility name, so VoiceOver (and a test driving it) can find it.
        text.setAccessibilityLabel("Clipboard text")
        return scroll
    }
}

extension DinoModel {
    /// Ghostty's `confirm-close-surface`: "true" asks when a program other than the shell runs in
    /// it, "always" every time, "false" never.
    var confirmClose: String {
        Self.terminals.configText("confirm-close-surface") ?? "true"
    }

    /// Before ending `shells` (closing their tab or pane): asks, as Ghostty would, when something
    /// other than a shell's prompt runs in one. A tmux client only detaches, and tmux keeps
    /// everything, so it doesn't count. True to go ahead.
    func confirmEnding(_ shells: [SessionInfo], in place: String) -> Bool {
        let live = shells.filter { $0.agent_id == "shell" && !$0.exited }
        let setting = confirmClose
        guard !live.isEmpty, setting != "false" else { return true }
        let busy = running(in: live)
        guard !busy.isEmpty || setting == "always" else { return true }
        let alert = NSAlert()
        if let (s, what) = busy.first {
            let here = busy.count > 1 || live.count > 1 ? "in “\(tabName(s))”" : "in this \(place)"
            alert.messageText = "\(what) is running \(here). Close it?"
            alert.informativeText = busy.count > 1
                ? "Programs are running in \(busy.count) of its shells. Closing ends them."
                : "Closing the \(place) ends it."
        } else {
            alert.messageText = "Close this \(place)?"
            alert.informativeText = "Its shell ends. dino asks because confirm-close-surface is set to always in your Ghostty config."
        }
        alert.addButton(withTitle: "Close")
        alert.addButton(withTitle: "Cancel")
        return alert.runModal() == .alertFirstButtonReturn
    }

    /// What runs in each of `shells` besides its prompt, asked of dinod now (the last poll may
    /// predate the command just started). A tmux client only detaches, and tmux keeps everything,
    /// so it doesn't count.
    private func running(in shells: [SessionInfo]) -> [(SessionInfo, String)] {
        shells.compactMap { s in
            if s.tmux != nil { return nil }
            if let inside = s.inside { return (s, launcherLabel(inside.agent)) }
            guard s.host == nil else { return s.running == true ? (s, "A command") : nil }
            switch Self.foregroundNow(s.id) {
            case .some(.some(let fg)): return (s, fg.name.isEmpty ? "A command" : fg.name)
            case .some(.none): return nil
            // An older dinod, or none answering in time: what the last poll said.
            case .none: return s.foreground.map { (s, $0.name) } ?? (s.running == true ? (s, "A command") : nil)
            }
        }
    }

    /// ⌘W asks before closing an agent (Settings → General), until "Don't ask again".
    static let askBeforeClosingKey = "askBeforeClosingAgents"
    static var askBeforeClosing: Bool { UserDefaults.standard.object(forKey: askBeforeClosingKey) as? Bool ?? true }

    /// Before ⌘W (or a tab's ✕, or Ghostty's `close_tab`) closes `sessions` with their tab or pane:
    /// an agent stops and is archived, a shell ends. One question, "Close …?", saying so, asked for
    /// an agent until "Don't ask again" is ticked. Shells alone ask as they always have, as Ghostty's
    /// `confirm-close-surface` says, and never a second time. True to go ahead.
    func confirmClosing(_ sessions: [SessionInfo], in place: String) -> Bool {
        let agents = sessions.filter { $0.agent_id != "shell" }
        let shells = sessions.filter { $0.agent_id == "shell" && !$0.exited }
        guard !agents.isEmpty, Self.askBeforeClosing else { return confirmEnding(shells, in: place) }
        let working = agents.filter { s in
            switch status(of: s) {
            case .thinking, .working, .waiting: true
            default: false
            }
        }
        var lines: [String] = []
        if agents.count == 1, let a = agents.first {
            let agent = launcherLabel(a.agent_id)
            lines.append(a.exited ? "The session is archived, and you can resume it from Archived."
                : working.isEmpty ? "\(agent) stops and the session is archived. You can resume it from Archived."
                : "\(agent) is working: it stops in the middle of what it's doing, and the session is archived. You can resume it from Archived.")
        } else {
            lines.append("Its \(agents.count) agents stop and their sessions are archived. You can resume them from Archived."
                + (working.isEmpty ? "" : working.count == agents.count ? " They're working." : " \(working.count) of them are working."))
        }
        if !shells.isEmpty {
            let busy = running(in: shells)
            lines.append(busy.isEmpty ? (shells.count == 1 ? "Its shell ends." : "Its shells end.")
                : busy.map { s, what in "\(what) is running in “\(tabName(s))”, and closing ends it." }.joined(separator: " "))
        }
        let alert = NSAlert()
        alert.messageText = "Close “\(sessions.map { tabName($0) }.joined(separator: " | "))”?"
        alert.informativeText = lines.joined(separator: "\n")
        alert.addButton(withTitle: "Close")
        alert.addButton(withTitle: "Cancel")
        alert.showsSuppressionButton = true
        alert.suppressionButton?.title = "Don't ask again"
        guard alert.runModal() == .alertFirstButtonReturn else { return false }
        if alert.suppressionButton?.state == .on { UserDefaults.standard.set(false, forKey: Self.askBeforeClosingKey) }
        return true
    }

    /// Session `id`'s foreground right now, from dinod: nil when it couldn't say (an older dinod,
    /// or no answer within half a second), `.some(nil)` at a shell's prompt.
    nonisolated static func foregroundNow(_ id: String) -> ForegroundProcess?? {
        guard let conn = try? DinoConnection(path: DinoEnvironment.socketPath) else { return nil }
        conn.timeout(0.5)
        do { return .some(try conn.foreground(session: id)) } catch { return nil }
    }

    /// "Close Session" (⇧⌘⌫, the sidebar's menu): a shell asks first when something runs in it;
    /// an agent stops as before.
    func closeSession(_ id: String) {
        guard let s = sessions.first(where: { $0.id == id }), s.agent_id == "shell" else { return kill(id) }
        guard confirmEnding([s], in: "tab") else { return }
        // As closing its tab: ⌘Z brings the shell back.
        dropTab(id)
    }
}
