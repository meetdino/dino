import AppKit
import Foundation

/// Help → Report a Bug…: GitHub's bug form for dino with what it asks you to look up filled in
/// (.github/ISSUE_TEMPLATE/bug.yml, whose field ids are the query's keys). Only versions go in:
/// no path, session, setting or account, and nothing is sent until you submit the form yourself.
enum BugReport {
    static let newIssue = "https://github.com/meetdino/dino/issues/new"

    /// What's filled in, each as the form shows it.
    struct Facts {
        /// "0.1.7 (build 12, c1ddaea2f)".
        var app: String
        /// "0.1.7 (c1ddaea2f)"; nil when dinod didn't answer.
        var dinod: String?
        /// "macOS 26.4 (25E246) on Apple M3 Pro".
        var macos: String
        /// Installed agents and their versions: ("Claude Code", "2.1.288").
        var agents: [(name: String, version: String)]
    }

    /// A field is cut to `fieldLimit` characters, and to `escapedLimit` once escaped (a character
    /// can take 12), so the three always fit in `urlLimit`; agents are left out from the end to
    /// keep under it. Browsers, GitHub and the clipboard all take this much.
    static let fieldLimit = 200
    static let escapedLimit = 600
    static let urlLimit = 2000

    static func url(_ facts: Facts) -> URL {
        let version = clean(facts.app) + "; dinod " + (facts.dinod.map(clean) ?? "not running")
        let agents = facts.agents.map { clean($0.name + " " + $0.version) }.filter { !$0.isEmpty }
        func build(_ agents: [String]) -> String {
            var query = [("template", "bug.yml"), ("version", cut(version)), ("macos", cut(clean(facts.macos)))]
            if !agents.isEmpty { query.append(("agent", cut(agents.joined(separator: ", ")))) }
            return newIssue + "?" + query.map { escape($0.0) + "=" + escape($0.1) }.joined(separator: "&")
        }
        var kept = agents
        var text = build(kept)
        while text.count > urlLimit, !kept.isEmpty {
            kept.removeLast()
            text = build(kept)
        }
        return URL(string: text)!
    }

    /// One line, without anything that could be a path (a word with a slash or tilde in it).
    static func clean(_ text: String) -> String {
        text.split(whereSeparator: { $0.isWhitespace || $0.isNewline || $0.unicodeScalars.contains { CharacterSet.controlCharacters.contains($0) } })
            .filter { !$0.contains("/") && !$0.contains("~") }
            .joined(separator: " ")
    }

    static func cut(_ text: String) -> String {
        guard text.count > fieldLimit || escape(text).count > escapedLimit else { return text }
        var kept = String(text.prefix(fieldLimit - 1))
        while escape(kept + "…").count > escapedLimit { kept.removeLast() }
        return kept + "…"
    }

    /// Percent-encoded but for RFC 3986's unreserved characters: `&`, `=`, `+` and `#` stay in
    /// their value (a `+` would otherwise read as a space).
    static func escape(_ text: String) -> String {
        var unreserved = CharacterSet(charactersIn: "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789")
        unreserved.insert(charactersIn: "-._~")
        return text.addingPercentEncoding(withAllowedCharacters: unreserved) ?? ""
    }

    // MARK: - On this Mac

    static var appVersion: String {
        let info = Bundle.main.infoDictionary ?? [:]
        let short = info["CFBundleShortVersionString"] as? String ?? "?"
        let build = [(info["CFBundleVersion"] as? String).map { "build \($0)" }, info["DinoBuild"] as? String].compactMap { $0 }
        return build.isEmpty ? short : "\(short) (\(build.joined(separator: ", ")))"
    }

    static var macos: String {
        let v = ProcessInfo.processInfo.operatingSystemVersion
        var text = "macOS \(v.majorVersion).\(v.minorVersion)" + (v.patchVersion > 0 ? ".\(v.patchVersion)" : "")
        if let build = sysctl("kern.osversion") { text += " (\(build))" }
        if let chip = sysctl("machdep.cpu.brand_string") { text += " on \(chip)" }
        return text
    }

    private static func sysctl(_ name: String) -> String? {
        var size = 0
        guard sysctlbyname(name, nil, &size, nil, 0) == 0, size > 1 else { return nil }
        var bytes = [CChar](repeating: 0, count: size)
        guard sysctlbyname(name, &bytes, &size, nil, 0) == 0 else { return nil }
        let text = String(cString: bytes).trimmingCharacters(in: .whitespaces)
        return text.isEmpty ? nil : text
    }

    /// Asks dinod which dino it is and which agents are installed (each answers `--version` at
    /// once, so it waits `agentsWait` at most), then opens the form in the browser.
    static func open(agentsWait: TimeInterval = 2) {
        Task.detached {
            let dinod = (try? DinoConnection(path: DinoEnvironment.socketPath).request(["type": "version"])).flatMap { reply in
                reply.dino.map { v in reply.build.map { "\(v) (\($0))" } ?? v }
            }
            let agents = Self.installedAgents(wait: agentsWait)
            let form = url(Facts(app: appVersion, dinod: dinod, macos: macos, agents: agents))
            await MainActor.run { _ = NSWorkspace.shared.open(form) }
        }
    }

    private static func installedAgents(wait: TimeInterval) -> [(name: String, version: String)] {
        final class Box: @unchecked Sendable { var agents: [AgentSetupInfo] = [] }
        let box = Box()
        let done = DispatchSemaphore(value: 0)
        DispatchQueue.global(qos: .userInitiated).async {
            box.agents = (try? DinoConnection(path: DinoEnvironment.socketPath).agentSetup()) ?? []
            done.signal()
        }
        guard done.wait(timeout: .now() + wait) == .success else { return [] }
        return box.agents.compactMap { a in a.version.map { (a.name, $0) } }
    }
}

/// Help → Show dinod Log: the log of the dinod this app runs with (`$DINO_HOME/dinod.log`, as
/// dinod writes it), selected in the Finder; before dinod has written one, its folder.
enum DinodLog {
    static var url: URL { URL(fileURLWithPath: DinoEnvironment.home).appendingPathComponent("dinod.log") }

    @MainActor static func reveal(_ model: DinoModel) {
        let log = url
        if FileManager.default.fileExists(atPath: log.path) {
            NSWorkspace.shared.activateFileViewerSelecting([log])
        } else if FileManager.default.fileExists(atPath: DinoEnvironment.home) {
            NSWorkspace.shared.open(URL(fileURLWithPath: DinoEnvironment.home))
        } else {
            model.error = "dinod hasn't started yet, so it has no log. It keeps one at \(log.path)."
        }
    }
}
