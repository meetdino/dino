import AppKit
import GhosttyTerminal
import SwiftUI
import UserNotifications

enum SessionStatus {
    case thinking, working, idle, done, needsYou, exited

    var label: String {
        switch self {
        case .thinking: "thinking"
        case .working: "working"
        case .idle: "idle"
        case .done: "done"
        case .needsYou: "needs you"
        case .exited: "exited"
        }
    }

    var color: Color {
        switch self {
        case .thinking: Color(red: 0.78, green: 0.52, blue: 1.0)
        case .working: Brand.green
        case .idle: .secondary
        case .done: Color(red: 0.45, green: 0.82, blue: 0.95)
        case .needsYou: Color(red: 1.0, green: 0.78, blue: 0.2)
        case .exited: Color(red: 0.95, green: 0.35, blue: 0.3)
        }
    }
}

enum Brand {
    static let green = Color(red: 0x75 / 255, green: 0xB3 / 255, blue: 0x40 / 255)
    static let spike = Color(red: 0xFC / 255, green: 0x4F / 255, blue: 0x26 / 255)
}

@MainActor
final class DinoModel: ObservableObject {
    @Published var sessions: [SessionInfo] = []
    @Published var quotas: [QuotaInfo] = []
    @Published var launchers: [LauncherInfo] = []
    @Published var selected: String?
    @Published var error: String?
    /// Agent sessions dino didn't start (running elsewhere, recent, cloud).
    @Published var found: [FoundSession] = []
    @Published var loadingCloud = false
    /// A handoff in progress: the session being moved, and whether we're waiting on its turn.
    @Published var moving: FoundSession?
    @Published var showContinue = false
    /// A handoff waiting for the user's confirmation.
    @Published var confirmMove: FoundSession?

    var elsewhere: [FoundSession] { found.filter { $0.source == "running" } }

    /// Where new sessions start.
    @Published var folder: URL = FileManager.default.homeDirectoryForCurrentUser

    /// Rang the bell or finished while in the background; cleared when selected.
    @Published private(set) var attention: Set<String> = []
    @Published private(set) var unseenDone: Set<String> = []

    /// One live Ghostty surface per session, kept mounted so switching is instant.
    private(set) var terminals: [String: TerminalViewState] = [:]
    private var connection: DinoConnection?
    private var polling = false

    func start() {
        Task.detached {
            do {
                try DinoEnvironment.ensureDaemon()
                let conn = try DinoConnection(path: DinoEnvironment.socketPath)
                let launchers = try conn.request(["type": "launchers"]).launchers ?? []
                await MainActor.run {
                    self.connection = conn
                    self.launchers = launchers
                    self.polling = true
                    self.poll()
                    self.watchElsewhere()
                }
            } catch {
                await MainActor.run { self.error = error.localizedDescription }
            }
        }
    }

    @Published private(set) var daemonDown = false

    private func poll() {
        guard polling, let conn = connection else { return }
        Task.detached {
            let resp = try? conn.request(["type": "state"])
            await MainActor.run {
                if let resp {
                    self.apply(resp.sessions ?? [], resp.quotas ?? [])
                    DispatchQueue.main.asyncAfter(deadline: .now() + 0.25) { self.poll() }
                } else {
                    self.lostDaemon()
                }
            }
        }
    }

    /// dinod stopped: drop dead panes and wait for it to come back (without restarting it ourselves).
    private func lostDaemon() {
        connection = nil
        polling = false
        daemonDown = true
        terminals.removeAll()
        sessions = []
        reconnect()
    }

    private func reconnect() {
        Task.detached {
            if let conn = try? DinoConnection(path: DinoEnvironment.socketPath) {
                await MainActor.run {
                    self.connection = conn
                    self.daemonDown = false
                    self.polling = true
                    self.poll()
                }
            } else {
                try? await Task.sleep(for: .seconds(1))
                await MainActor.run { self.reconnect() }
            }
        }
    }

    /// The "Start dinod" button.
    func startDaemon() {
        Task.detached { try? DinoEnvironment.ensureDaemon() }
    }

