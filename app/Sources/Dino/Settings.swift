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
        /// Archive a session after its PR merges or closes; nil from an older dinod.
        var close_merged: Bool?
        /// Offer the mode that never asks; nil from an older dinod.
        var allow_bypass: Bool?
        /// Give Claude sessions dino's session tools; nil from an older dinod.
        var session_tools: Bool?
        /// Providers agents may fall back to; empty or nil: any.
        var fallback_providers: [String]?

        func allows(_ short: String) -> Bool {
            short == "shell" || allowed_agents.isEmpty || allowed_agents.contains(short)
        }
    }
    struct Machine: Codable, Equatable {
        var onboarded: Bool
        /// Keep the Mac from idle-sleeping while tasks are scheduled; nil from an older dinod.
        var keep_awake: Bool?
        /// Keep the Mac from idle-sleeping while any agent works; nil from an older dinod (on there).
        var awake_while_working: Bool?
        /// Shells mark their prompts and say where they are; nil from an older dinod (on there).
        var shell_integration: Bool?
        /// An agent typed into a dino shell reports to dino from its start; nil from an older dinod.
        var shell_agents: Bool?
        /// Keeping agents running with the lid closed; nil from an older dinod.
        var lid: Lid?
        /// Which Claude Code sessions get the Claude subscription token; nil from an older dinod.
        var claude_token: ClaudeTokenUse?
        /// Look for updates once a day; nil from an older dinod (on there).
        var check_updates: Bool?
        /// Ghostty's `shell-integration` and its features (GHOSTTY_SHELL_FEATURES), as the app last
        /// read them from the user's Ghostty config, for the shells dinod starts; nil from an older dinod.
        var shell_integration_mode: String?
        var shell_features: String?
        /// One compiler cache for every session's builds; nil from an older dinod.
        var build_cache: BuildCache?
    }
    struct ClaudeTokenUse: Codable, Equatable {
        var ssh: Bool
        var local: Bool
        static let standard = ClaudeTokenUse(ssh: true, local: false)
    }
    struct Lid: Codable, Equatable {
        var enabled: Bool
        /// "working" (an agent is working) or "open" (an agent session is open).
        var when: String
        var on_battery: Bool
        var min_battery: Int
        /// 0: no limit.
        var max_hours: Double
        static let standard = Lid(enabled: false, when: "working", on_battery: false, min_battery: 30, max_hours: 8)
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
    /// The terminal's own choices, kept by dinod so they sync; nil from an older dinod.
    var terminal: Terminal?
    /// For people who live in tmux; nil from an older dinod.
    var tmux: Tmux?
    /// Features being tried out, by name, each a switch; nil from an older dinod. A map, so one
    /// this app doesn't know yet goes back to dinod as it came.
    var experimental: [String: Bool]?
    /// Where each agent goes when its route hits a limit, by agent id; nil from an older dinod.
    /// Kept as JSON, so what a newer dinod adds goes back as it came (see `FallbackSetting`).
    var fallbacks: [String: [String: JSONValue]]?

    struct Tmux: Codable, Equatable {
        /// dino's agents as windows in your tmux, each running `dino attach`.
        var show_agents: Bool
        /// The session they go in; empty: the one you're attached to.
        var session: String
        /// New tabs attach to this tmux session; empty: they don't.
        var new_tabs: String

        static let defaults = Tmux(show_agents: false, session: "dino", new_tabs: "")

        /// A name dino hands tmux as is (as dinod checks it): letters, digits, `-`, `_` and `.`.
        static func valid(_ name: String) -> Bool {
            !name.isEmpty && name.count <= 64 && name.allSatisfy { $0.isASCII && ($0.isLetter || $0.isNumber || "-_.".contains($0)) }
        }
    }

    struct Terminal: Codable, Equatable {
        var start_with: String
        var quick_key: String
        var quick_autohide: Bool
        var on_quit: String
        var appearance: String
        /// Who ⌘I in a shell asks for a command, by agent id; empty: dino picks (`dino ai`).
        var ask_agent: String = ""
        /// The model ⌘I asks `ask_agent`; empty: the one its new sessions start with.
        var ask_model: String = ""
        /// Who ⌘⏎ hands the line to, by launcher; empty: ⌘I's agent.
        var handoff_agent: String = ""

        /// dinod's defaults: StartWith.last, QuickTerminal.Key.commandGrave, hide on click, ask on
        /// quit, the Mac's look.
        static let defaults = Terminal(start_with: "last", quick_key: "cmd-grave", quick_autohide: true, on_quit: "", appearance: "system")

        init(start_with: String, quick_key: String, quick_autohide: Bool, on_quit: String, appearance: String) {
            self.start_with = start_with
            self.quick_key = quick_key
            self.quick_autohide = quick_autohide
            self.on_quit = on_quit
            self.appearance = appearance
        }

        /// A dinod from before `appearance` (or the AI line's choices) leaves it out: the Mac's
        /// look (and dino picks).
        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            start_with = try c.decode(String.self, forKey: .start_with)
            quick_key = try c.decode(String.self, forKey: .quick_key)
            quick_autohide = try c.decode(Bool.self, forKey: .quick_autohide)
            on_quit = try c.decode(String.self, forKey: .on_quit)
            appearance = try c.decodeIfPresent(String.self, forKey: .appearance) ?? "system"
            ask_agent = try c.decodeIfPresent(String.self, forKey: .ask_agent) ?? ""
            ask_model = try c.decodeIfPresent(String.self, forKey: .ask_model) ?? ""
            handoff_agent = try c.decodeIfPresent(String.self, forKey: .handoff_agent) ?? ""
        }

        /// Only what the app also keeps for itself (`mirrored`), the rest at its defaults.
        var appOwn: Terminal {
            Terminal(start_with: start_with, quick_key: quick_key, quick_autohide: quick_autohide, on_quit: on_quit, appearance: appearance)
        }

        /// These, with what the app keeps for itself taken from `own`.
        func with(appOwn own: Terminal) -> Terminal {
            var t = own.appOwn
            t.ask_agent = ask_agent
            t.ask_model = ask_model
            t.handoff_agent = handoff_agent
            return t
        }

        /// As the app keeps them for itself (it reads them there at launch, before dinod answers).
        @MainActor static var mirrored: Terminal {
            let d = UserDefaults.standard
            return Terminal(
                start_with: d.string(forKey: StartWith.key) ?? defaults.start_with,
                quick_key: d.string(forKey: QuickTerminal.Key.storageKey) ?? defaults.quick_key,
                quick_autohide: d.object(forKey: QuickTerminal.autohideKey) as? Bool ?? defaults.quick_autohide,
                on_quit: d.string(forKey: QuitChoice.key) ?? defaults.on_quit,
                appearance: d.string(forKey: Appearance.key) ?? defaults.appearance
            )
        }

        /// Make the app's copy these, and claim a changed shortcut.
        @MainActor func mirror() {
            let before = Terminal.mirrored
            guard before != appOwn else { return }
            let d = UserDefaults.standard
            d.set(start_with, forKey: StartWith.key)
            d.set(quick_key, forKey: QuickTerminal.Key.storageKey)
            d.set(quick_autohide, forKey: QuickTerminal.autohideKey)
            d.set(on_quit, forKey: QuitChoice.key)
            d.set(appearance, forKey: Appearance.key)
            // Chosen on another Mac (or in settings.toml): the look changes here too.
            if before.appearance != appearance { (Appearance(rawValue: appearance) ?? .system).apply() }
            if before.quick_key != quick_key { QuickTerminal.shared.registerKey() }
        }
    }
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
    /// The managed file each locked path comes from; nil from an older dinod.
    let locked_from: [String: String]?
    /// Hosts in ~/.ssh/config, to suggest; nil from an older dinod.
    let ssh_config_hosts: [String]?
    /// Why settings.toml doesn't parse ("settings.toml has an error on line 3: …"); nil when it does.
    let error: String?
}
private struct KeysResponse: Decodable { let keys: [KeyInfo] }

