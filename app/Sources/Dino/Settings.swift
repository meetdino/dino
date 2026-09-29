import SwiftUI

/// `settings.toml`, as dinod sends it. dinod owns the file; the app never parses TOML.
struct DinoSettings: Codable, Equatable {
    struct Routing: Codable, Equatable { var proxy: Bool }
    struct Policies: Codable, Equatable {
        /// Launcher short names; empty means all. The shell is always allowed.
        var allowed_agents: [String]
        var default_agent: String?
        var worktree_trust: Bool
        /// 0 means no limit.
        var session_token_budget: UInt64
        /// Archive a session after its PR merges; nil from an older dinod.
        var close_merged: Bool?
        /// Offer the mode that never asks; nil from an older dinod.
        var allow_bypass: Bool?
        /// Give Claude sessions dino's session tools; nil from an older dinod.
        var session_tools: Bool?

        func allows(_ short: String) -> Bool {
            short == "shell" || allowed_agents.isEmpty || allowed_agents.contains(short)
        }
    }
    struct Machine: Codable, Equatable {
        var onboarded: Bool
        /// Keep the Mac from idle-sleeping while tasks are scheduled; nil from an older dinod.
        var keep_awake: Bool?
    }
    struct Repo: Codable, Equatable { var env: [String: String] }
    struct SshHost: Codable, Equatable { var folder: String }
    var routing: Routing
    var policies: Policies
    var machine: Machine
    /// What new sessions start with, by agent id; nil from an older dinod.
    var agents: [String: Controls]?
    /// By the repo's main checkout.
    var repos: [String: Repo]?
    var worktrees: Worktrees?
    /// Machines to run sessions on over SSH, by host; nil from an older dinod.
    var ssh: [String: SshHost]?
}

/// A provider key's name and where it comes from; dinod never sends values.
struct KeyInfo: Codable, Identifiable, Equatable {
    let name: String
    let purpose: String?
    /// "dino", "environment", or nil: not set.
    let source: String?
    var id: String { name }
}

private struct SettingsResponse: Decodable {
    let settings: DinoSettings
    /// Key paths an organization sets, like "policies.allow_bypass"; nil from an older dinod.
    let locked: [String]?
    /// Hosts in ~/.ssh/config, to suggest; nil from an older dinod.
    let ssh_config_hosts: [String]?
}
private struct KeysResponse: Decodable { let keys: [KeyInfo] }

extension DinoConnection {
    func settings() throws -> DinoSettings {
        try settingsAndLocks().settings
    }

    fileprivate func settingsAndLocks() throws -> SettingsResponse {
        try JSONDecoder().decode(SettingsResponse.self, from: send(["type": "settings"]))
    }

    func setSettings(_ settings: DinoSettings) throws {
        let encoded = try JSONSerialization.jsonObject(with: JSONEncoder().encode(settings))
        _ = try send(["type": "set_settings", "settings": encoded])
    }

    /// Every agent dinod can start, allowed or not.
    func allLaunchers() throws -> [LauncherInfo] {
        try request(["type": "all_launchers"]).launchers ?? []
    }

    func keys() throws -> [KeyInfo] {
        try JSONDecoder().decode(KeysResponse.self, from: send(["type": "keys"])).keys
    }

    /// Store `value` under `name`, or remove the key when `value` is nil.
    func setKey(_ name: String, value: String?) throws {
        var body: [String: Any] = ["type": "set_key", "name": name]
        if let value { body["value"] = value }
        _ = try send(body)
    }
}

/// The Settings window's view of dinod: loads on open, writes each change straight through.
@MainActor
final class SettingsStore: ObservableObject {
    @Published var settings: DinoSettings?
    /// What the organization sets (managed-settings.json), by key path.
    @Published var locked: [String] = []
    @Published var keys: [KeyInfo] = []
    @Published var agents: [LauncherInfo] = []
    /// Hosts in ~/.ssh/config.
    @Published var configHosts: [String] = []
    @Published var error: String?

    func load() {
        run { c in (try c.settingsAndLocks(), try c.keys(), try c.allLaunchers()) } done: {
            self.settings = $0.0.settings
            self.locked = $0.0.locked ?? []
            self.configHosts = $0.0.ssh_config_hosts ?? []
            self.keys = $0.1
            self.agents = $0.2
        }
    }

