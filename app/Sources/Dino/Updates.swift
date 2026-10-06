import Sparkle
import SwiftUI

/// Updating dino. The app updates through Sparkle, from the feed and key scripts/release.sh puts
/// in Info.plist; the `dino` it carries (and the command-line link to it) comes along.
///
/// One thing at a time, the update first: on a Mac that hasn't seen the Welcome card, dino looks
/// for an update before showing it (a new install of an old version updates before you set it up),
/// and the card waits while one is offered, downloading or installing; it comes up once there's
/// none, you decline it, or the feed hasn't answered in a few seconds. While the card is up, the
/// daily check waits, and runs once it's closed.
///
/// Installing restarts dinod too, into the dino the new app carries: the app stops it as it quits
/// for the update (every session saved) and the new one starts it, which resumes them. Before that,
/// if anything would be cut off, dino says what (`RestartImpact`) and lets you choose: now, once
/// nothing is working, or when you next quit dino.
///
/// Nothing restarts by itself. A dino rebuilt while it runs (app/build.sh --install) offers Restart
/// to Update the same way: the new build waits beside this one (`staged`) and goes in its place
/// once this app and dinod have stopped, and dino opens again from it; so does a
/// dinod from another build than this app's (an older dino quit without stopping it), restarting
/// dinod alone.
@MainActor
final class Updates: ObservableObject {
    static let shared = Updates()

    /// Nil in a build without the release key (development builds): nothing to check against.
    let controller: SPUStandardUpdaterController?
    private let delegate = UpdaterDelegate()
    @Published private(set) var canCheck = false
    private var watching: NSKeyValueObservation?
    /// For what a restart would cut off; set with the app delegate's.
    weak var model: DinoModel?

    /// An update downloaded and set to install, waiting: for the moment nothing is working
    /// (`whenIdle`), or for dino to quit. `install` installs it now and relaunches dino.
    struct Pending {
        var version: String
        var whenIdle: Bool
        var install: (() -> Void)?
        /// Not a download: this app rebuilt on disk (relaunch into it), or dinod from another build
        /// than this app's (restart dinod alone).
        var local: Local?
    }
    enum Local { case rebuilt, daemon }
    @Published private(set) var pending: Pending?
    /// dino is quitting so an update can install and relaunch it.
    private(set) var relaunching = false

    /// Before the Welcome card: looking for an update, or one is being offered or installed.
    @Published private(set) var holdingWelcome = false
    /// The look before the Welcome card happens once a launch.
    private var lookedBeforeWelcome = false
    /// The look found an update: offered once the look's own cycle has ended.
    private var foundBeforeWelcome = false
    /// Sparkle's window for the update found before the card is up: the card waits for it.
    private var offeringBeforeWelcome = false

    /// The Welcome card is about to come up: look for an update first, and hold the card until
    /// there's none, it's declined, or the feed is slow to answer.
    func beforeWelcome() {
        guard !lookedBeforeWelcome else { return }
        lookedBeforeWelcome = true
        guard let updater = controller?.updater else { return }
        holdingWelcome = true
        // The daily check would only offer the same update again after the card.
        deferredCheck = false
        probe(updater, tries: 10)
        Task {
            try? await Task.sleep(for: .seconds(8))
            // No answer yet (offline, a slow network): the card doesn't wait for it.
            if !foundBeforeWelcome, !offeringBeforeWelcome { holdingWelcome = false }
        }
    }

    /// Sparkle looks once at a time: one already looking (its daily check, just turned away)
    /// finishes first.
    private func probe(_ updater: SPUUpdater, tries: Int) {
        guard updater.sessionInProgress else { return updater.checkForUpdateInformation() }
        guard tries > 0 else { return }
        Task {
            try? await Task.sleep(for: .milliseconds(300))
            probe(updater, tries: tries - 1)
        }
    }

