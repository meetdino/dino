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
        /// Close a session and its worktree after its PR merges; nil from an older dinod.
        var close_merged: Bool?

        func allows(_ short: String) -> Bool {
            short == "shell" || allowed_agents.isEmpty || allowed_agents.contains(short)
        }
    }
    struct Machine: Codable, Equatable { var onboarded: Bool }
    var routing: Routing
    var policies: Policies
    var machine: Machine
}

/// A provider key's name and where it comes from; dinod never sends values.
struct KeyInfo: Codable, Identifiable, Equatable {
    let name: String
    let purpose: String?
    /// "dino", "environment", or nil: not set.
    let source: String?
    var id: String { name }
}

private struct SettingsResponse: Decodable { let settings: DinoSettings }
private struct KeysResponse: Decodable { let keys: [KeyInfo] }

extension DinoConnection {
    func settings() throws -> DinoSettings {
        try JSONDecoder().decode(SettingsResponse.self, from: send(["type": "settings"])).settings
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
    @Published var keys: [KeyInfo] = []
    @Published var agents: [LauncherInfo] = []
    @Published var error: String?

    func load() {
        run { c in (try c.settings(), try c.keys(), try c.allLaunchers()) } done: {
            self.settings = $0.0
            self.keys = $0.1
            self.agents = $0.2
        }
    }

    func update(_ change: (inout DinoSettings) -> Void) {
        guard var next = settings else { return }
        change(&next)
        settings = next
        let saved = next
        run { c in try c.setSettings(saved) } done: { _ in }
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
    case account, general, policies, routing, keys
    var id: String { rawValue }

    var title: String {
        switch self {
        case .account: "Dino Account"
        case .general: "General"
        case .policies: "Policies"
        case .routing: "Routing"
        case .keys: "Keys"
        }
    }

    var icon: String {
        switch self {
        case .account: "person.crop.circle.fill"
        case .general: "gearshape.fill"
        case .policies: "checkmark.shield.fill"
        case .routing: "arrow.triangle.branch"
        case .keys: "key.fill"
        }
    }

    var tint: Color {
        switch self {
        case .account: .blue
        case .general: .gray
        case .policies: .indigo
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
                case .policies: PoliciesPane()
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
private struct Footnote: View {
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
            } header: {
                Text("Fan-out")
            } footer: {
                Footnote("Claude asks whether to trust each new folder, and every fan-out worktree is one. When you've trusted the repo, dino tells Claude its worktrees are trusted too, and forgets them when the fan-out closes. Codex does this on its own.")
            }
            Section {
                Toggle("Close sessions after their PR merges", isOn: Binding(
                    get: { policies?.close_merged ?? false },
                    set: { on in store.update { $0.policies.close_merged = on } }
                ))
            } header: {
                Text("Pull requests")
            } footer: {
                Footnote("When a session's PR merges, dino stops the session once its agent is idle and removes the worktree dino made for it, with its branch. A worktree with changes or commits that aren't pushed is kept. Sessions outside a dino worktree stay open.")
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