    /// `path` ("policies.allow_bypass", "agents.claude") or something in it is set by the organization.
    func isLocked(_ path: String) -> Bool {
        locked.contains { $0 == path || $0.hasPrefix(path + ".") || path.hasPrefix($0 + ".") }
    }

    func update(_ change: (inout DinoSettings) -> Void) {
        guard var next = settings else { return }
        change(&next)
        settings = next
        let saved = next
        // What agents offer follows the policies (bypass mode), so ask again.
        run { c in
            try c.setSettings(saved)
            return try c.allLaunchers()
        } done: { self.agents = $0 }
    }

    func setKey(_ name: String, value: String?) {
        run { c in
            try c.setKey(name, value: value)
            return (try c.keys(), try c.allLaunchers())
        } done: {
            self.keys = $0.0
            self.agents = $0.1
        }
    }

    /// Socket work off the main thread; dinod's message becomes the pane's error line.
    private func run<T: Sendable>(_ work: @escaping @Sendable (DinoConnection) throws -> T, done: @escaping @MainActor (T) -> Void) {
        Task.detached {
            do {
                let value = try work(DinoConnection(path: DinoEnvironment.socketPath))
                await MainActor.run {
                    self.error = nil
                    done(value)
                }
            } catch {
                await MainActor.run { self.error = "\(error)" }
            }
        }
    }
}

/// Settings' sections, in sidebar order.
enum SettingsPane: String, CaseIterable, Identifiable {
    case account, general, agents, policies, repos, worktrees, environments, routing, keys
    var id: String { rawValue }

    var title: String {
        switch self {
        case .account: "Dino Account"
        case .general: "General"
        case .agents: "Agents"
        case .policies: "Policies"
        case .repos: "Repositories"
        case .worktrees: "Worktrees"
        case .environments: "Environments"
        case .routing: "Routing"
        case .keys: "Keys"
        }
    }

    var icon: String {
        switch self {
        case .account: "person.crop.circle.fill"
        case .general: "gearshape.fill"
        case .agents: "cpu.fill"
        case .policies: "checkmark.shield.fill"
        case .repos: "folder.fill"
        case .worktrees: "square.stack.3d.up.fill"
        case .environments: "server.rack"
        case .routing: "arrow.triangle.branch"
        case .keys: "key.fill"
        }
    }

    var tint: Color {
        switch self {
        case .account: .blue
        case .general: .gray
        case .agents: .purple
        case .policies: .indigo
        case .repos: .teal
        case .worktrees: .teal
        case .environments: .blue
        case .routing: .green
        case .keys: .orange
        }
    }
}

/// A System Settings-style window: sections in a sidebar that never collapses, the pane beside it.
/// A plain Window, since the Settings scene forces centered toolbar tabs.
struct SettingsView: View {
    static let windowID = "settings"

    @StateObject private var store = SettingsStore()
    /// Settings reopens on the pane you left it at, like the system's.
    @AppStorage("settingsTab") private var pane: SettingsPane = .general

    var body: some View {
        NavigationSplitView(columnVisibility: .constant(.all)) {
            List(selection: Binding(get: { pane }, set: { if let p = $0 { pane = p } })) {
                AccountRow().tag(SettingsPane.account)
                    .padding(.vertical, 4)
                Section {
                    ForEach(SettingsPane.allCases.filter { $0 != .account }) { p in
                        HStack(spacing: 8) {
                            SettingsIcon(pane: p, size: 22)
                            Text(p.title)
                        }
                        .tag(p)
                    }
                }
            }
            .frame(width: 215)
            .navigationSplitViewColumnWidth(min: 215, ideal: 215, max: 215)
            .toolbar(removing: .sidebarToggle)
        } detail: {
            VStack(spacing: 0) {
                switch pane {
                case .account: AccountPane()
                case .general: GeneralPane()
                case .agents: AgentsPane()
                case .policies: PoliciesPane()
                case .repos: ReposPane()
                case .worktrees: WorktreesPane()
                case .environments: EnvironmentsPane()
                case .routing: RoutingPane()
                case .keys: KeysPane()
                }
                StoreError()
            }
            .navigationTitle(pane.title)
        }
        .environmentObject(store)
        .frame(width: 715, height: 470)
        .onAppear { store.load() }
    }
}