    /// The Welcome card is up (true), closed or not coming (false), or dino doesn't know yet (nil).
    var welcomeUp: Bool? {
        didSet {
            guard welcomeUp == false, deferredCheck else { return }
            deferredCheck = false
            controller?.updater.checkForUpdatesInBackground()
        }
    }
    /// The daily check came while the Welcome card was up (or might come): it runs once it's closed.
    private var deferredCheck = false

    private init() {
        let key = Bundle.main.object(forInfoDictionaryKey: "SUPublicEDKey") as? String
        controller = (key?.isEmpty == false) ? SPUStandardUpdaterController(startingUpdater: true, updaterDelegate: delegate, userDriverDelegate: nil) : nil
        watching = controller?.updater.observe(\.canCheckForUpdates, options: [.initial, .new]) { updater, _ in
            let can = updater.canCheckForUpdates
            Task { @MainActor in Updates.shared.canCheck = can }
        }
        watchFolder()
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

    /// Install the waiting update now: dino quits, the update installs, dino opens again. A build
    /// already here asks first if anything would be cut off, unless it was waiting for quiet.
    func installNow() {
        guard let pending else { return }
        if let local = pending.local { return restart(local, ask: !pending.whenIdle) }
        self.pending = nil
        if let install = pending.install { install() } else { checkNow() }
    }

    // MARK: Builds already on this Mac

    /// A newer build of this app is waiting beside it (the rebuild hook staged it), or is in its
    /// place already: Restart to Update. Looked at when dino comes forward and when its folder changes.
    func checkRebuilt() {
        let info = NSDictionary(contentsOf: Updates.staged.appendingPathComponent("Contents/Info.plist")) ?? Updates.diskInfo
        guard pending?.local != .rebuilt, let running = Updates.bundledBuild,
              let onDisk = info["DinoBuild"] as? String, onDisk != running else { return }
        pending = Pending(version: onDisk, whenIdle: false, install: nil, local: .rebuilt)
    }

    /// dinod is from another build than this app's, or runs outside its launch agent: Restart to
    /// Update restarts it into this app's (`checkDaemonVersion`), unless something else waits.
    func daemonMismatch(_ mismatch: Bool, version: String) {
        if mismatch, pending == nil {
            pending = Pending(version: version, whenIdle: false, install: nil, local: .daemon)
        } else if !mismatch, pending?.local == .daemon {
            pending = nil
        }
    }

    private func restart(_ local: Local, ask: Bool) {
        guard let model else { return }
        if ask, !model.daemonDown, !model.restartIsQuiet {
            switch askToRestart("Restart dino to update?", impact: RestartImpact(model)) {
            case .now: break
            case .whenIdle:
                pending?.whenIdle = true
                return
            case .later: return
            }
        }
        pending = nil
        switch local {
        case .daemon:
            model.restartDaemon()
        case .rebuilt:
            // Quitting stops dinod (`quitForUpdate`); the new build opens and starts its own.
            relaunching = true
            Self.reopenAfterQuit()
            NSApp.terminate(nil)
        }
    }

    /// The app opens again from where it is once this one has quit, the build waiting beside it in
    /// its place: opened while this one runs, LaunchServices would bring this one forward instead.
    /// In front only if this one was: a restart once agents are idle doesn't take over what you're
    /// doing elsewhere.
    private static func reopenAfterQuit() {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/bin/sh")
        p.arguments = ["-c", "while /bin/kill -0 \"$1\" 2>/dev/null; do sleep 0.1; done; \(installStaged); exec /usr/bin/open $3 \"$2\"", "dino-reopen",
                       String(getpid()), Bundle.main.bundlePath, NSApp.isActive ? "" : "-g"]
        p.standardOutput = FileHandle.nullDevice
        p.standardError = FileHandle.nullDevice
        try? p.run()
    }

    /// Where app/build.sh --install leaves a new build while this one runs: in a hidden folder
    /// beside it, under the same name. macOS knows dino, dinod and the programs in its terminals
    /// by this app at its path, so it stays there until they've stopped.
    nonisolated static var staged: URL {
        let app = Bundle.main.bundleURL
        return app.deletingLastPathComponent().appendingPathComponent(".\(app.lastPathComponent).next")
            .appendingPathComponent(app.lastPathComponent)
    }

    /// Shell: the build waiting at `staged` put in place of the app at "$2" (this one, quit).
    private static let installStaged = """
        next="$(dirname "$2")/.$(basename "$2").next"; old="$(dirname "$2")/.$(basename "$2").old.$$"
        if [ -d "$next/$(basename "$2")" ] && mv "$2" "$old"; then
            if mv "$next/$(basename "$2")" "$2"; then rm -rf "$old" "$next"; else mv "$old" "$2"; fi
            /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$2" 2>/dev/null
        fi
        """

    /// The folder the app is in, watched for the rebuild putting a new one in its place.
    private var folderWatch: DispatchSourceFileSystemObject?

    private func watchFolder() {
        guard Updates.bundledBuild != nil else { return }
        let fd = open(Bundle.main.bundleURL.deletingLastPathComponent().path, O_EVTONLY)
        guard fd >= 0 else { return }
        let source = DispatchSource.makeFileSystemObjectSource(fileDescriptor: fd, eventMask: .write, queue: .main)
        source.setEventHandler { MainActor.assumeIsolated { Updates.shared.checkRebuilt() } }
        source.setCancelHandler { close(fd) }
        source.resume()
        folderWatch = source
    }

    /// The app's Info.plist as it is on disk now, not as it was when this one started.
    nonisolated static var diskInfo: NSDictionary {
        NSDictionary(contentsOf: Bundle.main.bundleURL.appendingPathComponent("Contents/Info.plist")) ?? [:]
    }

    private enum Choice { case now, whenIdle, later }

    /// What restarting would cut off, said plainly, and the choice: now, once nothing is working,
    /// or later (when dino next quits).
    private func askToRestart(_ title: String, impact: RestartImpact) -> Choice {
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = impact.sentence + "\n\n"
            + "You can also install it once nothing is working, or the next time you quit dino."
        alert.addButton(withTitle: "Restart Now")
        alert.addButton(withTitle: impact.working > 0 ? "When Agents Are Idle" : "When Commands Finish")
        alert.addButton(withTitle: "Later")
        NSApp.activate(ignoringOtherApps: true)
        switch alert.runModal() {
        case .alertFirstButtonReturn: return .now
        case .alertSecondButtonReturn: return .whenIdle
        default: return .later
        }
    }

    // MARK: Sparkle's questions (through UpdaterDelegate)

    fileprivate func mayCheck(_ check: SPUUpdateCheck) -> Bool {
        // Asked for (Check for Updates…): always. The daily one: not over the Welcome card.
        guard check == .updatesInBackground, welcomeUp != false else { return true }
        deferredCheck = true
        return false
    }

    /// "Install and Relaunch" was clicked: with nothing to cut off, go ahead; otherwise say what
    /// restarting would interrupt, and install now, once it's quiet, or when dino quits.
    fileprivate func shouldPostpone(_ item: SUAppcastItem, install: @escaping () -> Void) -> Bool {
        guard let model, !model.daemonDown, !model.restartIsQuiet else { return false }
        switch askToRestart("Restart dino to install \(item.displayVersionString)?", impact: RestartImpact(model)) {
        case .now:
            return false
        case .whenIdle:
            pending = Pending(version: item.displayVersionString, whenIdle: true, install: install)
        case .later:
            pending = Pending(version: item.displayVersionString, whenIdle: false, install: install)
        }
        // Sparkle's "Ready to Install" window would stay up, its button doing nothing: dino has it now.
        controller?.userDriver.dismissUpdateInstallation()
        return true
    }

    fileprivate func willRelaunch() { relaunching = true }

    fileprivate func found(_ item: SUAppcastItem) {
        if holdingWelcome { foundBeforeWelcome = true }
    }

    /// Sparkle's window for the update the look found, once the look's session has ended (it
    /// ends just after saying so). Not offered within a few seconds: the card doesn't wait.
    private func offer(tries: Int) {
        guard let controller else { return }
        guard controller.updater.sessionInProgress else {
            NSApp.activate(ignoringOtherApps: true)
            return controller.checkForUpdates(nil)
        }
        guard tries > 0 else {
            holdingWelcome = false
            return
        }
        Task {
            try? await Task.sleep(for: .milliseconds(150))
            offer(tries: tries - 1)
        }
    }

    /// A look or an offer has ended. The look before the card found an update: offer it now, the
    /// card still waiting. The offer ended without relaunching (declined, later, failed): the card.
    fileprivate func cycleEnded(_ check: SPUUpdateCheck) {
        guard holdingWelcome else { return }
        if check == .updateInformation, foundBeforeWelcome, !offeringBeforeWelcome {
            offeringBeforeWelcome = true
            offer(tries: 20)
        } else if check == .updateInformation, foundBeforeWelcome {
            return
        } else if !relaunching {
            holdingWelcome = false
        }
    }

    /// Downloaded on its own (automatic updates): it installs when dino quits, or from the menu.
    fileprivate func willInstallOnQuit(_ item: SUAppcastItem, install: @escaping () -> Void) -> Bool {
        pending = Pending(version: item.displayVersionString, whenIdle: false, install: install)
        return true
    }

    /// Sparkle's own "install on quit": its window dismissed with the update ready.
    fileprivate func dismissed(_ item: SUAppcastItem, _ state: SPUUserUpdateState) {
        guard state.stage == .installing, pending == nil else { return }
        pending = Pending(version: item.displayVersionString, whenIdle: false, install: nil)
    }

    /// With an update waiting for quiet: now's the time.
    func sessionsChanged(quiet: Bool) {
        guard quiet, pending?.whenIdle == true else { return }
        installNow()
    }

    // MARK: Quitting

    /// dino is quitting and the update installs as it does: dinod restarts too, into the new dino.
    /// Nil when there's no update to install; otherwise whether to quit.
    func quitForUpdate(_ model: DinoModel) -> Bool? {
        if relaunching {
            // Confirmed already (or nothing was running): the new app starts dinod again.
            if !model.daemonDown { Self.stopDaemon(model) }
            return true
        }
        // dinod from another build stays as it is: quitting doesn't stop it otherwise either.
        guard let pending, pending.local != .daemon else { return nil }
        if !QuitChoice.systemIsGoingDown, !model.daemonDown, !model.restartIsQuiet {
            let alert = NSAlert()
            alert.messageText = "Quit and install dino \(pending.version)?"
            alert.informativeText = RestartImpact(model).sentence
            alert.addButton(withTitle: "Install and Quit")
            alert.addButton(withTitle: "Cancel")
            guard alert.runModal() == .alertFirstButtonReturn else { return false }
        }
        let running = !model.daemonDown
        if running { Self.stopDaemon(model) }
        // Rebuilt, the new app is beside this one: put in place, no waiting for it.
        if running || pending.local == .rebuilt {
            Self.startDaemonAfterInstall(waiting: pending.local == nil, start: running)
        }
        return true
    }

    /// Every session saved and stopped, and dinod gone (a few seconds at most), so the new dino
    /// finds it stopped and can register its launch agent again before starting it.
    private static func stopDaemon(_ model: DinoModel) {
        model.stopDaemon()
        for _ in 0..<50 where (try? DinoConnection(path: DinoEnvironment.socketPath)) != nil {
            usleep(100_000)
        }
    }

    /// Once this app has quit and Sparkle (or this, for a rebuild) has put the new one in its
    /// place, start dinod from it, which resumes the sessions: they don't wait for dino to be
    /// opened again. Gives up waiting for the new app after two minutes (the update failed) and
    /// starts the one that's there. `start` false: dinod wasn't running, and stays stopped.
    private static func startDaemonAfterInstall(waiting: Bool, start: Bool) {
        let bundle = Bundle.main.bundlePath
        let build = waiting ? Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? "" : ""
        let script = """
        while /bin/kill -0 "$1" 2>/dev/null; do sleep 0.2; done
        \(installStaged)
        [ -n "$4" ] || exit 0
        i=0
        while [ -n "$3" ] && [ $i -lt 600 ]; do
            v=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$2/Contents/Info.plist" 2>/dev/null)
            [ -n "$v" ] && [ "$v" != "$3" ] && [ -x "$2/Contents/Helpers/dino" ] && break
            sleep 0.2; i=$((i+1))
        done
        exec "$2/Contents/Helpers/dino" ping
        """
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/bin/sh")
        p.arguments = ["-c", script, "dino-update", String(getpid()), bundle, build, start ? "start" : ""]
        var env = ProcessInfo.processInfo.environment
        env["PATH"] = DinoEnvironment.loginPath
        env["DINO_HOME"] = DinoEnvironment.home
        p.environment = env
        p.standardOutput = FileHandle.nullDevice
        p.standardError = FileHandle.nullDevice
        try? p.run()
    }

    /// The dino this app carries, as `dino --version` says it ("dino 0.2.0 (c1ddaea2f)": the
    /// version); nil in a build without one (app/build.sh without --install).
    /// It waits for `dino` without `waitUntilExit`: that runs the run loop, and code it runs on
    /// the way (a SwiftUI update) reaching this again while it's being set crashed the app.
    nonisolated static let bundledVersion: String? = {
        guard let bin = DinoEnvironment.bundledDino else { return nil }
        let p = Process()
        p.executableURL = URL(fileURLWithPath: bin)
        p.arguments = ["--version"]
        let out = Pipe()
        p.standardOutput = out
        p.standardError = FileHandle.nullDevice
        let exited = DispatchSemaphore(value: 0)
        p.terminationHandler = { _ in exited.signal() }
        guard (try? p.run()) != nil else { return nil }
        let said = out.fileHandleForReading.readDataToEndOfFile()
        exited.wait()
        let text = String(data: said, encoding: .utf8) ?? ""
        let words = text.split(whereSeparator: \.isWhitespace)
        return words.count > 1 ? String(words[1]) : nil
    }()

    /// The build of dino this app carries: the commit it was built from (`DinoBuild`, from
    /// app/build.sh and scripts/release.sh), as its dinod says it with `build`.
    nonisolated static let bundledBuild = Bundle.main.object(forInfoDictionaryKey: "DinoBuild") as? String

    /// The binary at `path` with every link resolved, as dinod says which it runs from.
    nonisolated static func realPath(_ path: String) -> String {
        guard let resolved = realpath(path, nil) else { return path }
        defer { free(resolved) }
        return String(cString: resolved)
    }

    /// dinod isn't the dino this app carries: another build (a rebuilt dino you use, a release
    /// beside it, an update), or another binary. The build is the one on disk now, which a restart
    /// starts: an app still running when its rebuild replaced it doesn't take the new dinod for an
    /// old one. An older dinod says only its version, and an app from before builds were told
    /// apart has only that to go by.
    nonisolated static func fromAnotherBuild(version: String?, build: String?, exe: String?) -> Bool {
        guard let own = DinoEnvironment.bundledDino else { return false }
        if let exe, realPath(exe) != realPath(own) { return true }
        if let current = diskInfo["DinoBuild"] as? String ?? bundledBuild {
            return build != current
        }
        return version != bundledVersion
    }
}

/// Sparkle's delegate, handing each question to `Updates`.
private final class UpdaterDelegate: NSObject, SPUUpdaterDelegate {
    func updater(_: SPUUpdater, mayPerform updateCheck: SPUUpdateCheck) throws {
        guard Updates.shared.mayCheck(updateCheck) else {
            throw NSError(domain: "dino", code: 1, userInfo: [NSLocalizedDescriptionKey: "Waiting for the Welcome card to close"])
        }
    }

