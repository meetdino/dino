import AppKit
import SwiftUI

/// An app on this Mac that can open a session's folder.
struct Editor: Identifiable, Equatable {
    var bundleID: String
    var name: String
    var app: URL
    var id: String { bundleID }

    var icon: NSImage {
        let image = NSWorkspace.shared.icon(forFile: app.path)
        image.size = NSSize(width: 16, height: 16)
        return image
    }

    /// Open `folder` in it: a window on the folder for an editor, a shell there for a terminal.
    func open(_ folder: String) {
        let url = URL(fileURLWithPath: folder, isDirectory: true)
        if bundleID == Self.finder {
            NSWorkspace.shared.activateFileViewerSelecting([url])
            return
        }
        let config = NSWorkspace.OpenConfiguration()
        config.activates = true
        NSWorkspace.shared.open([url], withApplicationAt: app, configuration: config)
    }

    static let finder = "com.apple.finder"

    /// Editors first, then terminals, then Finder; the order the menu lists them in.
    static let known: [(id: String, name: String)] = [
        ("com.microsoft.VSCode", "VS Code"),
        ("com.todesktop.230313mzl4w4u92", "Cursor"),
        ("com.exafunction.windsurf", "Windsurf"),
        ("dev.zed.Zed", "Zed"),
        ("com.apple.dt.Xcode", "Xcode"),
        ("com.apple.Terminal", "Terminal"),
        ("com.googlecode.iterm2", "iTerm"),
        (finder, "Finder"),
    ]
}

/// The apps in `Editor.known` that are installed, looked up by bundle id wherever they live.
func installedEditors() -> [Editor] {
    Editor.known.compactMap { e in
        NSWorkspace.shared.urlForApplication(withBundleIdentifier: e.id).map { Editor(bundleID: e.id, name: e.name, app: $0) }
    }
}

/// The toolbar's "Open in": a click opens the folder where you last did, the arrow picks another app.
struct OpenInMenu: View {
    @EnvironmentObject var model: DinoModel
    @AppStorage("openIn") private var last = ""
    /// Looked up once: apps rarely come and go while dino is open.
    @State private var editors = installedEditors()

    private var folder: String? { model.selectedSession?.cwd }
    private var preferred: Editor? { editors.first { $0.bundleID == last } ?? editors.first }

    var body: some View {
        Menu {
            ForEach(editors) { e in
                Button {
                    open(e)
                } label: {
                    Label { Text(e.name) } icon: { Image(nsImage: e.icon) }
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

    private func open(_ e: Editor) {
        guard let folder else { return }
        last = e.bundleID
        e.open(folder)
    }
}