/// The rounded, colored glyph System Settings gives each section.
private struct SettingsIcon: View {
    let pane: SettingsPane
    let size: CGFloat

    var body: some View {
        Image(systemName: pane.icon)
            .font(.system(size: size * 0.55, weight: .semibold))
            .foregroundStyle(.white)
            .frame(width: size, height: size)
            .background(pane.tint.gradient, in: RoundedRectangle(cornerRadius: size * 0.24, style: .continuous))
    }
}

private struct AccountRow: View {
    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "person.crop.circle.fill")
                .font(.system(size: 30))
                .foregroundStyle(.secondary)
            VStack(alignment: .leading, spacing: 1) {
                Text("Log In").fontWeight(.semibold)
                Text("with your Dino Account").font(.caption).foregroundStyle(.secondary)
            }
        }
    }
}

private struct AccountPane: View {
    var body: some View {
        Form {
            Section {
                VStack(spacing: 10) {
                    Image(systemName: "person.crop.circle.fill")
                        .font(.system(size: 56))
                        .foregroundStyle(.secondary)
                    Text("Dino Account").font(.title2.weight(.semibold))
                    Text("Log in to keep your settings, policies and keys the same on every Mac you use dino on.")
                        .multilineTextAlignment(.center)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                    Button("Log In…") {}
                        .controlSize(.large)
                        .disabled(true)
                        .padding(.top, 4)
                    Text("Not available yet. dino works fully without an account, and nothing leaves this Mac.")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                }
                .frame(maxWidth: .infinity)
                .padding(.vertical, 12)
            }
        }
        .formStyle(.grouped)
    }
}

/// A section's explanation, left-aligned like System Settings'.
struct Footnote: View {
    let text: String
    init(_ text: String) { self.text = text }

    var body: some View {
        Text(text)
            .font(.callout)
            .foregroundStyle(.secondary)
            .multilineTextAlignment(.leading)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.leading, 10)
    }
}

/// Marks a setting the organization sets; hover says so.
struct OrgLock: View {
    var body: some View {
        Image(systemName: "lock.fill")
            .font(.caption)
            .foregroundStyle(.secondary)
            .help("Set by your organization")
    }
}

/// A setting row that's locked when the organization sets `path`: disabled, with the lock before it.
private struct OrgLocked: ViewModifier {
    @EnvironmentObject var store: SettingsStore
    let path: String

    func body(content: Content) -> some View {
        if store.isLocked(path) {
            HStack(spacing: 6) {
                OrgLock()
                content.disabled(true)
            }
            .help("Set by your organization")
        } else {
            content
        }
    }
}

extension View {
    func orgLocked(_ path: String) -> some View { modifier(OrgLocked(path: path)) }
}

/// Shown under a pane when dinod refused or couldn't be reached.
private struct StoreError: View {
    @EnvironmentObject var store: SettingsStore

    var body: some View {
        if let error = store.error {
            Label(error, systemImage: "exclamationmark.triangle.fill")
                .font(.callout)
                .foregroundStyle(.red)
                .padding([.horizontal, .bottom])
        }
    }
}

private struct GeneralPane: View {
    @EnvironmentObject var store: SettingsStore
    @AppStorage(QuitChoice.key) private var quitChoice = ""

    var body: some View {
        Form {
            Section {
                Picker("When you quit with agents running", selection: $quitChoice) {
                    Text("Ask").tag("")
                    Text("Keep them running").tag(QuitChoice.keep.rawValue)
                    Text("Stop them").tag(QuitChoice.stop.rawValue)
                }
            } footer: {
                Footnote("Agents run in dinod, not in this window. Stopped agents resume the next time dino starts.")
            }
            Section {
                Toggle("Keep your Mac awake while tasks are scheduled", isOn: Binding(
                    get: { store.settings?.machine.keep_awake ?? false },
                    set: { on in store.update { $0.machine.keep_awake = on } }
                ))
                .disabled(store.settings == nil)
                .orgLocked("machine.keep_awake")
            } footer: {
                Footnote("So scheduled tasks run on time. Closing the lid still sleeps it; missed tasks run once when it wakes.")
            }
            Section {
                LabeledContent("Settings and keys") {
                    HStack {
                        Text(NSString(string: DinoEnvironment.home).abbreviatingWithTildeInPath)
                            .foregroundStyle(.secondary)
                            .textSelection(.enabled)
                        Button("Show in Finder") {
                            NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: DinoEnvironment.home)
                        }
                    }
                }
            }
        }
        .formStyle(.grouped)
    }
}

