import SwiftUI

/// An agent using the Mac's apps or a browser, as dinod tells from the tools it calls (Claude
/// Code's computer use and Claude in Chrome, Codex's Computer Use, open-computer-use, browser MCP
/// servers): a banner over its session with Stop, and a mark on its sidebar row.
enum Reach: String {
    case computer, browser

    /// "your Mac", "your browser".
    var object: String { self == .computer ? "your Mac" : "your browser" }
    var symbol: String { self == .computer ? "cursorarrow.motionlines" : "globe" }
}

extension SessionInfo {
    /// What its agent uses outside its terminal right now, if anything.
    var reach: Reach? { using.flatMap(Reach.init(rawValue:)) }

    /// The agent's name as people say it: "Claude", "Codex"; a shell's, the agent started in it.
    var agentWord: String { AgentNames.of(inside?.agent ?? agent_id) }

    /// "Claude is using your Mac"; while it asks to, "Claude wants to use your Mac".
    var usingSentence: String? {
        guard let reach else { return nil }
        return needs != nil ? "\(agentWord) wants to use \(reach.object)" : "\(agentWord) is using \(reach.object)"
    }
}

extension DinoModel {
    /// Stop the agent's turn as its own key would (Esc in most): what it was doing in an app or
    /// the browser stops, and the session stays open for your next message.
    func interrupt(_ id: String) {
        guard let conn = connection else { return }
        Task.detached {
            do {
                try conn.interrupt(session: id)
            } catch {
                await MainActor.run { self.error = error.localizedDescription }
            }
        }
    }
}

/// Over the terminals: which agent in view is using the Mac or a browser, with Stop. Its session
/// stays open; Stop interrupts the turn the way the agent's own key (Esc in most) would. No
/// animation: it can be up for minutes, and a terminal at rest should cost nothing.
struct ComputerUseBanner: View {
    @EnvironmentObject var model: DinoModel
    /// Stop was clicked and the agent hasn't stopped yet.
    @State private var stopping: Set<String> = []

    var body: some View {
        // The selected session, else the other half of its split.
        let shown = model.shownSplit.map { [$0.first, $0.second] } ?? model.selected.map { [$0] } ?? []
        let ids = shown.filter { $0 == model.selected } + shown.filter { $0 != model.selected }
        if let s = ids.lazy.compactMap({ id in model.sessions.first { $0.id == id && $0.reach != nil } }).first,
           let reach = s.reach, let sentence = s.usingSentence {
            HStack(spacing: 10) {
                Image(systemName: reach.symbol)
                    .font(.title3)
                    .foregroundStyle(Color(nsColor: .systemOrange))
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 1) {
                    Text(ids.count > 1 && s.id != model.selected ? "\(sentence) (\(s.display))" : sentence)
                        .font(.callout.weight(.semibold))
                    Text(reach == .computer
                        ? "It sees your screen and clicks and types in your apps. Stop ends its turn; the session stays open."
                        : "It reads pages and clicks and types in your browser. Stop ends its turn; the session stays open.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Spacer(minLength: 8)
                Button {
                    stopping.insert(s.id)
                    model.interrupt(s.id)
                } label: {
                    if stopping.contains(s.id) {
                        Text("Stopping…")
                    } else {
                        Label("Stop", systemImage: "stop.fill")
                    }
                }
                .controlSize(.regular)
                .disabled(stopping.contains(s.id))
                .help("Interrupt \(s.agentWord)'s turn, as pressing its interrupt key in its pane would. The session stays open.")
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 7)
            .background(Color(nsColor: .systemOrange).opacity(0.12))
            .overlay(alignment: .bottom) { Divider() }
            .accessibilityElement(children: .contain)
            .accessibilityLabel(sentence)
            .onChange(of: s.reach) { _, now in if now == nil { stopping.remove(s.id) } }
            .onDisappear { stopping.remove(s.id) }
        }
    }
}

/// On a sidebar row: its agent is using the Mac or a browser right now.
struct UsingMark: View {
    let session: SessionInfo
    let reach: Reach

    var body: some View {
        Image(systemName: reach.symbol)
            .font(.caption)
            .foregroundStyle(Color(nsColor: .systemOrange))
            .help(session.usingSentence ?? "")
            .accessibilityLabel(session.usingSentence ?? "")
    }
}

// MARK: - Settings → Experimental: computer use for more agents

/// See crates/dino-core/src/ipc.rs.
struct ComputerUseInfo: Codable, Equatable {
    var version: String
    var installed: Bool
    var accessibility: Bool?
    var screen_recording: Bool?
    var agents: [ComputerUseAgent]
}

struct ComputerUseAgent: Codable, Equatable, Identifiable {
    var id: String
    var name: String
    /// dino added it, and it's still as dino left it.
    var on: Bool
    /// What dino runs (or edits) to add it.
    var command: String
    /// It has computer use of its own: how to turn that on.
    var native: String?
    /// It already has an open-computer-use of the user's own.
    var theirs: Bool?
}

private struct ComputerUseResponse: Decodable { let info: ComputerUseInfo }

extension DinoConnection {
    func computerUse(_ body: [String: Any]) throws -> ComputerUseInfo {
        try JSONDecoder().decode(ComputerUseResponse.self, from: send(body)).info
    }
}

/// Under its switch once it's on: the install, macOS's permissions for it, and which agents get it.
struct ComputerUseOptions: View {
    @EnvironmentObject var store: SettingsStore
    @State private var info: ComputerUseInfo?
    /// What's being done now ("install", "permissions", an agent's id).
    @State private var busy: String?
    @State private var error: String?

