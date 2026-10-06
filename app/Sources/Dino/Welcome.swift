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
    @State private var showComputerUse = false
    @State private var rowsHeight: CGFloat = 0
    /// Out of the way while an install or sign-in runs in its tab (a sign-in asks you things there);
    /// back with the outcome when it ends.
    @State private var away = false
    /// Taken down for a quit (DinoApplication), not closed: unseen, it's there at the next launch.
    @State private var quitting = false

    /// The ones dino works with best, in this order.
    private static let featured = ["claude", "codex", "copilot", "cursor", "amp", "kimi", "qwen", "pi", "hermes", "codewhale", "opencode"]

    @ObservedObject private var updates = Updates.shared

    /// Up: asked for (Help → Show Welcome), or on a Mac that hasn't seen it once an update found
    /// first has been offered (Updates.beforeWelcome).
    private var shown: Bool {
        guard let machine = store.settings?.machine else { return false }
        return (!machine.onboarded && !updates.holdingWelcome) || model.showWelcome
    }

    /// The card is coming on this Mac: an update goes first.
    private var firstTime: Bool { store.settings?.machine.onboarded == false }

    /// dinod never said whether this Mac has seen the card (see the second `.task`).
    @State private var gaveUp = false

    /// For Updates: whether the card is up or about to be (or out of the way for an install), or
    /// nil while dino doesn't know yet whether it will be. The daily check waits for it.
    private var up: Bool? { store.settings == nil && !gaveUp ? nil : shown || firstTime }

    var body: some View {
        Color.clear
            .frame(width: 0, height: 0)
            .sheet(isPresented: Binding(get: { shown && !away && !quitting }, set: { if !$0, shown, !away { close() } })) {
                card.onAppear(perform: look)
            }
            .onReceive(NotificationCenter.default.publisher(for: DinoApplication.quitting)) { _ in quitting = true }
            .onReceive(NotificationCenter.default.publisher(for: DinoApplication.quitCalledOff)) { _ in quitting = false }
            // Watched here rather than on the card, which is away while they run.
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
            .onChange(of: running.isEmpty) { _, idle in if idle, away { away = false } }
            // After the first frame: whether this Mac has seen the card. On a new Mac dinod is still
            // starting then, so it asks again each second until it answers (or gives up after 30 s):
            // asked once, the card never came up on the very first launch.
            .task {
                for _ in 0..<30 where store.settings == nil {
                    store.load()
                    try? await Task.sleep(for: .seconds(1))
                }
                gaveUp = true
            }
            .onChange(of: up, initial: true) { _, up in Updates.shared.welcomeUp = up }
            .onChange(of: firstTime, initial: true) { _, first in if first { Updates.shared.beforeWelcome() } }
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
                Text("To see this again, choose Help → Show Welcome.").font(.caption).foregroundStyle(.tertiary)
                Spacer()
                if let first = firstAgent {
                    // Without knowing ⌘N: the agent it starts, where you are.
                    Button("Start \(first.label)") {
                        close()
                        model.newSessionHere(first)
                    }
                }
                DoneButton(action: close)
            }
        }
        .padding(20)
        .frame(width: 460)
        .onExitCommand(perform: close)
    }

    // MARK: Rows

    private var rows: some View {
        VStack(alignment: .leading, spacing: 16) {
            group("Found on this Mac") {
                agents
                found
            }
            group("Try it") {
                row("keyboard", "In any shell, press ⌘I and say what you want in plain English. Choose which agent answers in Settings → Terminal.")
                startsPicker
            }
            group("Optional") {
                computerUse
                // Not checked here: asking macOS whether dino may control the Mac lists dino in
                // System Settings, which waits until you go to Permissions.
                Button {
                    settingsPane = .general
                    openWindow(id: SettingsView.windowID)
                } label: {
                    Text("Let programs in dino's terminals record the screen and control apps").multilineTextAlignment(.leading)
                }
                .buttonStyle(.link)
                .font(.callout)
                .help("Settings → General → Permissions asks macOS for Screen Recording and \(Permission.accessibility.title)")
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
                .help("API keys and tokens stay on this Mac and never sync")
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
        VStack(alignment: .leading, spacing: 2) {
            agentLine(a)
            // Signing in that needs explaining (Pi has no models of its own).
            if a.signed_in == false, let note = a.sign_in_note {
                Text(note).font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                    .padding(.leading, 13)
            }
        }
    }

    private func agentLine(_ a: AgentSetupInfo) -> some View {
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

    /// How each agent here turns on its own computer use; dino installs nothing for those. The
    /// rest can get open-computer-use from Settings → Experimental.
    @ViewBuilder private var computerUse: some View {
        let here = Self.featured.filter(Self.computerUseAgents.contains).compactMap { id in store.setup?.first { $0.id == id && $0.installed } }
        if !here.isEmpty {
            DisclosureGroup(isExpanded: $showComputerUse) {
                VStack(alignment: .leading, spacing: 6) {
                    ForEach(here) { a in computerUseRow(a) }
                    if here.contains(where: { !Self.hasOwnComputerUse($0) }) {
                        Button("Set up computer use for other agents (Experimental)…") {
                            settingsPane = .experimental
                            openWindow(id: SettingsView.windowID)
                        }
                        .buttonStyle(.link)
                        .font(.callout)
                        .help("Adds open-computer-use to the agents you choose")
                    }
                }
                .padding(.top, 4)
            } label: {
                Button { showComputerUse.toggle() } label: {
                    Text("Let agents use your Mac's apps").font(.callout)
                }
                .buttonStyle(.plain)
            }
        }
    }

    /// The agents this step speaks for: those the Experimental option can add open-computer-use to
    /// (dino-core's `agent_mcp::AGENTS`), Claude Code and Codex with their own among them.
    private static let computerUseAgents: Set = ["claude", "codex", "qwen", "kimi", "pi", "hermes", "codewhale", "opencode", "copilot"]

    /// Claude Code with a Pro or Max plan, and Codex's app, have their own.
    private static func hasOwnComputerUse(_ a: AgentSetupInfo) -> Bool {
        a.id == "codex" || (a.id == "claude" && ["Claude Pro", "Claude Max"].contains(a.account ?? ""))
    }

    @ViewBuilder private func computerUseRow(_ a: AgentSetupInfo) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(a.name).font(.callout.weight(.medium))
            Group {
                switch a.id {
                case "claude" where Self.hasOwnComputerUse(a):
                    Text("In Claude, run /mcp, select computer-use and choose Enable (once per project). The first time, it asks for Accessibility and Screen Recording permission. To use your browser, install the Claude in Chrome extension, then run /chrome and choose Enabled by default.")
                case "claude":
                    Text("Claude Code's built-in computer use needs a Pro or Max plan\(a.account.map { " (you're signed in with \($0))" } ?? ""). The option below can add open-computer-use instead.")
                case "codex":
                    Text("In the Codex app: Plugins → Computer Use → Install plugin, then turn on its server and skill. OpenAI documents it only for the app, not for the Codex CLI.")
                case "copilot":
                    Text("No built-in computer use. The option below can add open-computer-use, if your organization's Copilot policy allows MCP servers.")
                default:
                    Text("No built-in computer use. The option below can add open-computer-use.")
                }
            }
            .font(.caption)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
            HStack(spacing: 10) {
                if let docs = Self.computerUseDocs[a.id] {
                    Link("How it works", destination: docs).font(.caption)
                }
                if a.id == "claude", Self.hasOwnComputerUse(a), let chrome = Self.computerUseDocs["claude-chrome"] {
                    Link("Claude in Chrome", destination: chrome).font(.caption)
                }
            }
        }
    }

    private static let computerUseDocs: [String: URL] = [
        "claude": URL(string: "https://code.claude.com/docs/en/computer-use")!,
        "claude-chrome": URL(string: "https://code.claude.com/docs/en/chrome")!,
        "codex": URL(string: "https://learn.chatgpt.com/docs/computer-use")!,
    ]

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
                .help("dino uses your Ghostty config if you add one at ~/.config/ghostty/config")
        } else {
            row("checkmark.circle", "Your Ghostty config is applied")
                .help(GhosttyConfig.loaded.joined(separator: "\n"))
        }
    }

    /// The agent ⌘N starts, once one is signed in: what the card offers to start.
    private var firstAgent: LauncherInfo? {
        let startable = store.agents.filter { $0.agent_id != "shell" && (store.settings?.policies.allows($0.short) ?? true) }
        let want = store.settings?.policies.default_agent ?? suggested
        return want.flatMap { w in startable.first { $0.short == w } }
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
            away = true
        }
    }

    private func close() {
        // Taken down for a quit, never closed: it's unseen.
        guard !quitting else { return }
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

/// Done, with the keyboard first: with keyboard navigation on, the first control took it (the ⌘N
/// picker, or the default-terminal link, so Space would have made dino the default terminal). Its
/// own focus state: one declared on the view that presents the sheet never reached the sheet's
/// window, so setting it did nothing. Set once the sheet is the key window (at onAppear it isn't yet).
private struct DoneButton: View {
    let action: () -> Void
    @FocusState private var focused: Bool

    var body: some View {
        Button("Done", action: action)
            .keyboardShortcut(.defaultAction)
            .focused($focused)
            .onAppear { DispatchQueue.main.async { focused = true } }
    }
}