    func updater(_: SPUUpdater, shouldPostponeRelaunchForUpdate item: SUAppcastItem, untilInvokingBlock installHandler: @escaping () -> Void) -> Bool {
        Updates.shared.shouldPostpone(item, install: installHandler)
    }

    func updater(_: SPUUpdater, didFindValidUpdate item: SUAppcastItem) {
        Updates.shared.found(item)
    }

    func updater(_: SPUUpdater, didFinishUpdateCycleFor updateCheck: SPUUpdateCheck, error _: (any Error)?) {
        Updates.shared.cycleEnded(updateCheck)
    }

    func updaterWillRelaunchApplication(_: SPUUpdater) {
        Updates.shared.willRelaunch()
    }

    func updater(_: SPUUpdater, willInstallUpdateOnQuit item: SUAppcastItem, immediateInstallationBlock immediateInstallHandler: @escaping () -> Void) -> Bool {
        Updates.shared.willInstallOnQuit(item, install: immediateInstallHandler)
    }

    func updater(_: SPUUpdater, userDidMake choice: SPUUserUpdateChoice, forUpdate updateItem: SUAppcastItem, state: SPUUserUpdateState) {
        if choice == .dismiss { Updates.shared.dismissed(updateItem, state) }
    }
}

/// What restarting dinod would do to the sessions, said plainly: agents resume their
/// conversations (one in the middle of a turn loses that turn); shells start again, fresh, in the
/// folder they were in, so a command running in one stops, as does an agent typed into one.
struct RestartImpact {
    var working = 0
    var idle = 0
    var shells = 0
    var busyShells = 0

