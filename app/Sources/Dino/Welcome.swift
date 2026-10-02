import SwiftUI

/// First open: what dino found on this Mac, in a sheet over the window. The shell is already up
/// behind it; the sheet fills in as dinod looks, and every row can be left alone. Closing it (Done
/// or Esc) marks this Mac onboarded (never synced) and hands the keyboard to the shell; Help →
/// Show Welcome brings it back.
struct WelcomeCard: View {
    @EnvironmentObject var model: DinoModel
    @StateObject private var store = SettingsStore()
    @Environment(\.openWindow) private var openWindow
    @AppStorage("settingsTab") private var settingsPane: SettingsPane = .general
    /// Local model servers that are running (Ollama, LM Studio…); nil until asked.
    @State private var local: [ProviderInfo]?
    /// Agents whose install or sign-in shell is open, by agent: asked again until it's done.
    @State private var running: [String: (session: String, action: String)] = [:]
    @State private var isDefault = Opening.isDefault
    @State private var asked = false
    @State private var showMissing = false
    @State private var rowsHeight: CGFloat = 0

    /// The ones dino works with best, in this order.
    private static let featured = ["claude", "codex", "kimi", "qwen", "pi", "hermes"]

    private var shown: Bool {
        guard let machine = store.settings?.machine else { return false }
        return !machine.onboarded || model.showWelcome
    }

    var body: some View {
        Color.clear
            .frame(width: 0, height: 0)
            .sheet(isPresented: Binding(get: { shown }, set: { if !$0, shown { close() } })) {
                card.onAppear(perform: look)
            }
            // After the first frame: whether this Mac has seen the card. On a new Mac dinod is still
            // starting then, so it asks again each second until it answers (or gives up after 30 s):
            // asked once, the card never came up on the very first launch.
            .task {
                for _ in 0..<30 where store.settings == nil {
                    store.load()
                    try? await Task.sleep(for: .seconds(1))
                }
            }
            .onChange(of: model.showWelcome) { _, on in if on { store.load() } }
    }

