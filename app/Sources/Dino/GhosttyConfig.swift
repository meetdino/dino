import Foundation
import GhosttyTerminal

/// The user's own Ghostty config under dino's few overrides, so a pane looks and types like their
/// Ghostty: font, theme, cursor, keybinds. Read the way Ghostty reads it, and again when it changes.
@MainActor
enum GhosttyConfig {
    /// Where Ghostty looks, in its order: later files win, macOS's after the XDG ones.
    static let files: [URL] = {
        let home = FileManager.default.homeDirectoryForCurrentUser
        let xdg = ProcessInfo.processInfo.environment["XDG_CONFIG_HOME"].flatMap { $0.isEmpty ? nil : URL(fileURLWithPath: $0) }
            ?? home.appendingPathComponent(".config")
        let support = home.appendingPathComponent("Library/Application Support/com.mitchellh.ghostty")
        return [xdg.appendingPathComponent("ghostty/config.ghostty"), xdg.appendingPathComponent("ghostty/config"),
                support.appendingPathComponent("config.ghostty"), support.appendingPathComponent("config")]
    }()
    private static let paths = files.map(\.path)

    /// Settings that would take a pane from dino: every pane runs `dino attach`, and dinod starts
    /// the shell or agent behind it, in its own folder.
    static let ignored: Set<String> = ["command", "initial-command", "working-directory", "wait-after-command", "input", "env"]

    private static let colorKeys = ["theme", "background", "foreground", "palette", "cursor-color", "cursor-text", "selection-"]

    /// The config files read last time (includes too), and lines Ghostty refused.
    private(set) static var loaded: [String] = []
    private(set) static var skipped: [String] = []
    private static var stamps: [String: Date] = [:]

    /// `overrides` go last, so they win over anything the user set.
    static func apply(to controller: TerminalController, overrides: String) {
        var read: [String] = []
        let all = files.flatMap { expand($0, depth: 0, read: &read) }
        var lines = all
        loaded = read
        stamps = Dictionary(uniqueKeysWithValues: read.map { ($0, modified($0)) })
        skipped = []
        // The library's built-in light and dark colors go after the config: they'd hide the user's.
        let colored = all.contains { line in colorKeys.contains { line.hasPrefix($0) } }
        controller.setTheme(colored ? TerminalTheme() : .default)
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