    @MainActor init(_ model: DinoModel) {
        for s in model.sessions where !s.exited {
            let busy = [.thinking, .working, .waiting, .needsYou].contains(model.status(of: s))
            if s.agent_id == "shell" {
                shells += 1
                if s.running == true || s.inside != nil { busyShells += 1 }
            } else if busy {
                working += 1
            } else {
                idle += 1
            }
        }
    }

    init(working: Int, idle: Int, shells: Int, busyShells: Int) {
        (self.working, self.idle, self.shells, self.busyShells) = (working, idle, shells, busyShells)
    }

    var sentence: String {
        var parts = ["Installing restarts all your sessions."]
        let agents = working + idle
        if working > 0 {
            parts.append("\(count(working, "agent")) \(working == 1 ? "is" : "are") working and will be interrupted; "
                + (agents == 1 ? "it picks up its conversation afterwards."
                    : working == agents ? "they pick up their conversations afterwards."
                    : "all \(agents) agents pick up their conversations afterwards."))
        } else if agents > 0 {
            parts.append(agents == 1 ? "Your agent picks up its conversation afterwards." : "Your \(agents) agents pick up their conversations afterwards.")
        }
        if shells > 0 {
            parts.append("\(count(shells, "shell")) will start fresh in the same \(shells == 1 ? "folder" : "folders")"
                + (busyShells == 0 ? "." : busyShells == shells && shells == 1 ? ", stopping what's running in it."
                    : "; what's running in \(busyShells) of them stops."))
        }
        return parts.joined(separator: " ")
    }