private struct PoliciesPane: View {
    @EnvironmentObject var store: SettingsStore

    private static let budgets: [UInt64] = [0, 1_000_000, 5_000_000, 10_000_000, 25_000_000, 50_000_000, 100_000_000]

    private var policies: DinoSettings.Policies? { store.settings?.policies }
    private var agents: [LauncherInfo] { store.agents.filter { $0.short != "shell" } }
    private var startable: [LauncherInfo] { store.agents.filter { policies?.allows($0.short) ?? true } }

    private func allowed(_ l: LauncherInfo) -> Binding<Bool> {
        Binding(
            get: { policies?.allows(l.short) ?? true },
            set: { on in
                store.update {
                    var list = $0.policies.allowed_agents.isEmpty ? agents.map(\.short) : $0.policies.allowed_agents
                    list.removeAll { $0 == l.short }
                    if on { list.append(l.short) }
                    // Everything allowed is saved as "all", so agents dino finds later are allowed too.
                    $0.policies.allowed_agents = agents.allSatisfy { list.contains($0.short) } ? [] : list
                }
            }
        )
    }

    var body: some View {
        Form {
            Section {
                ForEach(agents) { l in
                    Toggle(l.label, isOn: allowed(l))
                        .orgLocked("policies.allowed_agents")
                }
                Picker("⌘N starts", selection: Binding(
                    get: {
                        // What dinod falls back to when the chosen one is off or missing: the first allowed.
                        let want = policies?.default_agent ?? "claude"
                        return startable.contains { $0.short == want } ? want : startable.first?.short ?? ""
                    },
                    set: { d in store.update { $0.policies.default_agent = d == "claude" ? nil : d } }
                )) {
                    ForEach(startable) { l in
                        Text(l.label).tag(l.short)
                    }
                }
                .orgLocked("policies.default_agent")
            } header: {
                Text("Agents")
            } footer: {
                Footnote("Agents you turn off leave the menus and fan-out, and dino won't start them. Ones already running keep going. The shell is always there.")
            }
            Section {
                Toggle("Trust fan-out worktrees when the repo is trusted", isOn: Binding(
                    get: { policies?.worktree_trust ?? true },
                    set: { on in store.update { $0.policies.worktree_trust = on } }
                ))
                .orgLocked("policies.worktree_trust")
            } header: {
                Text("Fan-out")
            } footer: {
                Footnote("Claude asks whether to trust each new folder, and every fan-out worktree is one. When you've trusted the repo, dino tells Claude its worktrees are trusted too, and forgets them when the fan-out closes. Codex does this on its own.")
            }
            Section {
                Toggle("Archive sessions after their PR merges", isOn: Binding(
                    get: { policies?.close_merged ?? false },
                    set: { on in store.update { $0.policies.close_merged = on } }
                ))
                .orgLocked("policies.close_merged")
            } header: {
                Text("Pull requests")
            } footer: {
                Footnote("When a session's PR merges, dino archives it once its agent is idle: the session stops, and the worktree dino made for it is removed if nothing in it would be lost. Unarchive it to pick the conversation up again, worktree and all. Sessions outside a dino worktree stay open.")
            }
            Section {
                Toggle("Allow bypass permissions mode", isOn: Binding(
                    get: { policies?.allow_bypass ?? true },
                    set: { on in
                        store.update {
                            $0.policies.allow_bypass = on
                            // Defaults that bypass go back to the agent's own mode.
                            if !on, let agents = $0.agents {
                                $0.agents = agents.mapValues { c in
                                    var c = c
                                    if c.mode == "bypass" { c.mode = nil }
                                    return c
                                }
                            }
                        }
                    }
                ))
                .orgLocked("policies.allow_bypass")
            } header: {
                Text("Permissions")
            } footer: {
                Footnote("Bypass lets an agent edit files and run any command without asking. Turn it off and dino hides it and won't start or switch a session into it. Sessions already in it keep running.")
            }
            Section {
                Toggle("Cross-session communication", isOn: Binding(
                    get: { policies?.session_tools ?? false },
                    set: { on in store.update { $0.policies.session_tools = on } }
                ))
                .orgLocked("policies.session_tools")
            } header: {
                Text("Sessions")
            } footer: {
                Footnote("Gives Claude sessions dino's tools to list and read every session in dino, whatever the agent, and, when you allow it, to message an idle one or start a new one. Applies to new sessions. For other agents, add “dino mcp” as an MCP server in their own settings.")
            }
            Section {
                Picker("Tokens per session", selection: Binding(
                    get: { policies?.session_token_budget ?? 0 },
                    set: { n in store.update { $0.policies.session_token_budget = n } }
                )) {
                    ForEach(budgetChoices, id: \.self) { n in
                        Text(n == 0 ? "No limit" : Self.format(n)).tag(n)
                    }
                }
                .orgLocked("policies.session_token_budget")
            } header: {
                Text("Budget")
            } footer: {
                Footnote("Counts input, cached and output tokens, like the sidebar. A session over its budget gets an error on its next model call. Only sessions routed through dino (see Routing); applies to running ones too.")
            }
        }
        .formStyle(.grouped)
        .disabled(store.settings == nil)
    }

