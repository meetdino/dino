import AppKit
import CryptoKit
import ServiceManagement
import SwiftUI

/// dinod as this app's launch agent. macOS gives the privacy permissions granted to dino (Screen
/// Recording, Accessibility, automation…) to what the app is responsible for; a dinod started by
/// forking passed on whoever started it instead (a terminal, an app since quit). Registered with
/// SMAppService, the agent is the app's, and so is every shell and agent dinod starts.
///
/// scripts/release.sh puts the agent in Contents/Library/LaunchAgents: `dino daemon` from
/// Contents/Helpers, restarted by launchd if it crashes, not at login (dinod starts when dino is
/// used, as before, and keeps running when the app quits). The app writes the agent's label and
/// state in `$DINO_HOME/dinod.launchd`, where `dino` (the app's, or one on the `PATH`) looks when it
/// starts dinod: `launchctl kickstart` for this agent, the old way without it
/// (crates/dino/src/launchd.rs). The dino you use built from source (app/build.sh --install) carries
/// one as a release does; a build to try things in (app/build.sh) carries none.
enum DinodAgent {
    /// The agent this app carries for this dino: the one whose `DINO_HOME` is the one the app runs
    /// with (none in its plist: the user's own). Nil in a development build, and for an isolated
    /// `$DINO_HOME` the build wasn't made for, which never touches the real agent.
    static let bundled: (plist: String, label: String, data: Data)? = {
        let dir = Bundle.main.bundleURL.appendingPathComponent("Contents/Library/LaunchAgents")
        let own = canonical(DinoEnvironment.home)
        for url in (try? FileManager.default.contentsOfDirectory(at: dir, includingPropertiesForKeys: nil)) ?? [] where url.pathExtension == "plist" {
            guard let data = try? Data(contentsOf: url),
                  let plist = try? PropertyListSerialization.propertyList(from: data, format: nil) as? [String: Any],
                  let label = plist["Label"] as? String else { continue }
            let home = (plist["EnvironmentVariables"] as? [String: String])?["DINO_HOME"] ?? NSString(string: "~/.config/dino").expandingTildeInPath
            if canonical(home) == own { return (url.lastPathComponent, label, data) }
        }
        return nil
    }()

    private static func canonical(_ path: String) -> String {
        URL(fileURLWithPath: NSString(string: path).expandingTildeInPath).resolvingSymlinksInPath().standardizedFileURL.path
    }

    private static var service: SMAppService? { bundled.map { SMAppService.agent(plistName: $0.plist) } }

    /// Running dinod through launchd: registered and allowed in Login Items.
    static var enabled: Bool { service?.status == .enabled }

    /// Waiting for the user in System Settings → General → Login Items (turned off there).
    static var needsApproval: Bool { service?.status == .requiresApproval }

    /// The plist registered last, to register again when an update changes it.
    private static let registeredKey = "dinodAgentPlist"

    /// The agent and the build carrying it: launchd won't start an agent registered by the build
    /// an update replaced (its job fails to spawn), so every update registers it again, as does
    /// every rebuild of the dino you use (its commit). The build on disk now: an app still running
    /// when a rebuild replaced it registers the new one.
    private static var digest: String? {
        let info = Updates.diskInfo
        let build = (info["CFBundleVersion"] as? String ?? "") + ((info["DinoBuild"] as? String).map { " \($0)" } ?? "")
        return bundled.map { SHA256.hash(data: $0.data + Data(build.utf8)).map { String(format: "%02x", $0) }.joined() }
    }

    /// An update changed the app or its agent, and launchd runs the old one until dinod is stopped
    /// and the agent registered again (`setUp(stopped: true)`, when the app restarts dinod).
    static var stale: Bool {
        guard enabled, let registered = UserDefaults.standard.string(forKey: registeredKey) else { return false }
        return registered != digest
    }

    /// At launch (and with dinod stopped, before it starts again): registered, and its state
    /// written for `dino`. `stopped`: dinod isn't running, so registering again can't stop it.
    static func setUp(stopped: Bool = false) {
        guard let service, let digest else { return }
        let registered = UserDefaults.standard.string(forKey: registeredKey)
        switch service.status {
        case .notRegistered, .notFound:
            register(service, digest)
        case .enabled where registered != nil && registered != digest && stopped:
            // An update: launchd keeps the old agent, and won't start it, until it's registered again.
            try? service.unregister()
            register(service, digest)
        case .enabled where registered == nil:
            UserDefaults.standard.set(digest, forKey: registeredKey)
        default:
            break
        }
        writeRecord()
    }

