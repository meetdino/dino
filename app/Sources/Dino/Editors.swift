import AppKit
import SwiftUI

/// An app dino opens things in: the file pane's file (at its line) or a session's folder.
/// The one list of them; the file pane offers the ones that edit files, "Open in" all of them.
struct ExternalEditor: Identifiable, Equatable {
    let name: String
    let bundleID: String
    /// Terminals and Finder open a folder but not a file.
    var foldersOnly = false
    var id: String { bundleID }

    static let finder = "com.apple.finder"

    /// Editors first, then terminals, then Finder; the order the menus list them in.
    static let known = [
        ExternalEditor(name: "VS Code", bundleID: "com.microsoft.VSCode"),
        ExternalEditor(name: "Cursor", bundleID: "com.todesktop.230313mzl4w4u92"),
        ExternalEditor(name: "Windsurf", bundleID: "com.exafunction.windsurf"),
        ExternalEditor(name: "Zed", bundleID: "dev.zed.Zed"),
        ExternalEditor(name: "Zed Preview", bundleID: "dev.zed.Zed-Preview"),
        ExternalEditor(name: "Xcode", bundleID: "com.apple.dt.Xcode"),
        ExternalEditor(name: "Terminal", bundleID: "com.apple.Terminal", foldersOnly: true),
        ExternalEditor(name: "iTerm", bundleID: "com.googlecode.iterm2", foldersOnly: true),
        ExternalEditor(name: "Finder", bundleID: finder, foldersOnly: true),
    ]

    /// The installed ones that edit files.
    static var installed: [ExternalEditor] { installedEditors().filter { !$0.foldersOnly } }

    static let preferenceKey = "editor"

    /// The one picked last, else the one that opens this kind of file, else the first installed.
    static func preferred(for path: String) -> ExternalEditor? {
        let all = installed
        if let id = UserDefaults.standard.string(forKey: preferenceKey), let e = all.first(where: { $0.bundleID == id }) { return e }
        if let app = NSWorkspace.shared.urlForApplication(toOpen: URL(fileURLWithPath: path)),
           let id = Bundle(url: app)?.bundleIdentifier, let e = all.first(where: { $0.bundleID == id })
        {
            return e
        }
        return all.first
    }

    var appURL: URL? { NSWorkspace.shared.urlForApplication(withBundleIdentifier: bundleID) }

    var icon: NSImage? {
        guard let app = appURL else { return nil }
        let image = NSWorkspace.shared.icon(forFile: app.path)
        image.size = NSSize(width: 16, height: 16)
        return image
    }

    /// Open a file, at `line` where the editor can be told one.
    func open(_ path: String, line: Int?) {
        let file = URL(fileURLWithPath: path)
        if let line {
            let encoded = path.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? path
            switch bundleID {
            case "com.microsoft.VSCode":
                if let u = URL(string: "vscode://file\(encoded):\(line)"), NSWorkspace.shared.open(u) { return }
            case "com.todesktop.230313mzl4w4u92":
                if let u = URL(string: "cursor://file\(encoded):\(line)"), NSWorkspace.shared.open(u) { return }
            case "com.exafunction.windsurf":
                if let u = URL(string: "windsurf://file\(encoded):\(line)"), NSWorkspace.shared.open(u) { return }
            case "dev.zed.Zed", "dev.zed.Zed-Preview":
                // Zed's own CLI, inside the app, takes `path:line`.
                if let cli = appURL?.appendingPathComponent("Contents/MacOS/cli"), run(cli.path, ["\(path):\(line)"]) { return }
            case "com.apple.dt.Xcode":
                if run("/usr/bin/xed", ["--line", "\(line)", path]) { return }
            default: break
            }
        }
        guard let app = appURL else { return }
        NSWorkspace.shared.open([file], withApplicationAt: app, configuration: NSWorkspace.OpenConfiguration())
    }

    /// Open a folder: a window on it for an editor, a shell there for a terminal.
    func openFolder(_ folder: String) {
        let url = URL(fileURLWithPath: folder, isDirectory: true)
        if bundleID == Self.finder {
            NSWorkspace.shared.activateFileViewerSelecting([url])
            return
        }
        guard let app = appURL else { return }
        let config = NSWorkspace.OpenConfiguration()
        config.activates = true
        NSWorkspace.shared.open([url], withApplicationAt: app, configuration: config)
    }

    private func run(_ tool: String, _ args: [String]) -> Bool {
        guard FileManager.default.isExecutableFile(atPath: tool) else { return false }
        let p = Process()
        p.executableURL = URL(fileURLWithPath: tool)
        p.arguments = args
        p.standardOutput = FileHandle.nullDevice
        p.standardError = FileHandle.nullDevice
        return (try? p.run()) != nil
    }
}

/// The apps in `ExternalEditor.known` that are installed, looked up by bundle id wherever they live.
func installedEditors() -> [ExternalEditor] {
    ExternalEditor.known.filter { $0.appURL != nil }
}

/// The toolbar's "Open in": a click opens the folder where you last did, the arrow picks another app.
struct OpenInMenu: View {
    @EnvironmentObject var model: DinoModel
    @AppStorage("openIn") private var last = ""
    /// Looked up once: apps rarely come and go while dino is open.
    @State private var editors = installedEditors()

    private var folder: String? { model.selectedSession?.cwd }
    /// The app used here last, else the file pane's editor, else the first installed.
    private var preferred: ExternalEditor? {
        editors.first { $0.bundleID == last }
            ?? editors.first { $0.bundleID == UserDefaults.standard.string(forKey: ExternalEditor.preferenceKey) }
            ?? editors.first
    }

    var body: some View {
        Menu {
            ForEach(editors) { e in
                Button {
                    open(e)
                } label: {
                    Label {
                        Text(e.name)
                    } icon: {
                        if let icon = e.icon { Image(nsImage: icon) }
                    }
                }
            }
        } label: {
            Label("Open in \(preferred?.name ?? "…")", systemImage: "arrow.up.forward.app")
        } primaryAction: {
            if let preferred { open(preferred) }
        }
        .help(folder.map { "Open \(shortPath($0)) in \(preferred?.name ?? "another app")" } ?? "Open the session's folder in another app")
        .disabled(folder == nil || editors.isEmpty)
        .onAppear { editors = installedEditors() }
    }

    private func open(_ e: ExternalEditor) {
        guard let folder else { return }
        last = e.bundleID
        e.openFolder(folder)
    }
}