    /// The presets, plus a value set by hand in settings.toml.
    private var budgetChoices: [UInt64] {
        let current = policies?.session_token_budget ?? 0
        return Self.budgets.contains(current) ? Self.budgets : (Self.budgets + [current]).sorted()
    }

    private static func format(_ n: UInt64) -> String {
        n >= 1_000_000 && n % 100_000 == 0
            ? "\((Double(n) / 1_000_000).formatted()) million"
            : "\(n.formatted()) tokens"
    }
}

private struct RoutingPane: View {
    @EnvironmentObject var store: SettingsStore

    private func has(_ key: String) -> Bool { store.keys.contains { $0.name == key && $0.source != nil } }

    var body: some View {
        VStack(spacing: 0) {
            Form {
                Section {
                    Toggle("Route agent traffic through dino", isOn: Binding(
                        get: { store.settings?.routing.proxy ?? true },
                        set: { on in store.update { $0.routing.proxy = on } }
                    ))
                    .disabled(store.settings == nil)
                    .orgLocked("routing.proxy")
                } footer: {
                    Footnote("dino's local proxy counts tokens per session and serves the free tier. Off, agents talk to their providers directly. Applies to sessions you start from now on.")
                }
                Section("Free tier") {
                    LabeledContent("Models") {
                        Text(has("NVIDIA_API_KEY") ? "NVIDIA NIM" : "Needs an NVIDIA key (see Keys)")
                            .foregroundStyle(has("NVIDIA_API_KEY") ? .primary : .secondary)
                    }
                    LabeledContent("Picks the model per turn") {
                        Text(has("TYPESAFE_API_KEY") ? "Jev, then built-in rules" : "Built-in rules")
                    }
                }
            }
            .formStyle(.grouped)
        }
    }
}

private struct KeysPane: View {
    @EnvironmentObject var store: SettingsStore
    @State private var editing: String?
    @State private var draft = ""
    @State private var removing: KeyInfo?

    var body: some View {
        VStack(spacing: 0) {
            Form {
                Section {
                    ForEach(store.keys) { key in
                        row(key)
                    }
                } footer: {
                    Footnote("Keys stay on this Mac in \(NSString(string: DinoEnvironment.home).abbreviatingWithTildeInPath)/keys, readable only by you, and dino never shows them again. They take effect immediately. Keychain storage comes with signed releases.")
                }
            }
            .formStyle(.grouped)
        }
        .confirmationDialog("Remove \(removing?.name ?? "")?", isPresented: Binding(get: { removing != nil }, set: { if !$0 { removing = nil } }), presenting: removing) { key in
            Button("Remove", role: .destructive) { store.setKey(key.name, value: nil) }
        } message: { _ in
            Text("dino can't show it again, so you'll need to paste it to add it back.")
        }
    }