/// An agent in Settings → Agents: whether it's here and signed in, and the agent's own commands
/// for getting it and signing in, which dino runs in a shell.
struct AgentSetupInfo: Codable, Identifiable, Equatable {
    let id: String
    let name: String
    let path: String?
    let version: String?
    /// Nil when the agent has no quick way to ask.
    let signed_in: Bool?
    /// "Claude Max", "ChatGPT", "API key".
    let account: String?
    let install: String
    let sign_in: String?
    /// What to type in the agent to sign in, for ones that do it from inside ("/login").
    let sign_in_hint: String?
    let homepage: String
    /// What signing in means for it, when that isn't obvious ("Pi has no models of its own…").
    let sign_in_note: String?

    var installed: Bool { path != nil }
}
private struct AgentSetupResponse: Decodable { let agents: [AgentSetupInfo] }

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

    /// Every agent dino knows, as it is on this Mac now. Takes a moment: each one is asked.
    func agentSetup() throws -> [AgentSetupInfo] {
        try JSONDecoder().decode(AgentSetupResponse.self, from: send(["type": "agent_setup"])).agents
    }

    /// Runs the agent's own `install` or `sign_in` command in a new shell; the session's id.
    func agentAction(_ id: String, _ action: String) throws -> String? {
        try request(["type": "agent_action", "id": id, "action": action]).id
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
    /// The managed file each locked path comes from.
    @Published var lockedFrom: [String: String] = [:]
    @Published var keys: [KeyInfo] = []
    @Published var agents: [LauncherInfo] = []
    /// Every known agent, installed or not; nil until first asked.
    @Published var setup: [AgentSetupInfo]?
    /// Hosts in ~/.ssh/config.
    @Published var configHosts: [String] = []
    @Published var error: String?
    /// Counts the settings dinod has saved: what depends on a change being in effect waits for it.
    @Published private(set) var saves = 0

    func load() {
        run { c in (try c.settingsAndLocks(), try c.keys(), try c.allLaunchers()) } done: {
            self.settings = $0.0.settings
            let locked = $0.0.locked ?? []
            if self.locked != locked { self.locked = locked }
            let from = $0.0.locked_from ?? [:]
            if self.lockedFrom != from { self.lockedFrom = from }
            self.configHosts = $0.0.ssh_config_hosts ?? []
            if let error = $0.0.error { self.error = error }
            self.keys = $0.1
            self.agents = $0.2
        }
    }

    /// Ask every agent again; one just installed also becomes one dino can start.
    func loadSetup() {
        run { c in (try c.agentSetup(), try c.allLaunchers()) } done: {
            if self.setup != $0.0 { self.setup = $0.0 }
            if self.agents != $0.1 { self.agents = $0.1 }
        }
    }

    /// Runs agent `id`'s own install or sign-in in a new shell; `done` gets the session.
    func agentAction(_ id: String, _ action: String, done: @escaping @MainActor (String) -> Void) {
        run { c in try c.agentAction(id, action) } done: { if let s = $0 { done(s) } }
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
        } done: {
            self.agents = $0
            self.saves += 1
        }
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

/// Settings' sections, in sidebar order: the app and the Mac first, then what agents do.
enum SettingsPane: String, CaseIterable, Identifiable {
    case account, general, terminal, tmux, power, agents, models, workspaces, experimental, managed
    var id: String { rawValue }

    /// A pane remembered from before: Policies was split up, most of it into Agents.
    init?(rawValue: String) {
        if rawValue == "policies" {
            self = .agents
            return
        }
        guard let pane = Self.allCases.first(where: { $0.rawValue == rawValue }) else { return nil }
        self = pane
    }

    /// The sidebar's groups, under the account row; what the organization manages only when it does.
    static func groups(managed: Bool) -> [[SettingsPane]] {
        [[.general, .terminal, .tmux, .power], [.agents, .models, .workspaces], [.experimental]] + (managed ? [[.managed]] : [])
    }

    var title: String {
        switch self {
        case .account: "Dino Account"
        case .general: "General"
        case .terminal: "Terminal"
        case .tmux: "tmux"
        case .power: "Power"
        case .agents: "Agents"
        case .models: "Models & Providers"
        case .workspaces: "Workspaces"
        case .experimental: "Experimental"
        case .managed: "Managed by your organization"
        }
    }

    var icon: String {
        switch self {
        case .account: "person.crop.circle.fill"
        case .general: "gearshape.fill"
        case .terminal: "terminal.fill"
        case .tmux: "rectangle.split.3x1.fill"
        case .power: "bolt.fill"
        case .agents: "cpu.fill"
        case .models: "cube.fill"
        case .workspaces: "folder.fill"
        case .experimental: "flask.fill"
        case .managed: "building.2.fill"
        }
    }

    var tint: Color {
        switch self {
        case .account: .blue
        case .general: .gray
        case .terminal: Color(white: 0.35)
        case .tmux: .green
        case .power: .orange
        case .agents: .purple
        case .models: .pink
        case .workspaces: .teal
        case .experimental: .brown
        case .managed: .indigo
        }
    }

    /// Open Settings at this pane next time it shows.
    func select() {
        UserDefaults.standard.set(rawValue, forKey: "settingsTab")
    }

    /// The parts a pane is split into, shown as tabs at its top; none for a single-part pane.
    var parts: [SettingsPart] {
        switch self {
        case .models: [.providers, .keys]
        case .workspaces: [.worktrees, .repos, .ssh]
        default: []
        }
    }
}

/// A part of a Settings pane. The raw values are stable: other places open Settings at one.
enum SettingsPart: String, Identifiable {
    case providers, keys, worktrees, repos, ssh
    var id: String { rawValue }

    var title: String {
        switch self {
        case .providers: "Providers"
        case .keys: "API Keys"
        case .worktrees: "Worktrees"
        case .repos: "Repositories"
        case .ssh: "SSH Hosts"
        }
    }

    var pane: SettingsPane {
        switch self {
        case .providers, .keys: .models
        case .worktrees, .repos, .ssh: .workspaces
        }
    }

    /// The defaults key for the part a pane last showed.
    static func key(_ pane: SettingsPane) -> String { "settingsPart.\(pane.rawValue)" }

    /// Open Settings at this part next time it shows.
    func select() {
        pane.select()
        UserDefaults.standard.set(rawValue, forKey: Self.key(pane))
    }
}

/// A System Settings-style window: sections in a sidebar that never collapses, the pane beside it.
/// A plain Window, since the Settings scene forces centered toolbar tabs.
struct SettingsView: View {
    static let windowID = "settings"

    @StateObject private var store = SettingsStore()
    /// Settings reopens on the pane you left it at, like the system's.
    @AppStorage("settingsTab") private var pane: SettingsPane = .general

    /// The pane to show: what the organization manages only while it manages something.
    private var shown: SettingsPane {
        pane == .managed && store.settings != nil && store.locked.isEmpty ? .general : pane
    }

    var body: some View {
        NavigationSplitView(columnVisibility: .constant(.all)) {
            List(selection: Binding(get: { shown }, set: { if let p = $0 { pane = p } })) {
                AccountRow().tag(SettingsPane.account)
                    .padding(.vertical, 4)
                ForEach(SettingsPane.groups(managed: !store.locked.isEmpty), id: \.self) { group in
                    Section {
                        ForEach(group) { p in
                            HStack(spacing: 8) {
                                SettingsIcon(pane: p, size: 22)
                                Text(p.title)
                            }
                            .tag(p)
                        }
                    }
                }
            }
            .frame(width: 215)
            .navigationSplitViewColumnWidth(min: 215, ideal: 215, max: 215)
            .toolbar(removing: .sidebarToggle)
        } detail: {
            VStack(spacing: 0) {
                switch shown {
                case .account: AccountPane()
                case .general: GeneralPane()
                case .terminal: TerminalSettingsPane()
                case .tmux: Form { TmuxSection() }.formStyle(.grouped)
                case .power: PowerPane()
                case .agents: AgentsPane()
                case .models, .workspaces: PartedPane(pane: pane)
                case .experimental: ExperimentalPane()
                case .managed: ManagedPane()
                }
                StoreError()
            }
            .navigationTitle(shown.title)
        }
        .modifier(TerminalChoicesSync())
        .environmentObject(store)
        // Resizable: the model list and the storage list use the room.
        .frame(minWidth: 715, idealWidth: 820, minHeight: 470, idealHeight: 620)
        .onAppear { store.load() }
    }
}

/// A pane made of parts, with tabs to switch between them: each part is a full pane of its own,
/// so a long list (models, worktrees on disk) never pushes the short ones out of sight.
private struct PartedPane: View {
    let pane: SettingsPane
    @AppStorage private var stored: String

    init(pane: SettingsPane) {
        self.pane = pane
        _stored = AppStorage(wrappedValue: pane.parts.first?.rawValue ?? "", SettingsPart.key(pane))
    }

    private var part: SettingsPart {
        SettingsPart(rawValue: stored).flatMap { pane.parts.contains($0) ? $0 : nil } ?? pane.parts[0]
    }

    var body: some View {
        VStack(spacing: 0) {
            Picker("Show", selection: Binding(get: { part }, set: { stored = $0.rawValue })) {
                ForEach(pane.parts) { p in Text(p.title).tag(p) }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .fixedSize()
            .padding(.top, 12)
            .padding(.bottom, 2)
            switch part {
            case .providers: ProvidersPane()
            case .keys: KeysPane()
            case .worktrees: WorktreesPane()
            case .repos: ReposPane()
            case .ssh: EnvironmentsPane()
            }
        }
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
            .accessibilityLabel("Set by your organization")
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

/// The terminal's choices, which dinod keeps too so they sync to your other Macs. Watched for the
/// whole window: they're split between General and Terminal.
private struct TerminalChoicesSync: ViewModifier {
    @EnvironmentObject var store: SettingsStore
    @AppStorage(QuitChoice.key) private var quitChoice = ""
    @AppStorage(StartWith.key) private var startWith = StartWith.last.rawValue
    @AppStorage(QuickTerminal.Key.storageKey) private var quickKey = QuickTerminal.Key.commandGrave.rawValue
    @AppStorage(QuickTerminal.autohideKey) private var quickAutohide = true
    @AppStorage(Appearance.key) private var appearance = Appearance.system.rawValue

    func body(content: Content) -> some View {
        content.onChange(of: DinoSettings.Terminal(start_with: startWith, quick_key: quickKey, quick_autohide: quickAutohide, on_quit: quitChoice, appearance: appearance)) { _, own in
            let t = (store.settings?.terminal ?? .defaults).with(appOwn: own)
            if store.settings?.terminal != t { store.update { $0.terminal = t } }
        }
    }
}

/// Settings → General: how dino starts and quits, its notifications, updates, the permissions its
/// terminals get, and where its files are.
private struct GeneralPane: View {
    @AppStorage(QuitChoice.key) private var quitChoice = ""
    @AppStorage(StartWith.key) private var startWith = StartWith.last.rawValue
    @AppStorage(DinoModel.askBeforeClosingKey) private var askBeforeClosing = true

    var body: some View {
        Form {
            Section {
                Picker("When dino opens", selection: $startWith) {
                    Text("Your last session").tag(StartWith.last.rawValue)
                    Text("A new shell").tag(StartWith.shell.rawValue)
                }
                Picker("When you quit with agents running", selection: $quitChoice) {
                    Text("Ask").tag("")
                    Text("Keep them running").tag(QuitChoice.keep.rawValue)
                    Text("Stop them").tag(QuitChoice.stop.rawValue)
                }
                Toggle("Ask before ⌘W closes an agent", isOn: $askBeforeClosing)
            } footer: {
                Footnote("If there's no last session to open, dino opens a new shell. Quitting dino doesn't stop your agents unless you choose “Stop them”. Stopped agents resume their conversations the next time you open dino. Closing an agent's tab or pane stops it and archives the session, to resume from Archived.")
            }
            Section("Notifications") {
                NeedsYouNotifyToggle(form: true)
            }
            UpdatesSection()
            DinodAgentSection()
            PermissionsSection()
            Section {
                LabeledContent("Settings and keys folder") {
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

/// Settings → Terminal: how it looks, the shell, the quick terminal, and opening from other apps.
private struct TerminalSettingsPane: View {
    @EnvironmentObject var store: SettingsStore
    @AppStorage(QuickTerminal.Key.storageKey) private var quickKey = QuickTerminal.Key.commandGrave.rawValue
    @AppStorage(QuickTerminal.autohideKey) private var quickAutohide = true
    @AppStorage(Appearance.key) private var appearance = Appearance.system.rawValue
    @State private var quickTaken = false
    @State private var isDefault = false
    @State private var makingDefault = false

    /// What loads dino's marks in zsh in tmux panes, for the user's .zshrc; dino never writes it there.
    static let tmuxLine = #"[[ -n $TMUX ]] && source "${DINO_HOME:-$HOME/.config/dino}/shell-integration/zsh/dino-tmux.zsh" 2>/dev/null"#

    var body: some View {
        Form {
            Section {
                Picker("Appearance", selection: Binding(get: { appearance }, set: { (Appearance(rawValue: $0) ?? .system).choose() })) {
                    ForEach(Appearance.allCases) { Text($0.label).tag($0.rawValue) }
                }
                LabeledContent("Ghostty config") {
                    let files = GhosttyConfig.loaded.map { NSString(string: $0).abbreviatingWithTildeInPath }
                    Text(files.isEmpty ? "None (Ghostty's defaults)" : files.joined(separator: "\n"))
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.trailing)
                        .textSelection(.enabled)
                }
                if !GhosttyConfig.skipped.isEmpty {
                    LabeledContent("Lines not applied") {
                        Text(GhosttyConfig.skipped.joined(separator: "\n"))
                            .foregroundStyle(.secondary)
                            .multilineTextAlignment(.trailing)
                            .textSelection(.enabled)
                    }
                }
            } header: {
                Text("Appearance")
            } footer: {
                Footnote("Terminals use the font, colors, cursor and key bindings from your Ghostty config, and update when you save it. dino's own shortcuts keep working. Ghostty's command and working-directory options don't apply in dino.")
            }
            Section {
                Toggle("Shell integration", isOn: Binding(
                    get: { store.settings?.machine.shell_integration ?? true },
                    set: { on in store.update { $0.machine.shell_integration = on } }
                ))
                .disabled(store.settings == nil)
                .orgLocked("machine.shell_integration")
            } header: {
                Text("Shell")
            } footer: {
                VStack(alignment: .leading, spacing: 6) {
                    Footnote("Lets dino see your prompts and current folder in zsh, bash, fish, elvish and nushell, as Ghostty does, so new tabs open in the same folder and you can jump between prompts. Follows shell-integration and shell-integration-features in your Ghostty config. Your shell startup files aren't changed. Applies to new shells.\n\nA tmux you start runs as in any terminal: its panes load only your own startup files, as in Ghostty. For zsh in tmux panes to tell dino their exit codes and pass on notifications too, add this line to your .zshrc. It also turns on tmux's allow-passthrough for those panes.")
                    Text(Self.tmuxLine)
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.leading)
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.leading, 10)
                }
            }
            ShellAISection()
            Section {
                Picker("Shortcut", selection: Binding(
                    get: { quickKey },
                    set: {
                        quickKey = $0
                        QuickTerminal.shared.registerKey()
                        quickTaken = QuickTerminal.Key.current != .off && !QuickTerminal.shared.registered
                    }
                )) {
                    ForEach(QuickTerminal.Key.allCases) { Text($0.label).tag($0.rawValue) }
                }
                if quickTaken {
                    Text("Another app uses this shortcut. Choose a different one.")
                        .font(.caption)
                        .foregroundStyle(.orange)
                }
                Toggle("Hide it when you click elsewhere", isOn: $quickAutohide)
                    .disabled(QuickTerminal.shared.place.autohide != nil)
                    .help(QuickTerminal.shared.place.autohide != nil ? "Set by quick-terminal-autohide in your Ghostty config" : "")
            } header: {
                Text("Quick Terminal")
            } footer: {
                Footnote("Press the shortcut in any app to drop down a terminal from the top of the screen. Its shell keeps running while it's hidden. To change where it appears and its size, use the quick-terminal settings in your Ghostty config. A global toggle_quick_terminal key binding there also opens it.")
            }
            Section {
                LabeledContent("Default terminal") {
                    if isDefault {
                        Text("dino").foregroundStyle(.secondary)
                    } else if makingDefault {
                        // macOS takes up to a minute or so to apply it.
                        HStack(spacing: 6) {
                            ProgressView().controlSize(.small)
                            Text("Taking effect…").foregroundStyle(.secondary)
                        }
                    } else {
                        Button("Make dino the Default Terminal") {
                            guard Opening.makeDefault() else { return NSSound.beep() }
                            makingDefault = true
                            Task {
                                for _ in 0 ..< 60 where !Opening.isDefault {
                                    try? await Task.sleep(for: .seconds(3))
                                }
                                isDefault = Opening.isDefault
                                makingDefault = false
                            }
                        }
                    }
                }
            } footer: {
                Footnote("When dino is your default terminal, scripts (.command and .tool files), programs and man pages you open from the Finder or other apps open in dino. To open a shell in a folder, open the folder with dino, or choose “New dino Shell at Folder” from the Finder's Services menu.")
            }
        }
        .formStyle(.grouped)
        .onAppear {
            isDefault = Opening.isDefault
            quickTaken = QuickTerminal.Key.current != .off && !QuickTerminal.shared.registered
        }
    }
}

/// Settings → Terminal: who the shell's AI line asks (⌘I) and hands requests to (⌘⏎).
private struct ShellAISection: View {
    @EnvironmentObject var store: SettingsStore

    private var terminal: DinoSettings.Terminal { store.settings?.terminal ?? .defaults }

    private func allowed(_ l: LauncherInfo) -> Bool { store.settings?.policies.allows(l.short) ?? true }

    /// Agents here that can answer once with no tools, one per agent.
    private var askers: [LauncherInfo] {
        var seen = Set<String>()
        return store.agents.filter { $0.answers_once == true && allowed($0) && seen.insert($0.agent_id).inserted }
    }

    /// Everything dino can start here, but a shell.
    private var startable: [LauncherInfo] {
        store.agents.filter { $0.agent_id != "shell" && allowed($0) }
    }

    /// Who ⌘I asks when none is chosen, as `dino ai` picks: the default agent if it can answer,
    /// else the first here that can.
    private var automatic: LauncherInfo? {
        // A free tier's agent, on its own models.
        let launcher = store.settings?.policies.default_agent ?? "claude"
        let wanted = launcher == "free" ? "claude" : launcher.replacingOccurrences(of: "-free", with: "")
        return askers.first { $0.agent_id == wanted } ?? askers.first
    }

    private var asker: LauncherInfo? {
        terminal.ask_agent.isEmpty ? automatic : askers.first { $0.agent_id == terminal.ask_agent }
    }

    private func set(_ change: @escaping (inout DinoSettings.Terminal) -> Void) {
        store.update { s in
            var t = s.terminal ?? .defaults
            change(&t)
            s.terminal = t
        }
    }

    /// The model, as `ControlFields` takes it: only the model, Default being new sessions'.
    private func model(_ agent: LauncherInfo) -> some View {
        var knobs = agent.knobs ?? .none
        knobs.modes = []
        knobs.efforts = []
        return ControlFields(
            knobs: knobs,
            controls: Binding(
                get: { Controls(model: terminal.ask_model.isEmpty ? nil : terminal.ask_model) },
                set: { c in if (c.model ?? "") != terminal.ask_model { set { $0.ask_model = c.model ?? "" } } }
            ),
            defaults: Controls(model: store.settings?.agents?[agent.agent_id]?.model)
        )
        .orgLocked("terminal.ask_model")
    }

    var body: some View {
        Section {
            Picker("⌘I asks", selection: Binding(
                get: { terminal.ask_agent },
                set: { id in
                    guard id != terminal.ask_agent else { return }
                    // A model is one agent's own name for it.
                    set { $0.ask_agent = id; $0.ask_model = "" }
                }
            )) {
                Text(automatic.map { "Automatic (\($0.label))" } ?? "Automatic").tag("")
                ForEach(askers) { Text($0.label).tag($0.agent_id) }
                if !terminal.ask_agent.isEmpty, !askers.contains(where: { $0.agent_id == terminal.ask_agent }) {
                    Text("\(terminal.ask_agent) (not on this Mac)").tag(terminal.ask_agent)
                }
            }
            .orgLocked("terminal.ask_agent")
            if !terminal.ask_agent.isEmpty, let a = asker, a.knobs?.model == true {
                model(a)
            }
            Picker("⌘⏎ sends to", selection: Binding(
                get: { terminal.handoff_agent },
                set: { id in if id != terminal.handoff_agent { set { $0.handoff_agent = id } } }
            )) {
                Text(asker.map { "Same as ⌘I (\($0.label))" } ?? "Same as ⌘I").tag("")
                ForEach(startable) { Text($0.label).tag($0.short) }
                if !terminal.handoff_agent.isEmpty, !startable.contains(where: { $0.short == terminal.handoff_agent }) {
                    Text("\(terminal.handoff_agent) (not on this Mac)").tag(terminal.handoff_agent)
                }
            }
            .orgLocked("terminal.handoff_agent")
        } header: {
            Text("AI in the Shell")
        } footer: {
            Footnote("Type what you want in plain English at a shell prompt. ⌘I turns it into a command and puts it on the line for you to check and run. ⌘⏎ sends the line to an agent as a new session. In other terminals with dino's shell integration, use Alt+I and Alt+Enter.\n\nThe agent ⌘I asks can't use tools, so it can't change anything. Only agents that support this are listed. A small, fast model answers sooner.")
        }
        .disabled(store.settings == nil)
    }
}

/// Settings → Power: keeping the Mac awake while agents work, for scheduled automations, and for agents with the lid closed.
private struct PowerPane: View {
    @EnvironmentObject var store: SettingsStore

    var body: some View {
        Form {
            Section {
                Toggle("Keep your Mac awake while agents work", isOn: Binding(
                    get: { store.settings?.machine.awake_while_working ?? true },
                    set: { on in store.update { $0.machine.awake_while_working = on } }
                ))
                .disabled(store.settings == nil)
                .orgLocked("machine.awake_while_working")
                AwakeNow()
            } header: {
                Text("Staying Awake")
            } footer: {
                Footnote("Your Mac won't go to sleep on its own while any agent is working. The display can still turn off. Closing the lid still puts your Mac to sleep, unless you turn on “Keep agents running with the lid closed” below.")
            }
            Section {
                Toggle("Keep your Mac awake while automations are scheduled", isOn: Binding(
                    get: { store.settings?.machine.keep_awake ?? false },
                    set: { on in store.update { $0.machine.keep_awake = on } }
                ))
                .disabled(store.settings == nil)
                .orgLocked("machine.keep_awake")
            } footer: {
                Footnote("Your Mac won't go to sleep on its own while you have scheduled automations, so they run on time. Closing the lid still puts your Mac to sleep. An automation that was due while your Mac slept runs once when it wakes.")
            }
            LidSection()
        }
        .formStyle(.grouped)
    }
}

/// Settings → tmux. For people who live in tmux, and theirs stays theirs: dino never
/// edits its config, takes none of its keys, and only touches windows it made.
private struct TmuxSection: View {
    @EnvironmentObject var store: SettingsStore
    @State private var session = ""
    @State private var tabs = ""
    @FocusState private var editing: Field?

    private enum Field { case session, tabs }

    private var tmux: DinoSettings.Tmux { store.settings?.tmux ?? .defaults }

    private func change(_ edit: (inout DinoSettings.Tmux) -> Void) {
        var t = tmux
        edit(&t)
        if t != tmux { store.update { $0.tmux = t } }
    }

    var body: some View {
        Section {
            Toggle("Show agents in tmux", isOn: Binding(get: { tmux.show_agents }, set: { on in change { $0.show_agents = on } }))
            if tmux.show_agents {
                Picker("Put agents in", selection: Binding(
                    get: { tmux.session.isEmpty ? "attached" : "named" },
                    set: { v in change { $0.session = v == "attached" ? "" : (DinoSettings.Tmux.valid(session) ? session : "dino") } }
                )) {
                    Text("Their own tmux session").tag("named")
                    Text("The tmux session you're attached to").tag("attached")
                }
                if !tmux.session.isEmpty {
                    name("Session", text: $session, field: .session)
                }
            }
            Toggle("New tabs open in tmux", isOn: Binding(
                get: { !tmux.new_tabs.isEmpty },
                set: { on in change { $0.new_tabs = on ? (DinoSettings.Tmux.valid(tabs) ? tabs : "main") : "" } }
            ))
            if !tmux.new_tabs.isEmpty {
                name("Session", text: $tabs, field: .tabs)
            }
        } footer: {
            Footnote("Each agent appears as a tmux window, so you can reach it with `tmux attach` from anywhere. Closing the window or quitting tmux doesn't stop the agent, and its window comes back. dino doesn't start tmux and never changes your tmux config or your own windows. New tabs attach to the tmux session you name, and create it if needed.\n\nTo show your agents in the tmux status bar, add: set -g status-right '#(dino status --tmux)'")
        }
        .disabled(store.settings == nil)
        .onAppear {
            session = tmux.session.isEmpty ? "dino" : tmux.session
            tabs = tmux.new_tabs.isEmpty ? "main" : tmux.new_tabs
        }
        // A name is taken when you're done typing it, not letter by letter.
        .onChange(of: editing) { was, _ in
            if was == .session { commit(.session) }
            if was == .tabs { commit(.tabs) }
        }
    }

    private func name(_ label: String, text: Binding<String>, field: Field) -> some View {
        VStack(alignment: .trailing, spacing: 2) {
            TextField(label, text: text)
                .focused($editing, equals: field)
                .onSubmit { commit(field) }
            if !DinoSettings.Tmux.valid(text.wrappedValue) {
                Text("Use only letters, digits, -, _ and .").font(.caption).foregroundStyle(.orange)
            }
        }
    }

    /// A name typed into a field that's showing. Focus leaving a field also commits it, and a
    /// field goes away when its switch is turned off: that must not turn the switch back on.
    private func commit(_ field: Field) {
        switch field {
        case .session where tmux.show_agents && !tmux.session.isEmpty && DinoSettings.Tmux.valid(session): change { $0.session = session }
        case .tabs where !tmux.new_tabs.isEmpty && DinoSettings.Tmux.valid(tabs): change { $0.new_tabs = tabs }
        default: break
        }
    }
}

/// Settings → Agents: which agents you use, and the one ⌘N starts.
private struct AgentChoiceSection: View {
    @EnvironmentObject var store: SettingsStore

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
        Section {
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
            ForEach(agents) { l in
                Toggle(l.label, isOn: allowed(l))
                    .orgLocked("policies.allowed_agents")
            }
        } header: {
            Text("Agents You Use")
        } footer: {
            Footnote("Agents you turn off are hidden from menus and can't be started. Sessions already running keep going. Shells are always available.")
        }
    }
}

/// Settings → Agents: whether any agent may run in the mode that never asks.
private struct BypassSection: View {
    @EnvironmentObject var store: SettingsStore

    var body: some View {
        Section {
            Toggle("Allow bypass permissions mode", isOn: Binding(
                get: { store.settings?.policies.allow_bypass ?? true },
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
            Footnote("In bypass permissions mode, an agent edits files and runs any command without asking you. When this is on and you've accepted Claude Code's own warning about it, Claude Code starts with it in its Shift+Tab cycle, so switching to it needs no restart. Claude Code then doesn't block edits in plan mode either. When this is off, the mode isn't offered, and no session can start in it or switch to it. Sessions already in it keep running.")
        }
    }
}

/// Settings → Agents → Limits: how much a session may use.
private struct LimitsSection: View {
    @EnvironmentObject var store: SettingsStore

    private static let budgets: [UInt64] = [0, 1_000_000, 5_000_000, 10_000_000, 25_000_000, 50_000_000, 100_000_000]

    var body: some View {
        Section {
            Picker("Tokens per session", selection: Binding(
                get: { store.settings?.policies.session_token_budget ?? 0 },
                set: { n in store.update { $0.policies.session_token_budget = n } }
            )) {
                ForEach(budgetChoices, id: \.self) { n in
                    Text(n == 0 ? "No limit" : Self.format(n)).tag(n)
                }
            }
            .orgLocked("policies.session_token_budget")
        } header: {
            Text("Limits")
        } footer: {
            Footnote("Counts input, cached and output tokens, as the sidebar does. Once a session goes over the limit, its next request to the model fails with an error. Applies to sessions whose traffic goes through dino (Models & Providers → Providers), including ones already running.")
        }
    }

    /// The presets, plus a value set by hand in settings.toml.
    private var budgetChoices: [UInt64] {
        let current = store.settings?.policies.session_token_budget ?? 0
        return Self.budgets.contains(current) ? Self.budgets : (Self.budgets + [current]).sorted()
    }

    static func format(_ n: UInt64) -> String {
        n >= 1_000_000 && n % 100_000 == 0
            ? "\((Double(n) / 1_000_000).formatted()) million"
            : "\(n.formatted()) tokens"
    }
}

/// Whether agents' traffic goes through dino: the top of Models & Providers.
/// Sections only, for the providers' form to hold.
struct RoutingSections: View {
    @EnvironmentObject var store: SettingsStore

    var body: some View {
        Section {
            Toggle("Route agent traffic through dino", isOn: Binding(
                get: { store.settings?.routing.proxy ?? true },
                set: { on in store.update { $0.routing.proxy = on } }
            ))
            .disabled(store.settings == nil)
            .orgLocked("routing.proxy")
        } header: {
            Text("Routing")
        } footer: {
            Footnote("Lets dino count each session's tokens, apply token limits and fallbacks, and connect agents to the providers below and to the free models pool (Experimental). When this is off, agents connect to their providers directly. Applies to new sessions.")
        }
    }
}

/// A feature being tried out: off until turned on.
private struct ExperimentalFeature: Identifiable {
    /// Its name in settings.toml's [experimental], or under [policies] for one that predates it.
    let id: String
    let title: String
    /// What it does, and what leaves this Mac when it's on.
    let summary: String

    static let all = [
        ExperimentalFeature(
            id: "free_models",
            title: "Free models pool",
            summary: "Run agents on free NVIDIA models, with dino choosing a model for each turn. If you add a TypeSafe key, the first 8,000 characters of each turn's prompt are sent to api.typesafe.ai to choose the model. Nothing is sent while this is off."
        ),
        ExperimentalFeature(
            id: "computer_use",
            title: "Computer use for more agents",
            summary: "Lets agents without built-in computer use see your screen and click and type in your apps, using the open-source open-computer-use. Only the agents you choose get it. Be careful: anything on screen, such as a web page or a message, could tell an agent to do something you didn't ask for. Turning this off removes everything dino added."
        ),
        ExperimentalFeature(
            id: "session_tools",
            title: "Cross-session communication",
            summary: "Lets Claude sessions list and read your other dino sessions, whatever agent they run. With your permission, they can also message an idle session or start a new one. Applies to new sessions. To give other agents the same ability, add “dino mcp” as an MCP server in their settings."
        ),
    ]

    /// Kept under [policies], so it follows your account to your other Macs like other agent settings.
    var isPolicy: Bool { id == "session_tools" }

    /// Its key path in settings.toml, as the organization would lock it.
    var path: String { isPolicy ? "policies.\(id)" : "experimental.\(id)" }
}

/// Settings → Experimental: one switch per feature being tried out.
private struct ExperimentalPane: View {
    @EnvironmentObject var store: SettingsStore

    private func on(_ f: ExperimentalFeature) -> Binding<Bool> {
        let id = f.id
        if f.isPolicy {
            return Binding(
                get: { store.settings?.policies.session_tools ?? false },
                set: { on in store.update { $0.policies.session_tools = on } }
            )
        }
        return Binding(
            get: { store.settings?.experimental?[id] ?? false },
            set: { on in
                guard (store.settings?.experimental?[id] ?? false) != on else { return }
                store.update { $0.experimental = ($0.experimental ?? [:]).merging([id: on]) { $1 } }
            }
        )
    }

    private func has(_ key: String) -> Bool { store.keys.contains { $0.name == key && $0.source != nil } }

    var body: some View {
        Form {
            Section {
                ForEach(ExperimentalFeature.all) { f in
                    Toggle(isOn: on(f)) {
                        Text(f.title)
                        Text(f.summary)
                    }
                    .orgLocked(f.path)
                    if f.id == "computer_use", on(f).wrappedValue {
                        ComputerUseOptions()
                    }
                    if f.id == "free_models", on(f).wrappedValue {
                        LabeledContent("Models") {
                            Text(has("NVIDIA_API_KEY") ? "NVIDIA NIM" : "Needs an NVIDIA key (Models & Providers → API Keys)")
                                .foregroundStyle(has("NVIDIA_API_KEY") ? .primary : .secondary)
                        }
                        LabeledContent("Chooses each turn's model with") {
                            Text(has("TYPESAFE_API_KEY") ? "TypeSafe Jev, then built-in rules" : "Built-in rules")
                        }
                    }
                }
            } footer: {
                Footnote("These features are still being tested. Each is off until you turn it on, and applies only to this Mac, except Cross-session communication, which syncs to your other Macs.")
            }
        }
        .formStyle(.grouped)
        .disabled(store.settings == nil)
    }
}

/// Settings → Managed by your organization: every setting the organization sets, what it's set
/// to, where it shows in Settings and which file sets it. Read-only: the organization's files
/// change them. Only in the sidebar while something is managed.
private struct ManagedPane: View {
    @EnvironmentObject var store: SettingsStore

    /// A locked setting as people know it: its name, and where it shows in Settings.
    private struct Item: Identifiable {
        let id: String
        let title: String
        let place: String
        /// Where "Show" goes.
        let pane: SettingsPane?
        let part: SettingsPart?
    }

    private var items: [Item] { store.locked.map(item) }

    var body: some View {
        Form {
            Section {
                if items.isEmpty {
                    Text("Your organization doesn't set anything in dino.").foregroundStyle(.secondary)
                }
                ForEach(items) { row($0) }
            } footer: {
                Footnote("Your organization sets these, and they override your own choices. They appear locked throughout Settings. Your own choices are kept, and come back if your organization stops setting them.")
            }
        }
        .formStyle(.grouped)
    }

    private func row(_ i: Item) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(alignment: .firstTextBaseline) {
                OrgLock()
                VStack(alignment: .leading, spacing: 2) {
                    Text(i.title)
                    if let pane = i.pane {
                        Button("Settings → \(i.place)") { open(pane, i.part) }
                            .buttonStyle(.link)
                            .font(.callout)
                            .help("Show it in Settings")
                    } else {
                        Text(i.place).font(.callout).foregroundStyle(.secondary)
                    }
                }
                Spacer()
                Text(value(i.id))
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.trailing)
                    .textSelection(.enabled)
            }
            if let file = store.lockedFrom[i.id] {
                Text("\(i.id) in \(NSString(string: file).abbreviatingWithTildeInPath)")
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
            }
        }
        .accessibilityElement(children: .combine)
    }

    private func open(_ pane: SettingsPane, _ part: SettingsPart?) {
        if let part { part.select() } else { pane.select() }
    }

    private func agentName(_ id: String) -> String {
        store.agents.first { $0.agent_id == id }?.label ?? store.setup?.first { $0.id == id }?.name ?? id
    }

    /// The path, read like the control it locks; a path dino doesn't show is listed as it is.
    private func item(_ path: String) -> Item {
        let parts = path.split(separator: ".", maxSplits: 1).map(String.init)
        let head = parts[0]
        let rest = parts.count > 1 ? parts[1] : ""
        func make(_ title: String, _ place: String, _ pane: SettingsPane?, _ part: SettingsPart? = nil) -> Item {
            Item(id: path, title: title, place: place, pane: pane, part: part)
        }
        switch (head, rest) {
        case ("policies", "default_agent"): return make("⌘N starts", "Agents", .agents)
        case ("policies", "allowed_agents"): return make("Agents you use", "Agents", .agents)
        case ("policies", "allow_bypass"): return make("Allow bypass permissions mode", "Agents → Permissions", .agents)
        case ("policies", "session_token_budget"): return make("Tokens per session", "Agents → Limits", .agents)
        case ("policies", "fallback_providers"): return make("Providers agents may fall back to", "Agents → Limits", .agents)
        case ("policies", "session_tools"): return make("Cross-session communication", "Experimental", .experimental)
        case ("policies", "worktree_trust"): return make("Trust worktrees when the repo is trusted", "Workspaces → Worktrees", .workspaces, .worktrees)
        case ("policies", "close_merged"): return make("Archive sessions after their PR merges or closes", "Workspaces → Worktrees", .workspaces, .worktrees)
        case ("routing", "proxy"): return make("Route agent traffic through dino", "Models & Providers → Providers", .models, .providers)
        case ("machine", "shell_integration"): return make("Shell integration", "Terminal", .terminal)
        case ("machine", "shell_agents"): return make("Show agents started in a shell in the sidebar", "Agents", .agents)
        case ("machine", "keep_awake"): return make("Keep your Mac awake while automations are scheduled", "Power", .power)
        case ("machine", "awake_while_working"): return make("Keep your Mac awake while agents work", "Power", .power)
        case ("machine", let r) where r == "lid" || r.hasPrefix("lid."): return make("Keep agents running with the lid closed", "Power", .power)
        case ("worktrees", "location"): return make("Worktree location", "Workspaces → Worktrees", .workspaces, .worktrees)
        case ("machine", "build_cache"), ("machine", "build_cache.enabled"): return make("Share one build cache across worktrees", "Workspaces → Worktrees", .workspaces, .worktrees)
        case ("machine", "build_cache.size_gb"): return make("Build cache size", "Workspaces → Worktrees", .workspaces, .worktrees)
        case ("worktrees", "branch_prefix"): return make("Branch prefix", "Workspaces → Worktrees", .workspaces, .worktrees)
        case ("experimental", let id):
            let title = ExperimentalFeature.all.first { $0.id == id }?.title ?? id
            return make(title, "Experimental", .experimental)
        case ("agents", let r):
            let p = r.split(separator: ".").map(String.init)
            let agent = agentName(p[0])
            let control = p.count > 1 ? ControlKind(rawValue: p[1])?.title ?? p[1] : "Defaults"
            return make("\(agent): \(control)", "Agents → New \(agent) Sessions", .agents)
        case ("repos", let r):
            // The repo's path has dots of its own: the variable is what follows its last ".env.".
            if let env = r.range(of: ".env.", options: .backwards) {
                let repo = (String(r[..<env.lowerBound]) as NSString).lastPathComponent
                return make("\(r[env.upperBound...]) in \(repo)", "Workspaces → Repositories", .workspaces, .repos)
            }
            return make((r as NSString).lastPathComponent, "Workspaces → Repositories", .workspaces, .repos)
        case ("ssh", let r):
            return make(r.split(separator: ".").first.map(String.init) ?? r, "Workspaces → SSH Hosts", .workspaces, .ssh)
        case ("fallbacks", let r):
            let agent = agentName(r.split(separator: ".").first.map(String.init) ?? r)
            return make("When \(agent) Hits a Limit", "Agents → Limits", .agents)
        case ("tmux", _): return make(rest, "tmux", .tmux)
        default: return make(path, "Only in settings.toml", nil)
        }
    }

    /// What it's set to, from the settings in effect (which the organization's values win in).
    private func value(_ path: String) -> String {
        guard let settings = store.settings,
              let data = try? JSONEncoder().encode(settings),
              let root = try? JSONSerialization.jsonObject(with: data)
        else { return "" }
        guard let v = Self.lookup(root, path.split(separator: ".").map(String.init)) else { return "" }
        switch (path, v) {
        case ("policies.session_token_budget", let n as NSNumber): return n.uint64Value == 0 ? "No limit" : LimitsSection.format(n.uint64Value)
        case ("policies.allowed_agents", let a as [String]) where a.isEmpty: return "All"
        case ("policies.allowed_agents", let a as [String]):
            return a.map { s in store.agents.first { $0.short == s }?.label ?? s }.joined(separator: ", ")
        case ("policies.default_agent", let s as String): return store.agents.first { $0.short == s }?.label ?? s
        case ("machine.build_cache.size_gb", let n as NSNumber): return "\(n.intValue) GB"
        default: return Self.format(v)
        }
    }

    private static func format(_ v: Any) -> String {
        switch v {
        case let n as NSNumber where CFGetTypeID(n) == CFBooleanGetTypeID(): n.boolValue ? "On" : "Off"
        case let n as NSNumber: n.stringValue
        case let s as String: s.isEmpty ? "None" : s
        case let a as [Any]: a.isEmpty ? "None" : a.map(format).joined(separator: ", ")
        case is NSNull: "Not set"
        default: "Set"
        }
    }

    /// `segments` in `json`, where a key may itself hold dots (a repo's path, a host).
    private static func lookup(_ json: Any, _ segments: [String]) -> Any? {
        guard !segments.isEmpty else { return json }
        guard let dict = json as? [String: Any] else { return nil }
        for n in 1...segments.count {
            if let next = dict[segments[..<n].joined(separator: ".")], let found = lookup(next, Array(segments[n...])) {
                return found
            }
        }
        return nil
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
                    Footnote("Keys are stored on this Mac in \(NSString(string: DinoEnvironment.home).abbreviatingWithTildeInPath)/keys, readable only by you. dino never shows a key again and never syncs it. Changes take effect immediately.")
                }
            }
            .formStyle(.grouped)
        }
        .confirmationDialog("Remove \(removing?.name ?? "")?", isPresented: Binding(get: { removing != nil }, set: { if !$0 { removing = nil } }), presenting: removing) { key in
            Button("Remove", role: .destructive) { store.setKey(key.name, value: nil) }
        } message: { _ in
            Text("To add it back later, you'll need to paste the key again.")
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
                Text("Set in your shell environment, which overrides a key stored here.")
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

/// Every agent dino knows: get it, sign in to it, which ones you use, what their new sessions
/// start with, and how much a session may use. Installing and signing in run the agent's own
/// commands in a shell, where you see them and answer them.
private struct AgentsPane: View {
    @EnvironmentObject var store: SettingsStore
    @EnvironmentObject var model: DinoModel
    /// Shells running an install or sign-in, by agent: asked again when one ends, and meanwhile,
    /// until the agent is installed or signed in.
    @State private var running: [String: (session: String, action: String)] = [:]
    @State private var showMore = false
    /// Settings → Models & Providers' providers, for the fallbacks.
    @State private var providers: [ProviderInfo] = []
    @AppStorage(UsingDisplay.key) private var usingDisplay = UsingDisplay.banner.rawValue

    /// The ones dino works with best, in this order; the rest are under More Agents.
    private static let featured = ["claude", "codex", "copilot", "cursor", "amp", "kimi", "qwen", "pi", "hermes", "codewhale", "opencode"]

    private var main: [AgentSetupInfo] {
        Self.featured.compactMap { id in store.setup?.first { $0.id == id } }
    }
    private var more: [AgentSetupInfo] {
        (store.setup ?? []).filter { !Self.featured.contains($0.id) }
    }

    /// One per agent that has controls; the free tier is its own, since it picks models itself.
    private var agents: [LauncherInfo] {
        var seen = Set<String>()
        return store.agents.filter { ($0.knobs?.any ?? false) && seen.insert($0.agent_id).inserted }
    }

    /// One per agent that can run on another route than its own: those it can fall back to.
    private var fallbackAgents: [LauncherInfo] {
        var seen = Set<String>()
        return store.agents.filter { !($0.formats ?? []).isEmpty && !$0.agent_id.hasSuffix("-free") && seen.insert($0.agent_id).inserted }
    }

    private func controls(_ agent: String) -> Binding<Controls> {
        Binding(
            get: { store.settings?.agents?[agent] ?? Controls() },
            set: { c in store.update { $0.agents = ($0.agents ?? [:]).merging([agent: c]) { $1 }.filter { $0.value != Controls() } } }
        )
    }

    var body: some View {
        Form {
            Section {
                if store.setup == nil {
                    HStack(spacing: 8) {
                        ProgressView().controlSize(.small)
                        Text("Looking for agents on this Mac…").foregroundStyle(.secondary)
                    }
                }
                ForEach(main) { a in AgentSetupRow(agent: a, running: running[a.id] != nil, act: act) }
                if !more.isEmpty {
                    DisclosureGroup("More Agents", isExpanded: $showMore) {
                        ForEach(more) { a in AgentSetupRow(agent: a, running: running[a.id] != nil, act: act) }
                    }
                }
            } header: {
                Text("On This Mac")
            } footer: {
                Footnote("Install and Sign In run the agent's own commands in a new shell, so you can watch them and answer any questions. dino never sees your sign-in details.")
            }
            AgentChoiceSection()
            Section {
                Toggle("Show agents started in a shell in the sidebar", isOn: Binding(
                    get: { store.settings?.machine.shell_agents ?? true },
                    set: { on in store.update { $0.machine.shell_agents = on } }
                ))
                .disabled(store.settings?.machine.shell_integration == false)
                .orgLocked("machine.shell_agents")
            } footer: {
                Footnote("When you run `claude`, `codex` or another agent in a dino shell, it's a session like any dino starts: in the sidebar with its turns, questions and tasks, its mode and model in the toolbar, and back on its conversation when dino restarts. When it exits, the shell is a plain shell again. Requires Shell integration (Terminal). To keep a shell a plain terminal, choose Keep as Terminal from its menu.")
            }
            Section {
                Picker("Show when an agent uses your Mac", selection: $usingDisplay) {
                    ForEach(UsingDisplay.allCases) { Text($0.label).tag($0.rawValue) }
                }
            } footer: {
                Footnote("Shows when an agent sees your screen, clicks or types in your apps, or controls a browser, whatever tool it uses. A banner you close stays hidden until the agent starts again. The session's menu always shows it and lets you stop the agent.")
            }
            if store.setup?.contains(where: { $0.id == "claude" && $0.installed }) == true {
                ClaudeAccountsSection(act: openShell)
                ClaudeTokenSection(act: openShell)
            }
            BypassSection()
            if agents.isEmpty {
                Section {
                    Text("None of your agents has a mode, model or effort to choose.").foregroundStyle(.secondary)
                }
            }
            // The organization may set one control and leave the others: each is locked on its own.
            ForEach(agents) { l in
                Section("New \(l.label) Sessions") {
                    ControlFields(knobs: l.knobs!, controls: controls(l.agent_id), lockPath: "agents.\(l.agent_id)")
                }
            }
            Section {} footer: {
                Footnote("New sessions start with these unless you pick something else when you start one. Default uses the agent's own settings. To change a running session, use its toolbar, or press ⇧⌘M for mode, ⇧⌘I for model, or ⇧⌘E for effort.")
            }
            // Agents → Limits: a session's budget, then where each agent goes at its limit.
            LimitsSection()
            ForEach(fallbackAgents) { l in
                FallbackSection(launcher: l, providers: providers)
            }
        }
        .formStyle(.grouped)
        .disabled(store.settings == nil)
        .onAppear { store.loadSetup() }
        .task {
            let all = await Task.detached { (try? DinoConnection(path: DinoEnvironment.socketPath).providers()) ?? [] }.value
            if all != providers { providers = all }
        }
        // Back from the shell: what it did shows here.
        .onReceive(NotificationCenter.default.publisher(for: NSWindow.didBecomeKeyNotification)) { note in
            if (note.object as? NSWindow)?.identifier?.rawValue.hasPrefix(SettingsView.windowID) == true { store.loadSetup() }
        }
        // While an install or sign-in runs, ask now and then, and once more when its shell ends.
        // A task, not a timer publisher: this view redraws with every session change.
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
        // Done once it did what it was for, even if its shell stays open.
        .onChange(of: store.setup) { _, setup in
            for (id, r) in running {
                guard let a = setup?.first(where: { $0.id == id }) else { continue }
                if r.action == "install" ? a.installed : a.signed_in == true { running[id] = nil }
            }
        }
    }

    /// Shows `session`, a shell dinod just opened, in the main window, in front.
    private func openShell(_ session: String) {
        model.pendingSelect = session
        NSApp.windows.first { w in w.isVisible && !(w.identifier?.rawValue.hasPrefix(SettingsView.windowID) ?? false) && w.canBecomeMain }?
            .makeKeyAndOrderFront(nil)
    }

    /// Runs it in a new shell, shown in the main window, where the user watches and answers it.
    private func act(_ agent: AgentSetupInfo, _ action: String) {
        // Read now, while the view is live: the reply comes after this copy of it is gone.
        let model = model
        store.agentAction(agent.id, action) { session in
            running[agent.id] = (session, action)
            model.pendingSelect = session
            NSApp.windows.first { w in w.isVisible && !(w.identifier?.rawValue.hasPrefix(SettingsView.windowID) ?? false) && w.canBecomeMain }?
                .makeKeyAndOrderFront(nil)
        }
    }
}

/// One agent: whether it's here and signed in, and the way to get it or sign in.
private struct AgentSetupRow: View {
    let agent: AgentSetupInfo
    /// Its install or sign-in shell is open.
    let running: Bool
    let act: (AgentSetupInfo, String) -> Void

    var body: some View {
        HStack(alignment: .center, spacing: 10) {
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(agent.name)
                    if let url = URL(string: agent.homepage), !agent.homepage.isEmpty {
                        Link(destination: url) {
                            Image(systemName: "arrow.up.right.square").foregroundStyle(.secondary)
                        }
                        .buttonStyle(.plain)
                        .help(agent.homepage)
                    }
                }
                Text(detail).font(.callout).foregroundStyle(.secondary)
            }
            Spacer()
            if running {
                ProgressView().controlSize(.small).help("Running in a shell in the main window")
            }
            status
            if !agent.installed {
                Button("Install…") { act(agent, "install") }
                    .help(agent.install)
            } else if let command = agent.sign_in, agent.signed_in != true {
                Button("Sign In…") { act(agent, "sign_in") }
                    .help(agent.sign_in_hint.map { "Opens \(agent.name). Type \($0) there to sign in." } ?? command)
            }
        }
        .padding(.vertical, 2)
    }

    private var detail: String {
        guard agent.installed else { return "Not installed" }
        let version = agent.version.map { "Version \($0)" } ?? "Installed"
        if agent.signed_in == false, let note = agent.sign_in_note {
            return "\(version) · \(note)"
        }
        if agent.signed_in == nil, let hint = agent.sign_in_hint {
            return "\(version) · To sign in, type \(hint) in \(agent.name)"
        }
        return version
    }

    @ViewBuilder private var status: some View {
        if agent.installed, let signedIn = agent.signed_in {
            HStack(spacing: 5) {
                Circle().fill(signedIn ? Color.green : Color.orange).frame(width: 7, height: 7)
                Text(signedIn ? (agent.account ?? "Signed in") : "Signed out").font(.callout).foregroundStyle(.secondary)
            }
        }
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
                Footnote("These variables are set for every session in the repo and its worktrees, from the next time a session starts or restarts. They're stored unencrypted, and when you're signed in they sync to your other Macs, matched by the repo's remote. Don't put passwords or tokens here.")
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
            known = await Task.detached { (try? DinoConnection(path: DinoEnvironment.socketPath).tree(folders: []))?.repos ?? [] }.value
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
    /// Why the host typed can't be added, said under the field.
    @State private var problem: String?

    private var hosts: [String: DinoSettings.SshHost] { store.settings?.ssh ?? [:] }
    private var suggested: [String] { store.configHosts.filter { hosts[$0] == nil } }

    private func add(_ host: String) {
        let host = host.trimmingCharacters(in: .whitespaces)
        if let why = Self.problem(with: host, hosts: hosts) {
            problem = why
            return
        }
        store.update { $0.ssh = ($0.ssh ?? [:]).merging([host: .init(folder: "")]) { a, _ in a } }
        draft = ""
        problem = nil
        entering = false
    }

    /// What's wrong with a host as typed, if anything: it goes to ssh as one argument.
    static func problem(with host: String, hosts: [String: DinoSettings.SshHost]) -> String? {
        if host.isEmpty { return "Type a host: a name from ~/.ssh/config, or user@host." }
        if host.hasPrefix("-") { return "A host can't start with “-”." }
        if host.contains(where: \.isWhitespace) { return "A host can't contain spaces. Put ports and options in ~/.ssh/config." }
        if hosts[host] != nil { return "\(host) is already in the list." }
        return nil
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
                        .accessibilityLabel("Remove \(host)")
                    }
                    .orgLocked("ssh.\(host)")
                }
                if entering {
                    VStack(alignment: .leading, spacing: 4) {
                        HStack {
                            TextField("Host", text: $draft, prompt: Text("devbox or user@host"))
                                .textFieldStyle(.roundedBorder)
                                .onSubmit { add(draft) }
                            Button("Add") { add(draft) }.disabled(draft.trimmingCharacters(in: .whitespaces).isEmpty)
                            Button("Cancel") {
                                entering = false
                                draft = ""
                                problem = nil
                            }
                        }
                        if let problem {
                            Text(problem).font(.caption).foregroundStyle(.red)
                        }
                    }
                    .onChange(of: draft) { _, _ in if problem != nil { problem = nil } }
                }
                if hosts.isEmpty && !entering {
                    Text("No hosts yet.").foregroundStyle(.secondary)
                }
            } header: {
                Text("SSH Hosts")
            } footer: {
                Footnote("You can start new sessions on these machines. dino connects with ssh and starts the agent there, so the agent must be installed on that machine. Usernames, keys and ports come from your ~/.ssh/config. The folder is where sessions start if you don't choose one. Leave it empty to use the home folder.")
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
                    Footnote("Hosts in your ~/.ssh/config appear in this menu.")
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
                    .accessibilityLabel(shown ? "Hide the value" : "Show the value")
                Button("Edit") { draft = value }
                Button(action: remove) { Image(systemName: "minus.circle") }
                    .buttonStyle(.borderless)
                    .help("Remove \(key)")
                    .accessibilityLabel("Remove \(key)")
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
