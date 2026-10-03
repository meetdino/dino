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

        func allows(_ short: String) -> Bool {
            short == "shell" || allowed_agents.isEmpty || allowed_agents.contains(short)
        }
    }
    struct Machine: Codable, Equatable {
        var onboarded: Bool
        /// Keep the Mac from idle-sleeping while tasks are scheduled; nil from an older dinod.
        var keep_awake: Bool?
        /// Shells mark their prompts and say where they are; nil from an older dinod (on there).
        var shell_integration: Bool?
        /// Keeping agents running with the lid closed; nil from an older dinod.
        var lid: Lid?
        /// Which Claude Code sessions get the Claude subscription token; nil from an older dinod.
        var claude_token: ClaudeTokenUse?
        /// Look for updates once a day; nil from an older dinod (on there).
        var check_updates: Bool?
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

        /// dinod's defaults: StartWith.last, QuickTerminal.Key.commandGrave, hide on click, ask on quit.
        static let defaults = Terminal(start_with: "last", quick_key: "cmd-grave", quick_autohide: true, on_quit: "")

        /// As the app keeps them for itself (it reads them there at launch, before dinod answers).
        @MainActor static var mirrored: Terminal {
            let d = UserDefaults.standard
            return Terminal(
                start_with: d.string(forKey: StartWith.key) ?? defaults.start_with,
                quick_key: d.string(forKey: QuickTerminal.Key.storageKey) ?? defaults.quick_key,
                quick_autohide: d.object(forKey: QuickTerminal.autohideKey) as? Bool ?? defaults.quick_autohide,
                on_quit: d.string(forKey: QuitChoice.key) ?? defaults.on_quit
            )
        }

        /// Make the app's copy these, and claim a changed shortcut.
        @MainActor func mirror() {
            let before = Terminal.mirrored
            guard before != self else { return }
            let d = UserDefaults.standard
            d.set(start_with, forKey: StartWith.key)
            d.set(quick_key, forKey: QuickTerminal.Key.storageKey)
            d.set(quick_autohide, forKey: QuickTerminal.autohideKey)
            d.set(on_quit, forKey: QuitChoice.key)
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
    /// Hosts in ~/.ssh/config, to suggest; nil from an older dinod.
    let ssh_config_hosts: [String]?
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
    @Published var keys: [KeyInfo] = []
    @Published var agents: [LauncherInfo] = []
    /// Every known agent, installed or not; nil until first asked.
    @Published var setup: [AgentSetupInfo]?
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
    case account, general, agents, models, workspaces, policies
    var id: String { rawValue }

    var title: String {
        switch self {
        case .account: "Dino Account"
        case .general: "General"
        case .agents: "Agents"
        case .models: "Models & Providers"
        case .workspaces: "Workspaces"
        case .policies: "Policies"
        }
    }

    var icon: String {
        switch self {
        case .account: "person.crop.circle.fill"
        case .general: "gearshape.fill"
        case .agents: "cpu.fill"
        case .models: "cube.fill"
        case .workspaces: "folder.fill"
        case .policies: "checkmark.shield.fill"
        }
    }

    var tint: Color {
        switch self {
        case .account: .blue
        case .general: .gray
        case .agents: .purple
        case .models: .pink
        case .workspaces: .teal
        case .policies: .indigo
        }
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
        UserDefaults.standard.set(pane.rawValue, forKey: "settingsTab")
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
                case .models, .workspaces: PartedPane(pane: pane)
                case .policies: PoliciesPane()
                }
                StoreError()
            }
            .navigationTitle(pane.title)
        }
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

struct GeneralPane: View {
    @EnvironmentObject var store: SettingsStore
    @AppStorage(QuitChoice.key) private var quitChoice = ""
    @AppStorage(StartWith.key) private var startWith = StartWith.last.rawValue
    @AppStorage(QuickTerminal.Key.storageKey) private var quickKey = QuickTerminal.Key.commandGrave.rawValue
    @AppStorage(QuickTerminal.autohideKey) private var quickAutohide = true
    @State private var quickTaken = false
    @State private var isDefault = false
    @State private var makingDefault = false

    /// A section asked for when Settings opens ("tmux", from the tab strip's suggestion): shown
    /// at once rather than below the fold.
    static let scrollKey = "settings.general.scrollTo"

