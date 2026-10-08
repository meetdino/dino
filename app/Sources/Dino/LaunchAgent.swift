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
/// Contents/Helpers, kept by the app's own executable (`DinodHost`, so that its permissions are all
/// dino's), restarted by launchd if it crashes, not at login (dinod starts when dino is
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
        // Not the agent from before `DinodHost` (`<bundle>.dinod`), carried only to be unregistered.
        for url in (try? FileManager.default.contentsOfDirectory(at: dir, includingPropertiesForKeys: nil)) ?? [] where url.pathExtension == "plist" && url.lastPathComponent.contains(".dinod-host") {
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
    /// The plist alone, as registered last (or as launchd was seen running it with this build).
    /// Not set by a dino from before #157: there it's unknown, and the agent's plist hasn't changed
    /// since it was first carried (0.1.6).
    private static let registeredPlistKey = "dinodAgentPlistOnly"

    /// The agent and the build carrying it: an update registers it again when dinod is stopped
    /// (`setUp(stopped: true)`), as does every rebuild of the dino you use (its commit). The build
    /// on disk now: an app still running when a rebuild replaced it registers the new one.
    private static var digest: String? {
        let info = Updates.diskInfo
        let build = (info["CFBundleVersion"] as? String ?? "") + ((info["DinoBuild"] as? String).map { " \($0)" } ?? "")
        return bundled.map { hash($0.data + Data(build.utf8)) }
    }

    private static var plistDigest: String? { bundled.map { hash($0.data) } }

    private static func hash(_ data: Data) -> String {
        SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
    }

    /// Registered by another build. Whether launchd starts this build under it only dinod can say
    /// (`needsRestart`): it does for a Developer ID signature, whose requirement (Team ID and bundle
    /// id) is the same for every build, and needn't for an ad hoc one, whose requirement is its cdhash.
    static var stale: Bool {
        guard enabled, let registered = UserDefaults.standard.string(forKey: registeredKey) else { return false }
        return registered != digest
    }

    /// Whether dinod has to restart (and the agent be registered again) for it to run under this
    /// app's agent (#157). dinod says which agent launchd started it under (`runningUnder`, from the
    /// agent's plist) and whether it's this app's build (`thisBuild`). Both, and launchd has
    /// already started this build under the registration it has, whichever build made that: an
    /// update installed as dino quits (`Updates.quitForUpdate`) starts the new dinod through the
    /// agent before the new app opens. A restart is needed for dinod outside the agent, under
    /// another, or from another build, and for an agent whose plist changed since it was registered.
    nonisolated static func needsRestart(label: String, runningUnder: String?, thisBuild: Bool, stale: Bool,
                                         registeredPlist: String?, plist: String?) -> Bool {
        if runningUnder != label { return true }
        if let registeredPlist, registeredPlist != plist { return true }
        return stale && !thisBuild
    }

    /// For `checkDaemonVersion`: false in a build without an agent, or with it off in Login Items.
    /// dinod under this agent and from this build: the registration counts as this build's from now on.
    static func needsRestart(runningUnder: String?, thisBuild: Bool) -> Bool {
        guard enabled, let bundled else { return false }
        let registeredPlist = UserDefaults.standard.string(forKey: registeredPlistKey)
        let restart = needsRestart(label: bundled.label, runningUnder: runningUnder, thisBuild: thisBuild, stale: stale,
                                   registeredPlist: registeredPlist, plist: plistDigest)
        if !restart, stale { record() }
        return restart
    }

    /// This build's agent, as launchd has it now.
    private static func record() {
        UserDefaults.standard.set(digest, forKey: registeredKey)
        UserDefaults.standard.set(plistDigest, forKey: registeredPlistKey)
    }

    /// At launch (and with dinod stopped, before it starts again): registered, and its state
    /// written for `dino`. `stopped`: dinod isn't running, so registering again can't stop it.
    static func setUp(stopped: Bool = false) {
        guard let service, let digest else { return }
        if stopped { retirePredecessor() }
        let registered = UserDefaults.standard.string(forKey: registeredKey)
        switch service.status {
        case .notRegistered, .notFound:
            register(service)
        case .enabled where registered != nil && registered != digest && stopped:
            // An update: launchd keeps the agent as the old build registered it, which it may not
            // start for this one (an ad hoc signature), until it's registered again.
            try? service.unregister()
            register(service)
        case .enabled where registered == nil:
            record()
        default:
            break
        }
        writeRecord()
    }

    /// The agent from before `DinodHost` (`<bundle>.dinod…`, which ran Contents/Helpers/dino):
    /// launchd keeps an agent's launch constraint from when it was first registered, so the
    /// app's executable couldn't run under it, and this one has a name of its own. The old one is
    /// unregistered (the bundle carries its plist for that) once it isn't running dinod: dinod
    /// stopped, or running under this one. Unregistering it would stop what it runs.
    static func retirePredecessor() {
        guard let bundled else { return }
        let old = SMAppService.agent(plistName: bundled.plist.replacingOccurrences(of: ".dinod-host", with: ".dinod"))
        if old.status == .enabled || old.status == .requiresApproval { try? old.unregister() }
    }

    private static func register(_ service: SMAppService) {
        do {
            try service.register()
            record()
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

/// What dinod's launch agent runs: the app's own executable with `--dinod`, which starts dinod
/// (Contents/Helpers/dino daemon) and waits for it, nothing else (no window, no AppKit). macOS
/// gives what launchd starts the privacy permissions of the program itself: run as
/// Contents/Helpers/dino, dinod and everything in its terminals got dino's Screen Recording but
/// asked for Accessibility as a "dino" executable of their own, which the app couldn't ask for
/// and which Privacy & Security lists apart. Under the app's executable they're all dino's.
///
/// Signals launchd sends go on to dinod, and its end is this process's: an exit with its status,
/// or the same signal, so launchd still tells a crash (restarted) from `dino stop` (not).
enum DinodHost {
    static let argument = "--dinod"

    /// dinod, for the signal handlers (which can't capture anything).
    nonisolated(unsafe) private static var child: pid_t = 0

    static func runIfAsked() {
        guard CommandLine.arguments.dropFirst().first == argument else { return }
        exit(run())
    }

    private static func run() -> Int32 {
        let dino = Bundle.main.bundleURL.appendingPathComponent("Contents/Helpers/dino").path
        let argv: [UnsafeMutablePointer<CChar>?] = [strdup("dino"), strdup("daemon"), nil]
        var pid: pid_t = 0
        let spawned = posix_spawn(&pid, dino, nil, nil, argv, environ)
        guard spawned == 0 else {
            FileHandle.standardError.write(Data("dino: couldn't start dinod at \(dino): \(String(cString: strerror(spawned)))\n".utf8))
            return 1
        }
        child = pid
        for sig in [SIGTERM, SIGINT, SIGHUP, SIGQUIT] {
            signal(sig) { sig in if DinodHost.child > 0 { kill(DinodHost.child, sig) } }
        }
        var status: Int32 = 0
        while waitpid(pid, &status, 0) == -1 && errno == EINTR {}
        // WIFSIGNALED / WTERMSIG / WEXITSTATUS, which Swift doesn't import.
        let sig = status & 0x7f
        if sig != 0 && sig != 0x7f {
            signal(sig, SIG_DFL)
            kill(getpid(), sig)
        }
        return (status >> 8) & 0xff
    }
}
