import AppKit
import Foundation
import DinoGhostty

/// The user's own Ghostty config under dino's few overrides, so a pane looks and types like their
/// Ghostty: font, theme, cursor, keybinds. Read the way Ghostty reads it, and again when it changes.
@MainActor
enum GhosttyConfig {
    /// Where Ghostty looks, in its order: later files win, macOS's after the XDG ones.
    static let files: [URL] = {
        let support = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/Application Support/com.mitchellh.ghostty")
        return [xdg.appendingPathComponent("ghostty/config.ghostty"), xdg.appendingPathComponent("ghostty/config"),
                support.appendingPathComponent("config.ghostty"), support.appendingPathComponent("config")]
    }()

    private static let xdg: URL = ProcessInfo.processInfo.environment["XDG_CONFIG_HOME"].flatMap { $0.isEmpty ? nil : URL(fileURLWithPath: $0) }
        ?? FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".config")

    /// Where Ghostty finds a theme by name, in its order: the user's own themes, then the ones that
    /// come with Ghostty (in Ghostty.app), then Ghostty 1.3.1's, which dino carries for a Mac
    /// without Ghostty. A name found nowhere would fall back to the engine's dark colors.
    private static let themeFolders: [URL] = {
        var folders = [xdg.appendingPathComponent("ghostty/themes")]
        let apps = [NSWorkspace.shared.urlForApplication(withBundleIdentifier: "com.mitchellh.ghostty"),
                    URL(fileURLWithPath: "/Applications/Ghostty.app"),
                    FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Applications/Ghostty.app")]
        for case let app? in apps {
            let themes = app.appendingPathComponent("Contents/Resources/ghostty/themes")
            if !folders.contains(themes) { folders.append(themes) }
        }
        if let carried = GhosttyRuntimeResources.directoryURL?.appendingPathComponent("themes") { folders.append(carried) }
        return folders
    }()
    private static let paths = files.map(\.path)

    /// Settings that would take a pane from dino: every pane runs `dino attach`, and dinod starts
    /// the shell or agent behind it, in its own folder.
    static let ignored: Set<String> = ["command", "initial-command", "working-directory", "wait-after-command", "input", "env"]

    /// The settings that color a pane. With none of them, the pane takes dino's own light and dark
    /// colors; `background-opacity` and the like don't count.
    private static let colorKeys: Set<String> = ["theme", "background", "foreground", "palette", "cursor-color", "cursor-text",
                                                 "selection-background", "selection-foreground"]

    /// The config files read last time (includes too), and lines Ghostty refused.
    private(set) static var loaded: [String] = []
    private(set) static var skipped: [String] = []
    private static var stamps: [String: Date] = [:]

    /// The look a pane takes for Ghostty's `window-theme`, which picks the light or dark a Ghostty
    /// window (and so its theme's half) shows: nil follows the app's look (`system`, and `auto` with
    /// a `light:…,dark:…` theme or dino's own colors); `auto` with one theme goes by how light its
    /// background is. dino's own window keeps the app's look.
    private(set) static var paneAppearance: NSAppearance.Name? {
        didSet { if paneAppearance != oldValue { LinkTerminalView.paneAppearanceChanged() } }
    }

    /// `overrides` go last, so they win over anything the user set. A `light:…,dark:…` theme stays
    /// one: as in Ghostty, each pane takes the half for its own light or dark (the app's look, see
    /// `Appearance`), and switches when that changes (Ghostty's soft `reload_config`).
    static func apply(to controller: TerminalController, overrides: String) {
        var read: [String] = []
        let all = files.flatMap { expand($0, depth: 0, read: &read) }.compactMap(resolvingTheme)
        var lines = all
        loaded = read
        stamps = Dictionary(uniqueKeysWithValues: read.map { ($0, modified($0)) })
        skipped = []
        // The library's built-in light and dark colors go after the config: they'd hide the user's.
        let colored = all.contains { colorKeys.contains(key(of: $0)) }
        controller.setTheme(colored ? TerminalTheme() : .default)
        // dino's own colors are a light and dark pair too, as far as `window-theme = auto` goes.
        let paired = !colored || all.contains { $0.hasPrefix("theme = light:") }
        defer {
            paneAppearance = windowTheme(controller, paired: paired)
            readPaneChrome(controller, paired: paired)
        }
        // Ghostty takes all of a config or none of it: leave out the lines it names and try again.
        for _ in 0 ..< 2 {
            if controller.updateConfigSource(.generated((lines + [overrides]).joined(separator: "\n"))) { return }
            let bad = refused(controller.lastConfigurationIssue ?? "", count: lines.count)
            guard !bad.isEmpty else { break }
            skipped += bad.sorted().map { lines[$0] }
            lines = lines.enumerated().filter { !bad.contains($0.offset) }.map(\.element)
        }
        // Still refused: dino's own settings alone, as if there were no config.
        skipped = all
        controller.updateConfigSource(.generated(overrides))
    }

    /// `window-theme` as Ghostty applies it on the Mac (see `paneAppearance`); `ghostty` works on
    /// Linux only and acts as `auto` here.
    private static func windowTheme(_ controller: TerminalController, paired: Bool) -> NSAppearance.Name? {
        switch controller.configText("window-theme") {
        case "light": return .aqua
        case "dark": return .darkAqua
        case "system": return nil
        default:
            guard !paired, let bg = controller.configColor("background") else { return nil }
            // AppKit's light test, as Ghostty's own `isLightColor`.
            let luminance = (0.299 * Double(bg.red) + 0.587 * Double(bg.green) + 0.114 * Double(bg.blue)) / 255
            return luminance > 0.5 ? .aqua : .darkAqua
        }
    }

    /// A config line's key: `background` for `background = #fff`.
    private static func key(of line: String) -> String {
        line.split(separator: "=", maxSplits: 1).first.map { $0.trimmingCharacters(in: .whitespaces) } ?? ""
    }

    /// A `theme` line with its names made absolute paths, as Ghostty would find them (both halves of
    /// `light:…,dark:…`); nil, leaving it out, when one isn't anywhere, so the pane keeps dino's own
    /// light and dark colors rather than the engine's dark ones.
    private static func resolvingTheme(_ line: String) -> String? {
        let parts = line.split(separator: "=", maxSplits: 1).map { $0.trimmingCharacters(in: .whitespaces) }
        guard parts.count == 2, parts[0] == "theme" else { return line }
        let value = parts[1].trimmingCharacters(in: CharacterSet(charactersIn: "\""))
        func path(_ name: String) -> String? {
            let name = name.trimmingCharacters(in: .whitespaces)
            if name.hasPrefix("/") || name.hasPrefix("~") {
                let full = NSString(string: name).expandingTildeInPath
                return FileManager.default.fileExists(atPath: full) ? full : nil
            }
            return themeFolders.map { $0.appendingPathComponent(name).path }.first { FileManager.default.fileExists(atPath: $0) }
        }
        let pairs = value.split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces) }
        if pairs.count == 2, pairs.allSatisfy({ $0.hasPrefix("light:") || $0.hasPrefix("dark:") }) {
            func half(_ mode: String) -> String? {
                pairs.first { $0.hasPrefix(mode) }.flatMap { path(String($0.dropFirst(mode.count))) }
            }
            guard let light = half("light:"), let dark = half("dark:") else { return nil }
            return "theme = light:\(light),dark:\(dark)"
        }
        return path(value).map { "theme = \($0)" }
    }

    /// Ghostty's `reload_config`: read the files again now, as the two-second check would.
    static func reload() {
        apply(to: DinoModel.terminals, overrides: DinoModel.menuKeys)
    }

    /// Ghostty's `open_config`: the config file in the editor the Mac opens it with, the first of
    /// Ghostty's files that exists, or a new `config.ghostty` where Ghostty would make one.
    static func openInEditor() {
        let fm = FileManager.default
        let url = files.first { fm.fileExists(atPath: $0.path) } ?? files[0]
        if !fm.fileExists(atPath: url.path) {
            try? fm.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            fm.createFile(atPath: url.path, contents: Data())
        }
        // A file with no extension or `.ghostty` has no app of its own: open it as text.
        let editor = NSWorkspace.shared.urlForApplication(toOpen: url)
            ?? NSWorkspace.shared.urlForApplication(withBundleIdentifier: "com.apple.TextEdit")
        guard let editor else {
            NSWorkspace.shared.open(url)
            return
        }
        NSWorkspace.shared.open([url], withApplicationAt: editor, configuration: NSWorkspace.OpenConfiguration())
    }

    /// A config file changed, appeared or went since `apply` read them.
    static var changed: Bool {
        paths.contains { stamps[$0] == nil && access($0, F_OK) == 0 } || stamps.contains { modified($0.key) != $0.value }
    }

    /// A plain stat: this runs every two seconds, and Foundation's attributes read far more.
    private static func modified(_ path: String) -> Date {
        var st = stat()
        guard stat(path, &st) == 0 else { return .distantPast }
        return Date(timeIntervalSince1970: TimeInterval(st.st_mtimespec.tv_sec) + TimeInterval(st.st_mtimespec.tv_nsec) / 1e9)
    }

    /// The file's settings, then those of the files it includes: Ghostty loads a `config-file` after
    /// the rest of the file that names it. Relative paths are from that file's folder.
    private static func expand(_ url: URL, depth: Int, read: inout [String]) -> [String] {
        guard depth < 10, !read.contains(url.path), let text = try? String(contentsOf: url, encoding: .utf8) else { return [] }
        read.append(url.path)
        var lines: [String] = []
        var includes: [URL] = []
        for raw in text.components(separatedBy: .newlines) {
            let line = raw.trimmingCharacters(in: .whitespaces)
            guard !line.isEmpty, !line.hasPrefix("#") else { continue }
            let key = line.split(separator: "=", maxSplits: 1).first.map { $0.trimmingCharacters(in: .whitespaces) } ?? ""
            if key == "config-file" {
                var value = line.split(separator: "=", maxSplits: 1).dropFirst().first.map { $0.trimmingCharacters(in: .whitespaces) } ?? ""
                value = value.trimmingCharacters(in: CharacterSet(charactersIn: "\""))
                if value.hasPrefix("?") { value.removeFirst() }
                guard !value.isEmpty else { continue }
                let path = NSString(string: value).expandingTildeInPath
                includes.append(path.hasPrefix("/") ? URL(fileURLWithPath: path) : url.deletingLastPathComponent().appendingPathComponent(path))
            } else if !ignored.contains(key) {
                lines.append(line)
            }
        }
        return lines + includes.flatMap { expand($0, depth: depth + 1, read: &read) }
    }

    /// The indexes of the lines among the first `count` that Ghostty's diagnostics name, as
    /// "<file>:<line>:…" (1-based, in the generated file).
    private static func refused(_ issue: String, count: Int) -> Set<Int> {
        let pattern = try! NSRegularExpression(pattern: #"\.conf:(\d+):"#)
        let found = pattern.matches(in: issue, range: NSRange(issue.startIndex..., in: issue)).compactMap { m in
            Range(m.range(at: 1), in: issue).flatMap { Int(issue[$0]) }.map { $0 - 1 }
        }
        return Set(found.filter { $0 >= 0 && $0 < count })
    }
}