    var body: some View {
        ScrollViewReader { proxy in
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
                Picker("When dino opens", selection: $startWith) {
                    Text("Your last session").tag(StartWith.last.rawValue)
                    Text("A new shell").tag(StartWith.shell.rawValue)
                }
                Toggle("Shell integration", isOn: Binding(
                    get: { store.settings?.machine.shell_integration ?? true },
                    set: { on in store.update { $0.machine.shell_integration = on } }
                ))
                .disabled(store.settings == nil)
                .orgLocked("machine.shell_integration")
                LabeledContent("Ghostty config") {
                    let files = GhosttyConfig.loaded.map { NSString(string: $0).abbreviatingWithTildeInPath }
                    Text(files.isEmpty ? "None: Ghostty's defaults" : files.joined(separator: "\n"))
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.trailing)
                        .textSelection(.enabled)
                }
                if !GhosttyConfig.skipped.isEmpty {
                    LabeledContent("Left out") {
                        Text(GhosttyConfig.skipped.joined(separator: "\n"))
                            .foregroundStyle(.secondary)
                            .multilineTextAlignment(.trailing)
                            .textSelection(.enabled)
                    }
                }
            } header: {
                Text("Terminal")
            } footer: {
                Footnote("With no session to come back to, dino opens a shell; ⌘T opens another where you are. Shell integration has zsh and bash mark each prompt and say which folder they're in, as in Ghostty, without touching your startup files (new shells). Panes take your Ghostty font, colors, cursor and keybinds, and edits as you save them; dino keeps its own shortcuts and starts each pane itself, so command and working-directory don't apply.")
            }
            Section {
                Picker("Quick terminal", selection: Binding(
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
                    Text("Another app has this shortcut. Choose another.")
                        .font(.caption)
                        .foregroundStyle(.orange)
                }
                Toggle("Hide it when you click elsewhere", isOn: $quickAutohide)
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
                Footnote("The quick terminal drops down from the top of the screen from any app, and keeps its shell while it's hidden. As the default terminal, dino opens scripts (.command, .tool), programs and man pages you open from the Finder or other apps; a folder opened with dino, or \"New dino Shell at Folder\" in the Finder's Services menu, opens a shell there.")
            }
            .onAppear {
                isDefault = Opening.isDefault
                quickTaken = QuickTerminal.Key.current != .off && !QuickTerminal.shared.registered
            }
            TmuxSection().id("tmux")
            Section {
                Toggle("Keep your Mac awake while tasks are scheduled", isOn: Binding(
                    get: { store.settings?.machine.keep_awake ?? false },
                    set: { on in store.update { $0.machine.keep_awake = on } }
                ))
                .disabled(store.settings == nil)
                .orgLocked("machine.keep_awake")
            } footer: {
                Footnote("So scheduled tasks run on time. Closing the lid still sleeps it, unless Lid closed below keeps it awake; missed tasks run once when it wakes.")
            }
            LidSection()
            UpdatesSection()
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
        // dinod keeps them too, so they sync to your other Macs.
        .onChange(of: DinoSettings.Terminal(start_with: startWith, quick_key: quickKey, quick_autohide: quickAutohide, on_quit: quitChoice)) { _, t in
            if store.settings?.terminal != t { store.update { $0.terminal = t } }
        }
        .onAppear {
            guard let target = UserDefaults.standard.string(forKey: Self.scrollKey) else { return }
            UserDefaults.standard.removeObject(forKey: Self.scrollKey)
            DispatchQueue.main.async { withAnimation { proxy.scrollTo(target, anchor: .top) } }
        }
        }
    }
}