    @ViewBuilder private func row(_ key: KeyInfo) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(key.name).font(.body.monospaced())
                    if let purpose = key.purpose {
                        Text(purpose).font(.callout).foregroundStyle(.secondary)
                    }
                }
                Spacer()
                status(key)
                if editing != key.name {
                    Button(key.source == "dino" ? "Replace…" : "Set…") {
                        draft = ""
                        editing = key.name
                    }
                    if key.source == "dino" {
                        Button("Remove…", role: .destructive) { removing = key }
                    }
                }
            }
            if editing == key.name {
                HStack {
                    SecureField("Paste the key", text: $draft)
                        .textFieldStyle(.roundedBorder)
                        .onSubmit { save(key) }
                    Button("Cancel") { editing = nil }
                    Button("Save") { save(key) }
                        .keyboardShortcut(.defaultAction)
                        .disabled(draft.trimmingCharacters(in: .whitespaces).isEmpty)
                }
            }
            if key.source == "environment" {
                Text("Set in your shell environment, which wins over a key stored here.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
        }
    }

    private func status(_ key: KeyInfo) -> some View {
        let (text, color): (String, Color) = switch key.source {
        case "dino": ("Stored", .green)
        case "environment": ("From environment", .blue)
        default: ("Not set", .secondary)
        }
        return HStack(spacing: 5) {
            Circle().fill(color).frame(width: 7, height: 7)
            Text(text).font(.callout).foregroundStyle(.secondary)
        }
    }

    private func save(_ key: KeyInfo) {
        let value = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty else { return }
        store.setKey(key.name, value: value)
        draft = ""
        editing = nil
    }
}

/// What each agent's new sessions start with. "Default" leaves it to the agent's own settings.
private struct AgentsPane: View {
    @EnvironmentObject var store: SettingsStore

    /// One per agent that has controls; the free tier is its own, since it picks models itself.
    private var agents: [LauncherInfo] {
        var seen = Set<String>()
        return store.agents.filter { ($0.knobs?.any ?? false) && seen.insert($0.agent_id).inserted }
    }

    private func controls(_ agent: String) -> Binding<Controls> {
        Binding(
            get: { store.settings?.agents?[agent] ?? Controls() },
            set: { c in store.update { $0.agents = ($0.agents ?? [:]).merging([agent: c]) { $1 }.filter { $0.value != Controls() } } }
        )
    }

    var body: some View {
        Form {
            if agents.isEmpty {
                Section {
                    Text("No agent dino can start has a mode, model or effort to choose.").foregroundStyle(.secondary)
                }
            }
            // The organization may set one control and leave the others: each is locked on its own.
            ForEach(agents) { l in
                Section(l.label) {
                    ControlFields(knobs: l.knobs!, controls: controls(l.agent_id), lockPath: "agents.\(l.agent_id)")
                }
            }
            Section {} footer: {
                Footnote("New sessions start with these unless you choose otherwise in New Session…. Default is whatever the agent's own settings say. Change a running session from its toolbar: ⇧⌘M mode, ⇧⌘I model, ⇧⌘E effort.")
            }
        }
        .formStyle(.grouped)
        .disabled(store.settings == nil)
    }
}

/// Environment variables for every session in a repo, its worktrees included.
private struct ReposPane: View {
    @EnvironmentObject var store: SettingsStore
    /// Repos dino has sessions in, to add without a file dialog.
    @State private var known: [RepoInfo] = []
    /// A repo added here but with no variable yet (an empty one isn't saved).
    @State private var adding: String?

    private var repos: [String: DinoSettings.Repo] { store.settings?.repos ?? [:] }
    private var shown: [String] { Array(Set(repos.keys).union(adding.map { [$0] } ?? [])).sorted() }

    private func setEnv(_ repo: String, _ change: @escaping (inout [String: String]) -> Void) {
        store.update {
            var all = $0.repos ?? [:]
            var env = all[repo]?.env ?? [:]
            change(&env)
            all[repo] = env.isEmpty ? nil : DinoSettings.Repo(env: env)
            $0.repos = all
        }
    }