    private func apply(_ next: [SessionInfo], _ quotas: [QuotaInfo]) {
        let appActive = NSApp.isActive
        for s in next {
            guard let prev = sessions.first(where: { $0.id == s.id }) else { continue }
            let looking = appActive && s.id == selected
            if !looking {
                if s.bells > prev.bells { attention.insert(s.id) }
                let wasBusy = prev.activity == "working" || prev.needs != nil
                if s.activity == "done", wasBusy {
                    unseenDone.insert(s.id)
                    Notifier.post(session: s, title: "\(s.name) finished", body: s.title ?? "Ready for your review")
                }
                if let needs = s.needs, prev.needs == nil {
                    Notifier.post(session: s, title: "\(s.name) needs you", body: needs)
                }
            }
        }
        if next != sessions { sessions = next }
        if quotas != self.quotas { self.quotas = quotas }
        let live = Set(next.map(\.id))
        terminals = terminals.filter { live.contains($0.key) }
        if let want = pendingSelect, live.contains(want) {
            pendingSelect = nil
            select(want)
        }
        if selected == nil || !live.contains(selected!) {
            // Through select(), so the terminal also takes keyboard focus on launch.
            select(next.first?.id)
        }
        let waiting = next.filter { status(of: $0) == .needsYou }.count
        NSApp.dockTile.badgeLabel = waiting > 0 ? "\(waiting)" : nil
    }

    func status(of s: SessionInfo) -> SessionStatus {
        if s.exited { return .exited }
        if attention.contains(s.id) || s.needs != nil { return .needsYou }
        if s.in_flight > 0 { return .thinking }
        if s.activity == "working" || (s.output_ms_ago ?? .max) < 1500 { return .working }
        if unseenDone.contains(s.id) { return .done }
        return .idle
    }

    func select(_ id: String?) {
        selected = id
        guard let id else { return }
        attention.remove(id)
        unseenDone.remove(id)
        terminals[id]?.requestFocus()
    }

    /// Next session that needs the user, then one that finished unseen.
    func jumpToAttention() {
        let order = sessions.map(\.id)
        let start = order.firstIndex(of: selected ?? "") ?? -1
        let rotated = (1 ... max(order.count, 1)).map { order[(start + $0 + order.count) % max(order.count, 1)] }
        let pick = rotated.first { id in sessions.first { $0.id == id }.map { status(of: $0) == .needsYou } ?? false }
            ?? rotated.first { unseenDone.contains($0) }
        if let pick { select(pick) }
    }

    func terminal(for id: String) -> TerminalViewState {
        if let t = terminals[id] { return t }
        let t = TerminalViewState()
        t.configuration = TerminalSurfaceOptions(
            backend: .exec,
            envVars: ["PATH": DinoEnvironment.loginPath],
            command: "\(DinoEnvironment.dinoBinary) attach \(id)",
            waitAfterCommand: false
        )
        terminals[id] = t
        return t
    }

    func newSession(_ launcher: LauncherInfo) {
        guard let conn = connection else { return }
        let cwd = folder.path
        Task.detached {
            do {
                let resp = try conn.request(["type": "new", "launcher": launcher.short, "args": [], "cwd": cwd, "cols": 120, "rows": 40])
                await MainActor.run {
                    if let id = resp.id { self.select(id) }
                }
            } catch {
                await MainActor.run { self.error = error.localizedDescription }
            }
        }
    }

    /// Keep "On this Mac" fresh: sessions running in other terminals come and go.
    private func watchElsewhere() {
        Task.detached {
            while true {
                if let conn = try? DinoConnection(path: DinoEnvironment.socketPath), let list = try? conn.found(cloud: false) {
                    await MainActor.run {
                        // Keep cloud entries from the last full load.
                        let cloud = self.found.filter { $0.source == "cloud" }
                        if list + cloud != self.found { self.found = list + cloud }
                    }
                }
                try? await Task.sleep(for: .seconds(3))
            }
        }
    }

    /// Everything, including cloud (slower); for the Continue sheet.
    func loadFound() {
        loadingCloud = true
        Task.detached {
            let list = (try? DinoConnection(path: DinoEnvironment.socketPath).found(cloud: true)) ?? []
            await MainActor.run {
                self.found = list
                self.loadingCloud = false
            }
        }
    }

    /// Move a found session into dino. A running one finishes its turn first, then continues here.
    func adopt(_ f: FoundSession) {
        moving = f
        let cwd = folder.path
        Task.detached {
            do {
                // Own connection: a handoff may wait minutes for the turn to end.
                let conn = try DinoConnection(path: DinoEnvironment.socketPath)
                let id = try conn.adopt(f, cwd: cwd)
                await MainActor.run {
                    self.moving = nil
                    self.showContinue = false
                    self.found.removeAll { $0 == f }
                    // The next poll brings the new session; select it once it's there.
                    self.pendingSelect = id
                }
            } catch {
                await MainActor.run {
                    self.moving = nil
                    self.error = error.localizedDescription
                }
            }
        }
    }

    private var pendingSelect: String?

    func chooseFolder() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.directoryURL = folder
        panel.prompt = "Use Folder"
        if panel.runModal() == .OK, let url = panel.url { folder = url }
    }

    func kill(_ id: String) {
        guard let conn = connection else { return }
        Task.detached { _ = try? conn.request(["type": "kill", "id": id]) }
    }
}