/// Settings → General → tmux. For people who live in tmux, and theirs stays theirs: dino never
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
            Toggle("Show my agents in tmux", isOn: Binding(get: { tmux.show_agents }, set: { on in change { $0.show_agents = on } }))
            if tmux.show_agents {
                Picker("Put them in", selection: Binding(
                    get: { tmux.session.isEmpty ? "attached" : "named" },
                    set: { v in change { $0.session = v == "attached" ? "" : (DinoSettings.Tmux.valid(session) ? session : "dino") } }
                )) {
                    Text("A session of their own").tag("named")
                    Text("The session you're in").tag("attached")
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
        } header: {
            Text("tmux")
        } footer: {
            Footnote("Your agents show up as tmux windows (running `dino attach`), so `tmux attach` from anywhere reaches them. They still run in dino: closing a window, or quitting tmux, leaves the agent running and its window comes back. dino never starts tmux for this, never changes your tmux config or your own windows. New tabs run `tmux new -A -s` with the name you give. For your status bar: set -g status-right '#(dino status --tmux)'.")
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
                Text("Letters, digits, - _ and . only").font(.caption).foregroundStyle(.orange)
            }
        }
    }

    private func commit(_ field: Field) {
        switch field {
        case .session where DinoSettings.Tmux.valid(session): change { $0.session = session }
        case .tabs where DinoSettings.Tmux.valid(tabs): change { $0.new_tabs = tabs }
        default: break
        }
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
                Toggle("Archive sessions after their PR merges or closes", isOn: Binding(
                    get: { policies?.close_merged ?? false },
                    set: { on in store.update { $0.policies.close_merged = on } }
                ))
                .orgLocked("policies.close_merged")
            } header: {
                Text("Pull requests")
            } footer: {
                Footnote("When a session's PR merges or is closed, dino archives it once its agent is idle, so the conversation can be picked up again. After a merge, the worktree dino made for it is removed too if nothing in it would be lost; after a close it stays (see Workspaces → Worktrees), since the work never landed. Unarchive it to pick up where it left off, worktree and all. Sessions outside a dino worktree stay open.")
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
                Footnote("Counts input, cached and output tokens, like the sidebar. A session over its budget gets an error on its next model call. Only sessions routed through dino (see Models & Providers → Providers); applies to running ones too.")
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

/// Whether agents' traffic goes through dino, and the free tier: the top of Models & Providers.
/// Sections only, for the providers' form to hold.
struct RoutingSections: View {
    @EnvironmentObject var store: SettingsStore

    private func has(_ key: String) -> Bool { store.keys.contains { $0.name == key && $0.source != nil } }

    var body: some View {
        Section {
            Toggle("Route agent traffic through dino", isOn: Binding(
                get: { store.settings?.routing.proxy ?? true },
                set: { on in store.update { $0.routing.proxy = on } }
            ))
            .disabled(store.settings == nil)
            .orgLocked("routing.proxy")
            LabeledContent("Free tier models") {
                Text(has("NVIDIA_API_KEY") ? "NVIDIA NIM" : "Needs an NVIDIA key (see API Keys)")
                    .foregroundStyle(has("NVIDIA_API_KEY") ? .primary : .secondary)
            }
            LabeledContent("Free tier picks the model per turn") {
                Text(has("TYPESAFE_API_KEY") ? "Jev, then built-in rules" : "Built-in rules")
            }
        } header: {
            Text("Routing")
        } footer: {
            Footnote("dino's local proxy counts tokens per session, serves the free tier and connects agents to the providers below. Off, agents talk to their providers directly. Applies to sessions you start from now on.")
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
                    Footnote("Keys stay on this Mac in \(NSString(string: DinoEnvironment.home).abbreviatingWithTildeInPath)/keys, readable only by you, and dino never shows them again or syncs them to your other Macs. They take effect immediately. Keychain storage comes with signed releases.")
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

/// Every agent dino knows: get it, sign in to it, and what its new sessions start with. Installing
/// and signing in run the agent's own commands in a shell, where you see them and answer them.
private struct AgentsPane: View {
    @EnvironmentObject var store: SettingsStore
    @EnvironmentObject var model: DinoModel
    /// Shells running an install or sign-in, by agent: asked again when one ends, and meanwhile,
    /// until the agent is installed or signed in.
    @State private var running: [String: (session: String, action: String)] = [:]
    @State private var showMore = false

    /// The ones dino works with best, in this order; the rest are under More Agents.
    private static let featured = ["claude", "codex", "kimi", "qwen", "pi", "hermes"]

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
                Footnote("Install and Sign In run the agent's own commands in a new shell, where you can see them and answer their questions. dino never sees your logins.")
            }
            if store.setup?.contains(where: { $0.id == "claude" && $0.installed }) == true {
                ClaudeTokenSection(act: openShell)
            }
            if agents.isEmpty {
                Section {
                    Text("No agent dino can start has a mode, model or effort to choose.").foregroundStyle(.secondary)
                }
            }
            // The organization may set one control and leave the others: each is locked on its own.
            ForEach(agents) { l in
                Section("New \(l.label) Sessions") {
                    ControlFields(knobs: l.knobs!, controls: controls(l.agent_id), lockPath: "agents.\(l.agent_id)")
                }
            }
            Section {} footer: {
                Footnote("New sessions start with these unless you choose otherwise in New Session…. Default is whatever the agent's own settings say. Change a running session from its toolbar: ⇧⌘M mode, ⇧⌘I model, ⇧⌘E effort.")
            }
        }
        .formStyle(.grouped)
        .disabled(store.settings == nil)
        .onAppear { store.loadSetup() }
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
                    .help(agent.sign_in_hint.map { "Opens \(agent.name); type \($0) there" } ?? command)
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
            return "\(version) · signs in with \(hint) inside \(agent.name)"
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
                Footnote("Set for every session dino starts in the repo or one of its worktrees, from the next start or restart. Values are kept in settings.toml, readable only by you, and when you're signed in they sync to your other Macs, matched by the repo's remote. Keep passwords and tokens out of them.")
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
                        .accessibilityLabel("Remove \(host)")
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