    private static func register(_ service: SMAppService, _ digest: String) {
        do {
            try service.register()
            UserDefaults.standard.set(digest, forKey: registeredKey)
        } catch {
            // Turned off in Login Items: `status` says so, and Settings → General shows the way back.
            NSLog("dino: couldn't register dinod's launch agent: \(error.localizedDescription)")
        }
    }

    /// `$DINO_HOME/dinod.launchd`, as `dino` reads it (`dino_core::launchd_record`).
    static func writeRecord() {
        guard let bundled, let service else { return }
        let state = switch service.status {
        case .enabled: "enabled"
        case .requiresApproval: "requiresApproval"
        case .notRegistered: "notRegistered"
        case .notFound: "notFound"
        @unknown default: "unknown"
        }
        let text = "label=\(bundled.label)\nstate=\(state)\napp=\(Bundle.main.bundlePath)\nbundle=\(Bundle.main.bundleIdentifier ?? "")\n"
        let url = URL(fileURLWithPath: DinoEnvironment.home).appendingPathComponent("dinod.launchd")
        guard (try? String(contentsOf: url, encoding: .utf8)) != text else { return }
        try? FileManager.default.createDirectory(atPath: DinoEnvironment.home, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        try? Data(text.utf8).write(to: url, options: .atomic)
    }

    /// Once per agent, at launch: dinod would run without dino's permissions until it's allowed.
    @MainActor
    static func askForApproval() {
        guard needsApproval, let bundled else { return }
        let key = "dinodAgentAsked.\(bundled.label)"
        guard !UserDefaults.standard.bool(forKey: key) else { return }
        UserDefaults.standard.set(true, forKey: key)
        let alert = NSAlert()
        alert.messageText = "Allow dino in Login Items"
        alert.informativeText = "Your agents and shells run in dino's background service. Until you allow dino in System Settings → General → Login Items, programs in your terminals don't get the permissions you give dino, such as Screen Recording and Accessibility."
        alert.addButton(withTitle: "Open Login Items")
        alert.addButton(withTitle: "Not Now")
        if alert.runModal() == .alertFirstButtonReturn { SMAppService.openSystemSettingsLoginItems() }
    }
}

/// Settings → General: dinod's launch agent, when this build has one.
struct DinodAgentSection: View {
    @EnvironmentObject var model: DinoModel
    @State private var needsApproval = DinodAgent.needsApproval
    @State private var confirming = false

    var body: some View {
        if DinodAgent.bundled != nil {
            Section {
                if needsApproval {
                    LabeledContent("dino isn't allowed in Login Items") {
                        Button("Open Login Items") { SMAppService.openSystemSettingsLoginItems() }
                    }
                } else if model.daemonUnmanaged {
                    LabeledContent("Background service was started outside dino") {
                        Button("Restart Now") { confirming = true }
                            .disabled(model.restartingDaemon)
                    }
                }
            } header: {
                Text("Running in the Background")
            } footer: {
                Footnote(needsApproval
                    ? "Allow dino in Login Items so programs in your terminals get the permissions you give dino in Privacy & Security, such as Screen Recording. Your agents and shells still run without it, but without those permissions."
                    : "Your agents and shells keep running after you quit dino. Programs in your terminals get the permissions you give dino in Privacy & Security, such as Screen Recording.")
            }
            .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
                refresh()
            }
            .onAppear { refresh() }
            .alert("Restart dino's background service?", isPresented: $confirming) {
                Button("Restart") { model.restartDaemon() }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text("Every session restarts. Agents resume their conversations, but a turn in progress is lost. Commands running in shells stop.")
            }
        }
    }

    /// Back from Login Items: allowed now, and Restart to Update offers to move dinod into launchd.
    private func refresh() {
        needsApproval = DinodAgent.needsApproval
        DinodAgent.writeRecord()
        model.checkDaemonVersion()
    }
}
