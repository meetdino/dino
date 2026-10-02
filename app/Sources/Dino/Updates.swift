import Sparkle
import SwiftUI

/// Updating dino. The app updates through Sparkle, from the feed and key scripts/release.sh puts
/// in Info.plist; the `dino` it carries (and the command-line link to it) comes along. dinod keeps
/// running the dino it started with until the app restarts it into the new one, which waits until
/// no agent is working and no shell is running a command, since restarting stops and resumes them.
@MainActor
final class Updates: ObservableObject {
    static let shared = Updates()

    /// Nil in a build without the release key (development builds): nothing to check against.
    let controller: SPUStandardUpdaterController?
    @Published private(set) var canCheck = false
    private var watching: NSKeyValueObservation?

    private init() {
        let key = Bundle.main.object(forInfoDictionaryKey: "SUPublicEDKey") as? String
        controller = (key?.isEmpty == false) ? SPUStandardUpdaterController(startingUpdater: true, updaterDelegate: nil, userDriverDelegate: nil) : nil
        watching = controller?.updater.observe(\.canCheckForUpdates, options: [.initial, .new]) { updater, _ in
            let can = updater.canCheckForUpdates
            Task { @MainActor in Updates.shared.canCheck = can }
        }
    }

    var available: Bool { controller != nil }

    var automatic: Bool {
        get { controller?.updater.automaticallyChecksForUpdates ?? false }
        set {
            objectWillChange.send()
            controller?.updater.automaticallyChecksForUpdates = newValue
        }
    }

    func checkNow() { controller?.checkForUpdates(nil) }

    /// The dino this app carries, as `dino --version` says it; nil in a development build.
    nonisolated static let bundledVersion: String? = {
        guard let bin = DinoEnvironment.bundledDino else { return nil }
        let p = Process()
        p.executableURL = URL(fileURLWithPath: bin)
        p.arguments = ["--version"]
        let out = Pipe()
        p.standardOutput = out
        p.standardError = FileHandle.nullDevice
        guard (try? p.run()) != nil else { return nil }
        p.waitUntilExit()
        let text = String(data: out.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
        return text.split(separator: " ").last.map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
    }()
}

extension DinoModel {
    /// After connecting: is dinod the dino this app carries? A dinod from before an update (or
    /// too old to say) is marked, and restarted once nothing is busy.
    func checkDaemonVersion() {
        guard let want = Updates.bundledVersion, let conn = connection else { return }
        Task.detached {
            let running = (try? conn.request(["type": "version"]))?.dino
            await MainActor.run {
                self.daemonVersion = running ?? "an older dino"
                self.daemonOutdated = running != want
            }
        }
    }

    /// Nothing that restarting dinod would cut off: no agent working, waiting on background work
    /// or asking for something, and no shell running a command.
    var restartIsQuiet: Bool {
        sessions.allSatisfy { s in
            if s.exited { return true }
            if s.agent_id == "shell", s.running == true { return false }
            return ![.thinking, .working, .waiting, .needsYou].contains(status(of: s))
        }
    }

    /// Restart dinod into the dino this app carries: sessions are saved, stopped and resumed.
    func restartDaemon() {
        guard !restartingDaemon else { return }
        restartingDaemon = true
        Task.detached {
            _ = try? DinoConnection(path: DinoEnvironment.socketPath).request(["type": "shutdown"])
            // Until the old one has let go of the socket, `ping` would find it.
            for _ in 0..<50 where (try? DinoConnection(path: DinoEnvironment.socketPath)) != nil {
                try? await Task.sleep(for: .milliseconds(100))
            }
            try? DinoEnvironment.ensureDaemon()
            await MainActor.run {
                self.restartingDaemon = false
                self.daemonOutdated = false
            }
        }
    }
}

/// Settings → General → Updates.
struct UpdatesSection: View {
    @EnvironmentObject var store: SettingsStore
    @EnvironmentObject var model: DinoModel
    @ObservedObject private var updates = Updates.shared
    @State private var confirming = false

    var body: some View {
        Section {
            Toggle("Check for updates automatically", isOn: Binding(
                get: { updates.available ? updates.automatic : (store.settings?.machine.check_updates ?? true) },
                set: { on in
                    updates.automatic = on
                    store.update { $0.machine.check_updates = on }
                }
            ))
            .disabled(store.settings == nil)
            LabeledContent("dino \(Updates.bundledVersion ?? (Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? ""))") {
                Button("Check Now") { updates.checkNow() }
                    .disabled(!updates.available || !updates.canCheck)
            }
            if model.daemonOutdated {
                LabeledContent("dinod is still \(model.daemonVersion ?? "an older dino")") {
                    Button("Restart Now") { confirming = true }
                        .disabled(model.restartingDaemon)
                }
            }
        } header: {
            Text("Updates")
        } footer: {
            Footnote(updates.available
                ? "Once a day. The dino command comes along with the app. After an update, dinod restarts into the new dino once no agent is working and no shell is running a command; your sessions resume."
                : "This build doesn't update itself (it isn't a release).")
        }
        .alert("Restart dinod now?", isPresented: $confirming) {
            Button("Restart") { model.restartDaemon() }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Every session stops and resumes; an agent in the middle of a turn loses that turn, and commands running in shells stop.")
        }
    }
}