    private func count(_ n: Int, _ word: String) -> String { "\(n) \(word)\(n == 1 ? "" : "s")" }
}

/// Above the terminals while an update waits for quiet (it installs by itself then, or now), and
/// while a build already on this Mac waits for Restart to Update.
struct UpdateBanner: View {
    @ObservedObject private var updates = Updates.shared

    var body: some View {
        if let pending = updates.pending, pending.whenIdle || pending.local != nil {
            HStack(spacing: 10) {
                Image(systemName: "arrow.down.circle")
                    .foregroundStyle(.secondary)
                    .accessibilityHidden(true)
                Text(pending.local == nil ? "dino \(pending.version) will install once nothing is working"
                    : pending.whenIdle ? "dino will restart into the new build once nothing is working"
                    : pending.local == .rebuilt ? "A new build of dino is ready" : "dino's background service needs a restart to finish updating")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                Spacer(minLength: 8)
                Button(pending.whenIdle ? "Restart Now" : "Restart to Update") { updates.installNow() }
                    .controlSize(.small)
                    .help("Restarts dino now. Working agents are interrupted and resume afterwards.")
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 5)
            .overlay(alignment: .bottom) { Divider() }
        }
    }
}

/// The app menu's update item: a check, or the waiting update.
struct UpdateMenuItem: View {
    @ObservedObject private var updates = Updates.shared

