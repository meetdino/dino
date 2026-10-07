import AppKit

/// Choosing a folder in the Finder's panel. With folders choosable, the panel's button takes the
/// folder highlighted in its list over the one it shows, and one is highlighted without asking:
/// go up a level (⌘↑, the path menu, Back) and the folder you came out of is. So Open in your home
/// folder, coming up from ~/Movies, opened ~/Movies. The button names the folder it takes, as the
/// panel's selection and folder change (what its delegate is told, as Apple's "Getting the Current
/// Selection" has it): `Open “Movies”` there, `Open “talian”` with nothing highlighted.
@MainActor
enum FolderPanel {
    /// The folder chosen; nil when cancelled. `verb`: the button, before the folder's name.
    static func choose(in directory: URL?, verb: String = "Open", message: String? = nil, canCreate: Bool = true) -> URL? {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.canCreateDirectories = canCreate
        if let directory { panel.directoryURL = directory }
        if let message { panel.message = message }
        let namer = Namer(panel: panel, verb: verb)
        panel.delegate = namer
        namer.update()
        let ok = panel.runModal() == .OK
        panel.delegate = nil
        return ok ? panel.url : nil
    }

    /// What the button says for `verb` and the folder it takes (the highlighted one, else the one shown).
    nonisolated static func prompt(_ verb: String, taking url: URL?) -> String {
        guard let url else { return verb }
        return "\(verb) “\(FileManager.default.displayName(atPath: url.path))”"
    }

    /// Keeps the button's title on the folder it takes. The panel holds its delegate weakly: `choose` keeps this alive.
    @MainActor private final class Namer: NSObject, NSOpenSavePanelDelegate {
        let panel: NSOpenPanel
        let verb: String

        init(panel: NSOpenPanel, verb: String) {
            self.panel = panel
            self.verb = verb
        }

        func update() {
            panel.prompt = FolderPanel.prompt(verb, taking: panel.url ?? panel.directoryURL)
        }

        func panelSelectionDidChange(_ sender: Any?) { update() }
        func panel(_ sender: Any, didChangeToDirectoryURL url: URL?) { update() }
    }
}