    private var locked: Bool { store.isLocked("experimental.computer_use") }

    var body: some View {
        Group {
            LabeledContent {
                if busy == "install" {
                    HStack(spacing: 6) { ProgressView().controlSize(.small); Text("Installing…") }
                } else if info?.installed == true {
                    Text("Installed in dino's folder").foregroundStyle(.secondary)
                } else if info != nil {
                    Button("Install") { act("install", ["type": "computer_use_install"]) }
                }
            } label: {
                Text("open-computer-use \(info?.version ?? "")")
                Link("Source and license (MIT)", destination: URL(string: "https://github.com/iFurySt/open-codex-computer-use")!)
                    .font(.caption)
            }
            if info?.installed == true {
                LabeledContent {
                    if busy == "permissions" {
                        ProgressView().controlSize(.small)
                    } else {
                        Button(info?.accessibility == nil ? "Check…" : "Check Again") {
                            act("permissions", ["type": "computer_use_permissions"])
                        }
                    }
                } label: {
                    Text("Permissions")
                    Text(permissionsText).font(.caption)
                }
            }
            ForEach(info?.agents ?? []) { a in agentRow(a) }
            if let info, info.agents.isEmpty {
                Text("None of the agents it works with is on this Mac.").font(.callout).foregroundStyle(.secondary)
            }
            if let error {
                Label(error, systemImage: "exclamationmark.triangle.fill")
                    .font(.callout)
                    .foregroundStyle(.orange)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .disabled(locked)
        .task { load() }
        // Just turned on: installed once dinod has the switch on.
        .onChange(of: store.saves) { _, _ in
            if info?.installed == false, busy == nil { act("install", ["type": "computer_use_install"]) }
        }
    }

    private var permissionsText: String {
        guard let info, let ax = info.accessibility, let sr = info.screen_recording else {
            return "Accessibility and Screen Recording go to open-computer-use's own app, not to dino. Checking opens its setup window if one is missing."
        }
        if ax, sr { return "Accessibility and Screen Recording are granted to open-computer-use's app." }
        let missing = [ax ? nil : "Accessibility", sr ? nil : "Screen Recording"].compactMap { $0 }.joined(separator: " and ")
        return "\(missing) not granted yet: its setup window walks you through System Settings. Check again once you have."
    }

    @ViewBuilder private func agentRow(_ a: ComputerUseAgent) -> some View {
        // One dino added before it had its own (a plan changed) stays switchable, to remove it.
        if let native = a.native, !a.on {
            LabeledContent(a.name) { Text(native).font(.caption).foregroundStyle(.secondary) }
        } else if a.theirs == true {
            LabeledContent(a.name) { Text("Already has an open-computer-use of yours; dino leaves it alone").font(.caption).foregroundStyle(.secondary) }
        } else {
            Toggle(isOn: Binding(get: { a.on }, set: { on in
                act(a.id, ["type": "computer_use_agent", "agent": a.id, "on": on])
            })) {
                HStack(spacing: 6) {
                    Text(a.name)
                    if busy == a.id { ProgressView().controlSize(.mini) }
                }
                Text(a.on ? "Added with: \(a.command)" : "Adds it with: \(a.command)")
                    .font(.caption.monospaced())
                    .textSelection(.enabled)
                if a.id == "codex" {
                    Text("Codex's own Computer Use is documented for the Codex app, not the Codex CLI.").font(.caption)
                }
            }
            .disabled(busy != nil || info?.installed != true)
        }
    }

    private func load() {
        run(nil, ["type": "computer_use"]) { info in
            // On, and not installed yet (just turned on, or a download that failed before).
            if !info.installed, busy == nil { act("install", ["type": "computer_use_install"]) }
        }
    }

    private func act(_ what: String, _ body: [String: Any]) {
        guard busy == nil else { return }
        busy = what
        run(what, body) { _ in }
    }

    private func run(_ what: String?, _ body: [String: Any], then: @escaping @MainActor (ComputerUseInfo) -> Void) {
        nonisolated(unsafe) let body = body
        Task.detached {
            do {
                let next = try DinoConnection(path: DinoEnvironment.socketPath).computerUse(body)
                await MainActor.run {
                    if info != next { info = next }
                    error = nil
                    if what != nil { busy = nil }
                    then(next)
                }
            } catch {
                await MainActor.run {
                    // Asked before dinod saved the switch: it installs once it has (`saves`).
                    let early = what == "install" && error.localizedDescription.hasPrefix("Turn on")
                    self.error = early ? nil : error.localizedDescription
                    if what != nil { busy = nil }
                }
            }
        }
    }
}