    var body: some View {
        if let pending = updates.pending, pending.local != nil {
            Button("Restart to Update") { updates.installNow() }
        } else if let pending = updates.pending {
            Button("Restart to Install dino \(pending.version)") { updates.installNow() }
        } else {
            Button("Check for Updates…") { updates.checkNow() }
                .disabled(!updates.available)
        }
    }
}

extension DinoModel {
    /// After connecting: is dinod the dino this app carries? A dinod from before an update, from
    /// another build (the dino you use, rebuilt; a release beside it) or too old to say is marked,
    /// and Restart to Update offers to restart it.
    func checkDaemonVersion() {
        guard DinoEnvironment.bundledDino != nil, let conn = connection else { return }
        Task.detached {
            let reply = try? conn.request(["type": "version"])
            let running = reply?.dino
            let outdated = Updates.fromAnotherBuild(version: running, build: reply?.build, exe: reply?.exe)
            let unmanaged = DinodAgent.enabled && (reply?.launchd != DinodAgent.bundled?.label || DinodAgent.stale)
            if reply?.launchd != nil, reply?.launchd == DinodAgent.bundled?.label { DinodAgent.retirePredecessor() }
            await MainActor.run {
                self.daemonVersion = running.map { v in reply?.build.map { "\(v) (\($0))" } ?? v } ?? "an older version"
                self.daemonOutdated = outdated
                self.daemonUnmanaged = unmanaged
                Updates.shared.daemonMismatch(outdated || unmanaged, version: Updates.bundledBuild ?? Updates.bundledVersion ?? "")
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
            // Stopped: the agent registered again if an update changed it, then started through it.
            DinodAgent.setUp(stopped: true)
            try? DinoEnvironment.ensureDaemon()
            await MainActor.run {
                self.restartingDaemon = false
                self.daemonOutdated = false
                self.daemonUnmanaged = false
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
            // A build that isn't a release says which commit it is.
            // The app's version, which app/build.sh and scripts/release.sh set to its dino's: not
            // `bundledVersion`, which runs `dino` and has no place in a view's body.
            LabeledContent("dino \(Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "")"
                + (updates.available ? "" : Updates.bundledBuild.map { " (\($0))" } ?? "")) {
                Button("Check Now") { updates.checkNow() }
                    .disabled(!updates.available || !updates.canCheck)
            }
            if model.daemonOutdated {
                LabeledContent("Background service is still on \(model.daemonVersion ?? "an older version")") {
                    Button("Restart Now") { confirming = true }
                        .disabled(model.restartingDaemon)
                }
            }
        } header: {
            Text("Updates")
        } footer: {
            Footnote(updates.available
                ? "dino checks once a day. Updates include the dino command-line tool. Installing an update restarts your sessions: agents resume their conversations, and shells start fresh. If anything is working, dino asks first."
                : "This build isn't a release, so it doesn't update itself.")
        }
        .alert("Restart dino's background service?", isPresented: $confirming) {
            Button("Restart") { model.restartDaemon() }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Every session restarts. Agents resume their conversations, but a turn in progress is lost. Commands running in shells stop.")
        }
    }
}
