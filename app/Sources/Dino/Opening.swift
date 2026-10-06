import AppKit
import UniformTypeIdentifiers

/// dino as the Mac's terminal: folders and scripts opened with it (`open -a dino ~/code`, a
/// double-clicked .command), man pages (`x-man-page:` links), the Finder's "New dino Shell at
/// Folder", and being the default for them, as Terminal is.
@MainActor
enum Opening {
    /// What was opened before dinod answered; handled once it has.
    static var pending: [URL] = []

    /// What a default terminal opens: scripts (.command, .tool), programs, and man-page links.
    static let scriptTypes = ["com.apple.terminal.shell-script", "public.unix-executable"].compactMap { UTType($0) }
    static let schemes = ["x-man-page"]

    /// dino opens scripts, programs and man pages, instead of Terminal or another app.
    static var isDefault: Bool {
        let me = Bundle.main.bundleIdentifier
        let apps = scriptTypes.map { NSWorkspace.shared.urlForApplication(toOpen: $0) }
            + schemes.compactMap { URL(string: "\($0):ls") }.map { NSWorkspace.shared.urlForApplication(toOpen: $0) }
        return apps.allSatisfy { $0.flatMap { Bundle(url: $0)?.bundleIdentifier } == me }
    }

    /// Settings → Terminal's "Make dino the Default Terminal". LaunchServices' own calls, as iTerm2
    /// makes them: NSWorkspace's replacement waits on a confirmation macOS doesn't always show for
    /// an app that isn't notarized, and never returns. False if LaunchServices refused one; macOS
    /// applies them up to a minute or so later.
    static func makeDefault() -> Bool {
        guard let me = Bundle.main.bundleIdentifier else { return false }
        let ls: LaunchServicesDefaults = LegacyLaunchServices()
        return scriptTypes.map { ls.setHandler(me, forType: $0.identifier) }.allSatisfy { $0 }
            && schemes.map { ls.setHandler(me, forScheme: $0) }.allSatisfy { $0 }
    }

    /// A man page link as `man` arguments: `x-man-page://ls`, `x-man-page://1/ls`.
    static func manPage(_ url: URL) -> [String]? {
        let rest = url.absoluteString.dropFirst("x-man-page:".count).drop { $0 == "/" }
        let parts = rest.split(separator: "/").map { String($0).removingPercentEncoding ?? String($0) }
        // Not an option: `x-man-page://-Pbash/ls` would make `man` run a pager of the link's choosing.
        let ok = { (s: String) in !s.isEmpty && !s.hasPrefix("-") && s.allSatisfy { $0.isLetter || $0.isNumber || "._+-:".contains($0) } }
        switch parts.count {
        case 1 where ok(parts[0]): return [parts[0]]
        case 2 where ok(parts[0]) && ok(parts[1]): return parts
        default: return nil
        }
    }

    /// `s` quoted for a POSIX shell.
    static func quoted(_ s: String) -> String {
        "'" + s.replacingOccurrences(of: "'", with: "'\\''") + "'"
    }

    /// Opening a script runs it with dino's own access (files, folders it was allowed), so ask
    /// first, as Ghostty does: any app can open a file with dino.
    static func confirmRun(_ file: URL) -> Bool {
        let alert = NSAlert()
        alert.messageText = "Run “\(file.lastPathComponent)”?"
        alert.informativeText = "It runs in a new shell in \(NSString(string: file.deletingLastPathComponent().path).abbreviatingWithTildeInPath) and can access your files, just as dino can. Only run scripts you trust."
        alert.addButton(withTitle: "Run")
        alert.addButton(withTitle: "Cancel")
        NSApp.activate(ignoringOtherApps: true)
        return alert.runModal() == .alertFirstButtonReturn
    }
}

/// Serves the Finder's Services menu: "New dino Shell at Folder".
final class ServiceProvider: NSObject {
    weak var model: DinoModel?

    @MainActor @objc func openShellAtFolder(_ pboard: NSPasteboard, userData _: String?, error _: AutoreleasingUnsafeMutablePointer<NSString?>) {
        let urls = (pboard.readObjects(forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]) as? [URL]) ?? []
        // A file selected instead of a folder: its folder.
        let folders = urls.map { $0.hasDirectoryPath ? $0 : $0.deletingLastPathComponent() }
        NSApp.activate(ignoringOtherApps: true)
        model?.open(folders)
    }
}

extension DinoModel {
    /// Folders open a shell there; scripts and programs run in a new shell in their folder, once
    /// you say so; man-page links show the page. Before dinod answers they wait (see
    /// `Opening.pending`).
    func open(_ urls: [URL]) {
        guard connection != nil, launched, let shell = launchers.first(where: { $0.short == "shell" }) else {
            Opening.pending += urls
            return
        }
        for url in urls {
            if url.scheme == "x-man-page" {
                guard let args = Opening.manPage(url) else { continue }
                // Quitting the pager closes the shell, as closing Terminal's man window would.
                newSession(shell, in: NSHomeDirectory(), line: "man -- \(args.map(Opening.quoted).joined(separator: " ")); exit", label: "man \(args.last ?? "")")
            } else if url.isFileURL {
                var dir: ObjCBool = false
                guard FileManager.default.fileExists(atPath: url.path, isDirectory: &dir) else { continue }
                if dir.boolValue {
                    newSession(shell, in: url.path)
                } else if Opening.confirmRun(url) {
                    // Not executable (a .sh from a download): run it with sh, as Terminal would fail to.
                    let run = FileManager.default.isExecutableFile(atPath: url.path) ? Opening.quoted(url.path) : "sh \(Opening.quoted(url.path))"
                    newSession(shell, in: url.deletingLastPathComponent().path, line: run, label: url.lastPathComponent)
                }
            }
        }
    }
}

/// Setting default handlers. LaunchServices' functions are deprecated for NSWorkspace's, which hang
/// (see `Opening.makeDefault`); called through this protocol, the deprecation stays here.
private protocol LaunchServicesDefaults {
    func setHandler(_ bundle: String, forType type: String) -> Bool
    func setHandler(_ bundle: String, forScheme scheme: String) -> Bool
}

private struct LegacyLaunchServices: LaunchServicesDefaults {
    /// For opening it at all (a double-click), and as its shell.
    @available(macOS, deprecated: 12)
    func setHandler(_ bundle: String, forType type: String) -> Bool {
        [LSRolesMask.all, .shell].allSatisfy { LSSetDefaultRoleHandlerForContentType(type as CFString, $0, bundle as CFString) == noErr }
    }

    @available(macOS, deprecated: 12)
    func setHandler(_ bundle: String, forScheme scheme: String) -> Bool {
        LSSetDefaultHandlerForURLScheme(scheme as CFString, bundle as CFString) == noErr
    }
}