    var body: some View {
        Form {
            Section {
            } header: {
                Text("Environment")
            } footer: {
                Footnote("Set for every session dino starts in the repo or one of its worktrees, from the next start or restart. Values are kept in settings.toml, readable only by you.")
            }
            ForEach(shown, id: \.self) { path in
                let env = repos[path]?.env ?? [:]
                let locked = store.isLocked("repos.\(path)")
                Section {
                    ForEach(env.keys.sorted(), id: \.self) { key in
                        EnvRow(key: key, value: env[key] ?? "") { value in
                            setEnv(path) { $0[key] = value }
                        } remove: {
                            setEnv(path) { $0[key] = nil }
                        }
                        .orgLocked("repos.\(path).env.\(key)")
                    }
                    NewEnvRow(taken: Set(env.keys)) { key, value in
                        setEnv(path) { $0[key] = value }
                        if adding == path { adding = nil }
                    }
                } header: {
                    HStack {
                        Text((path as NSString).lastPathComponent)
                        Text((path as NSString).abbreviatingWithTildeInPath).foregroundStyle(.secondary).fontWeight(.regular)
                        if locked { OrgLock() }
                        Spacer()
                        Button("Remove") {
                            setEnv(path) { $0 = [:] }
                            if adding == path { adding = nil }
                        }
                        .buttonStyle(.link)
                        .font(.callout)
                        .help("Remove this repo's variables")
                        .disabled(locked)
                    }
                }
            }
            Section {
                Menu("Add Repository") {
                    ForEach(known.filter { !shown.contains($0.path) }) { r in
                        Button(r.name) { adding = r.path }
                    }
                    if known.contains(where: { !shown.contains($0.path) }) { Divider() }
                    Button("Choose Folder…") {
                        let panel = NSOpenPanel()
                        panel.canChooseDirectories = true
                        panel.canChooseFiles = false
                        panel.prompt = "Add"
                        if panel.runModal() == .OK, let url = panel.url { adding = url.path }
                    }
                }
                .fixedSize()
            }
        }
        .formStyle(.grouped)
        .disabled(store.settings == nil)
        .task {
            known = await Task.detached { (try? DinoConnection(path: DinoEnvironment.socketPath).tree(folders: [])) ?? [] }.value
                .filter { !$0.worktrees.isEmpty }
        }
    }
}

/// Machines to run sessions on over SSH. Hosts are as `ssh` takes them, so everything else
/// (user, port, keys, jump hosts) stays in ~/.ssh/config, which dino never changes.
private struct EnvironmentsPane: View {
    @EnvironmentObject var store: SettingsStore
    @State private var entering = false
    @State private var draft = ""

    private var hosts: [String: DinoSettings.SshHost] { store.settings?.ssh ?? [:] }
    private var suggested: [String] { store.configHosts.filter { hosts[$0] == nil } }

    private func add(_ host: String) {
        let host = host.trimmingCharacters(in: .whitespaces)
        guard !host.isEmpty, !host.hasPrefix("-"), !host.contains(" "), hosts[host] == nil else { return }
        store.update { $0.ssh = ($0.ssh ?? [:]).merging([host: .init(folder: "")]) { a, _ in a } }
        draft = ""
        entering = false
    }

    var body: some View {
        Form {
            Section {
                ForEach(hosts.keys.sorted(), id: \.self) { host in
                    HStack(spacing: 10) {
                        Image(systemName: "server.rack").foregroundStyle(.secondary)
                        Text(host).lineLimit(1)
                        Spacer()
                        HostFolderField(folder: hosts[host]?.folder ?? "") { folder in
                            store.update { $0.ssh?[host] = .init(folder: folder) }
                        }
                        Button {
                            store.update { $0.ssh?[host] = nil }
                        } label: {
                            Image(systemName: "minus.circle")
                        }
                        .buttonStyle(.borderless)
                        .help("Remove \(host)")
                    }
                    .orgLocked("ssh.\(host)")
                }
                if entering {
                    HStack {
                        TextField("Host", text: $draft, prompt: Text("devbox or user@host"))
                            .textFieldStyle(.roundedBorder)
                            .onSubmit { add(draft) }
                        Button("Add") { add(draft) }.disabled(draft.trimmingCharacters(in: .whitespaces).isEmpty)
                        Button("Cancel") {
                            entering = false
                            draft = ""
                        }
                    }
                }
                if hosts.isEmpty && !entering {
                    Text("No hosts yet.").foregroundStyle(.secondary)
                }
            } header: {
                Text("SSH Hosts")
            } footer: {
                Footnote("New sessions can run on these machines: dino connects with ssh, as you would in Terminal, and starts the agent there. It must be installed on the host. Logins, keys and ports come from ~/.ssh/config. The folder is where sessions start when you don't choose one; empty means the home folder.")
            }
            Section {
                Menu("Add Host") {
                    ForEach(suggested, id: \.self) { h in
                        Button(h) { add(h) }
                    }
                    if !suggested.isEmpty { Divider() }
                    Button("Enter Host…") { entering = true }
                }
                .fixedSize()
            } footer: {
                if store.configHosts.isEmpty {
                    Footnote("Hosts in ~/.ssh/config show up here to pick from.")
                }
            }
        }
        .formStyle(.grouped)
        .disabled(store.settings == nil)
    }
}