    private var card: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 10) {
                DinoMark(size: 22)
                Text("Welcome").font(.title2.weight(.semibold))
            }
            .accessibilityElement(children: .combine)
            .accessibilityLabel("Welcome to dino")
            .accessibilityAddTraits(.isHeader)
            // Its own height, up to a point; then it scrolls, so the window always fits. (A sheet
            // asks for its content's ideal height, which a ScrollView alone puts at nothing.)
            ScrollView {
                rows.onGeometryChange(for: CGFloat.self) { $0.size.height } action: { rowsHeight = $0 }
            }
            .scrollBounceBehavior(.basedOnSize)
            .frame(height: min(max(rowsHeight, 120), 560))
            HStack {
                Text("Help → Show Welcome brings this back").font(.caption).foregroundStyle(.tertiary)
                Spacer()
                Button("Done", action: close)
                    .keyboardShortcut(.defaultAction)
            }
        }
        .padding(20)
        .frame(width: 460)
        .onExitCommand(perform: close)
        // While an install or sign-in runs, ask now and then; once more when its shell ends.
        .task {
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(4))
                if !running.isEmpty { store.loadSetup() }
            }
        }
        .onChange(of: model.sessions) { _, sessions in
            let ended = running.filter { _, r in !sessions.contains { $0.id == r.session && !$0.exited } }
            guard !ended.isEmpty else { return }
            for agent in ended.keys { running[agent] = nil }
            store.loadSetup()
        }
        .onChange(of: store.setup) { _, setup in
            for (id, r) in running {
                guard let a = setup?.first(where: { $0.id == id }) else { continue }
                if r.action == "install" ? a.installed : a.signed_in == true { running[id] = nil }
            }
        }
    }

    // MARK: Rows

    private var rows: some View {
        VStack(alignment: .leading, spacing: 16) {
            group("Found on this Mac") {
                agents
                found
            }
            group("Try it") {
                row("keyboard", "Press ⌘I in any shell to ask in plain English")
                startsPicker
            }
            group("Optional") {
                if !isDefault {
                    Button("Make dino your default terminal") {
                        guard Opening.makeDefault() else { return NSSound.beep() }
                        isDefault = true
                    }
                    .buttonStyle(.link)
                    .font(.callout)
                }
                Button {
                    settingsPane = .account
                    openWindow(id: SettingsView.windowID)
                } label: {
                    Text("Sign in with GitHub to sync your settings across Macs").multilineTextAlignment(.leading)
                }
                .buttonStyle(.link)
                .font(.callout)
                .help("API keys and tokens never leave this Mac")
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func group(_ title: String, @ViewBuilder _ content: () -> some View) -> some View {
        VStack(alignment: .leading, spacing: 7) {
            Text(title).font(.subheadline.weight(.semibold)).foregroundStyle(.secondary)
                .accessibilityAddTraits(.isHeader)
            content()
        }
    }

    @ViewBuilder private var agents: some View {
        let setup = Self.featured.compactMap { id in store.setup?.first { $0.id == id } }
        let here = setup.filter(\.installed)
        let missing = setup.filter { !$0.installed }
        VStack(alignment: .leading, spacing: 5) {
            if store.setup == nil {
                HStack(spacing: 6) {
                    ProgressView().controlSize(.mini)
                    Text("Looking on this Mac…").font(.callout).foregroundStyle(.secondary)
                }
            } else if here.isEmpty {
                Text("No agents yet").font(.callout).foregroundStyle(.secondary)
            }
            ForEach(here) { a in agentRow(a) }
            if !missing.isEmpty {
                DisclosureGroup(isExpanded: Binding(get: { showMissing || here.isEmpty }, set: { showMissing = $0 })) {
                    ForEach(missing) { a in
                        HStack(spacing: 6) {
                            Text(a.name).font(.callout)
                            Spacer(minLength: 4)
                            if running[a.id] != nil {
                                ProgressView().controlSize(.mini)
                            } else {
                                Button("Install…") { act(a, "install") }
                                    .buttonStyle(.link)
                                    .font(.callout)
                                    .help("Runs \(a.install) in a new shell, where you can watch it")
                            }
                        }
                    }
                } label: {
                    // The whole line opens it, not only the chevron.
                    Button { showMissing.toggle() } label: {
                        Text("Other agents (\(missing.count))").font(.callout).foregroundStyle(.secondary)
                    }
                    .buttonStyle(.plain)
                }
            }
        }
    }

    private func agentRow(_ a: AgentSetupInfo) -> some View {
        HStack(spacing: 6) {
            Circle()
                .fill(a.signed_in == true ? Color(nsColor: .systemGreen) : a.signed_in == false ? Color(nsColor: .systemOrange) : Color.secondary.opacity(0.5))
                .frame(width: 7, height: 7)
                .accessibilityLabel(a.signed_in == true ? "Signed in" : a.signed_in == false ? "Not signed in" : "")
            Text(a.name).font(.callout)
            if let v = a.version {
                Text(v).font(.caption.monospacedDigit()).foregroundStyle(.tertiary).lineLimit(1)
            }
            Spacer(minLength: 4)
            if running[a.id] != nil {
                ProgressView().controlSize(.mini)
            } else if a.signed_in == false, a.sign_in != nil {
                Button("Sign In…") { act(a, "sign_in") }
                    .buttonStyle(.link)
                    .font(.callout)
            } else if let account = a.account, a.signed_in == true {
                Text(account).font(.caption).foregroundStyle(.secondary).lineLimit(1)
            }
        }
        .help(a.signed_in == nil ? (a.sign_in_hint.map { "Signs in with \($0) inside \(a.name)" } ?? a.name) : a.name)
    }

    @ViewBuilder private var found: some View {
        let keys = store.keys.filter { $0.source != nil }
        if !keys.isEmpty {
            row("key", keys.count == 1 ? "1 key found: \(keys[0].name)" : "\(keys.count) keys found")
                .help(keys.map(\.name).joined(separator: ", "))
        }
        if let local, !local.isEmpty {
            row("cpu", local.map { [$0.name, $0.version].compactMap { $0 }.joined(separator: " ") }.joined(separator: ", ") + " running")
        }
        if GhosttyConfig.loaded.isEmpty {
            row("terminal", "Using dino's terminal defaults")
                .help("Put a Ghostty config in ~/.config/ghostty/config and dino uses it")
        } else {
            row("checkmark.circle", "Your Ghostty config is applied")
                .help(GhosttyConfig.loaded.joined(separator: "\n"))
        }
    }

    /// What ⌘N starts: the agent you're signed in to, unless you chose.
    @ViewBuilder private var startsPicker: some View {
        let startable = store.agents.filter { $0.agent_id != "shell" && (store.settings?.policies.allows($0.short) ?? true) }
        if !startable.isEmpty {
            Picker("⌘N starts", selection: Binding(
                get: {
                    let want = store.settings?.policies.default_agent ?? suggested ?? "claude"
                    return startable.contains { $0.short == want } ? want : startable.first?.short ?? ""
                },
                set: { d in
                    store.update { $0.policies.default_agent = d == "claude" ? nil : d }
                }
            )) {
                ForEach(startable) { l in Text(l.label).tag(l.short) }
            }
            .font(.callout)
            .disabled(store.isLocked("policies.default_agent"))
        }
    }

    /// The first agent this Mac is signed in to, in dino's order; nil when none is.
    private var suggested: String? {
        Self.featured.first { id in store.setup?.first { $0.id == id }?.signed_in == true }
    }

    private func row(_ symbol: String, _ text: String) -> some View {
        Label {
            Text(text).font(.callout).fixedSize(horizontal: false, vertical: true)
        } icon: {
            Image(systemName: symbol).foregroundStyle(.secondary)
        }
    }

    // MARK: Actions

    /// Asked once the card shows: agents take a moment, so not before the shell's first frame.
    private func look() {
        guard !asked else { return }
        asked = true
        isDefault = Opening.isDefault
        store.loadSetup()
        Task.detached {
            let providers = (try? DinoConnection(path: DinoEnvironment.socketPath).providers()) ?? []
            await MainActor.run { local = providers.filter { $0.local && $0.connected } }
        }
    }

    /// The agent's own install or sign-in, in a new shell the user watches and answers.
    private func act(_ agent: AgentSetupInfo, _ action: String) {
        let model = model
        store.agentAction(agent.id, action) { session in
            running[agent.id] = (session, action)
            model.pendingSelect = session
        }
    }

    private func close() {
        // The agent it showed for ⌘N is the one ⌘N starts.
        if store.settings?.policies.default_agent == nil, let s = suggested, s != "claude",
           store.agents.contains(where: { $0.short == s }) {
            store.update { $0.policies.default_agent = s }
        }
        if store.settings?.machine.onboarded == false {
            store.update { $0.machine.onboarded = true }
        }
        model.showWelcome = false
        asked = false
        focusTerminal()
    }

    /// The card never keeps the keyboard: back to the terminal you were in.
    private func focusTerminal() {
        if let id = model.selected { model.select(id) }
    }
}