/// A host's default folder, saved on Return or when the field loses focus.
private struct HostFolderField: View {
    let folder: String
    let save: (String) -> Void
    @State private var draft: String?
    @FocusState private var focused: Bool

    var body: some View {
        TextField("Folder", text: Binding(get: { draft ?? folder }, set: { draft = $0 }), prompt: Text("~"))
            .textFieldStyle(.roundedBorder)
            .font(.body.monospaced())
            .frame(width: 220)
            .focused($focused)
            .onSubmit(commit)
            .onChange(of: focused) { _, now in if !now { commit() } }
    }

    private func commit() {
        if let d = draft?.trimmingCharacters(in: .whitespaces), d != folder { save(d) }
        draft = nil
    }
}

/// A variable: its value hidden until you ask, editable in place.
private struct EnvRow: View {
    let key: String
    let value: String
    let save: (String) -> Void
    let remove: () -> Void
    @State private var shown = false
    @State private var draft: String?

    var body: some View {
        HStack(spacing: 8) {
            Text(key).font(.body.monospaced()).lineLimit(1)
            Spacer()
            if let d = draft {
                TextField("Value", text: Binding(get: { d }, set: { draft = $0 }))
                    .textFieldStyle(.roundedBorder)
                    .font(.body.monospaced())
                    .frame(maxWidth: 260)
                    .onSubmit(commit)
                Button("Cancel") { draft = nil }
                Button("Save", action: commit)
            } else {
                Text(shown ? value : String(repeating: "•", count: min(max(value.count, 6), 16)))
                    .font(.body.monospaced())
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .textSelection(.enabled)
                Button { shown.toggle() } label: { Image(systemName: shown ? "eye.slash" : "eye") }
                    .buttonStyle(.borderless)
                    .help(shown ? "Hide the value" : "Show the value")
                Button("Edit") { draft = value }
                Button(action: remove) { Image(systemName: "minus.circle") }
                    .buttonStyle(.borderless)
                    .help("Remove \(key)")
            }
        }
    }

    private func commit() {
        if let d = draft { save(d) }
        draft = nil
    }
}

private struct NewEnvRow: View {
    let taken: Set<String>
    let add: (String, String) -> Void
    @State private var key = ""
    @State private var value = ""

    private var name: String { key.trimmingCharacters(in: .whitespaces) }
    /// What dinod accepts: letters, digits and _, not starting with a digit.
    private var valid: Bool {
        guard let first = name.unicodeScalars.first, !("0"..."9").contains(first) else { return false }
        return name.unicodeScalars.allSatisfy { $0.isASCII && (CharacterSet.alphanumerics.contains($0) || $0 == "_") }
    }

    var body: some View {
        HStack(spacing: 8) {
            TextField("NAME", text: $key)
                .textFieldStyle(.roundedBorder)
                .font(.body.monospaced())
                .frame(width: 180)
            SecureField("Value", text: $value)
                .textFieldStyle(.roundedBorder)
                .onSubmit(commit)
            Button("Add", action: commit)
                .disabled(!valid)
                .help(taken.contains(name) ? "Replaces the value of \(name)" : "Letters, digits and _, not starting with a digit")
        }
    }

    private func commit() {
        guard valid else { return }
        add(name, value)
        key = ""
        value = ""
    }
}
