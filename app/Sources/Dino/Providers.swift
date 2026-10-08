import AppKit
import SwiftUI

/// Where models come from besides the agents' own accounts: OpenRouter, model servers on this
/// Mac and coding plans, as dinod last looked (see `crates/dino-core/src/providers.rs`).
struct ProviderInfo: Codable, Identifiable, Equatable {
    let id: String
    let name: String
    let base: String
    /// "anthropic", "chat", "responses".
    let formats: [String]
    let local: Bool
    let connected: Bool
    let key: String?
    let version: String?
    let account: ProviderAccount?
    let error: String?
    /// A coding plan, connected with a pasted key; nil for the others (and from an older dinod).
    let plan: PlanInfo?
}

/// A coding plan's preset, as dinod gives it (see `crates/dino-core/src/plans.rs`).
struct PlanInfo: Codable, Equatable {
    let blurb: String?
    /// Where its URLs come from.
    let docs: String
    let keys_page: String?
    let terms: String?
    /// The generic entry: it takes a base URL too.
    let custom: Bool
    let models_note: String?
}

struct ProviderAccount: Codable, Equatable {
    let label: String?
    let usage: Double?
    let limit: Double?
    let credits: Double?
    let credits_used: Double?
    let free_tier: Bool?
}

struct Reason: Codable, Equatable, Hashable {
    let text: String
    let source: String?
}

/// Whether an agent can use a model, as dinod judges it from what the provider says and
/// dino's curated notes.
struct Verdict: Codable, Equatable, Identifiable {
    let agent: String
    let name: String
    /// "works", "caveat", "no".
    let status: String
    let reasons: [Reason]
    let via: String?
    let translated: Bool
    let recommended: Bool
    var id: String { agent }
}

struct ProviderModel: Codable, Equatable, Identifiable {
    let id: String
    let name: String
    let provider: String
    let context: UInt64?
    let max_output: UInt64?
    let tools: Bool?
    let reasoning: Bool?
    let vision: Bool
    let free: Bool
    let local: Bool
    let price_in: Double?
    let price_out: Double?
    let agents: [Verdict]

    var recommended: Verdict? { agents.first { $0.recommended } }
}

private struct ProvidersResponse: Decodable { let providers: [ProviderInfo] }
struct ModelsResponse: Decodable {
    let models: [ProviderModel]
    let loading: Bool
    let error: String?
}

extension DinoConnection {
    func providers() throws -> [ProviderInfo] {
        try JSONDecoder().decode(ProvidersResponse.self, from: send(["type": "providers"])).providers
    }

    func models(_ provider: String) throws -> ModelsResponse {
        try JSONDecoder().decode(ModelsResponse.self, from: send(["type": "models", "provider": provider]))
    }

    /// The sign-in page for `provider`; dinod waits for the browser to come back and keeps the key.
    fileprivate func connect(_ provider: String) throws -> URL? {
        try (JSONSerialization.jsonObject(with: send(["type": "connect_provider", "provider": provider])) as? [String: Any])?["url"]
            .flatMap { $0 as? String }
            .flatMap(URL.init(string:))
    }

    fileprivate func disconnect(_ provider: String) throws {
        _ = try send(["type": "disconnect_provider", "provider": provider])
    }

    /// Connect coding plan `plan` with `key` (and the generic entry's `base`): dinod checks it with
    /// the plan when it can and keeps it; it's never sent back.
    fileprivate func connectPlan(_ plan: String, key: String, base: String?) throws {
        _ = try send(["type": "connect_plan", "plan": plan, "key": key, "base": base.map { $0 as Any } ?? NSNull()])
    }
}

/// Settings → Providers' view of dinod: asks now and then while the pane is open.
@MainActor
final class ProvidersStore: ObservableObject {
    @Published var providers: [ProviderInfo] = []
    @Published var models: [String: [ProviderModel]] = [:]
    @Published var loading: Set<String> = []
    @Published var errors: [String: String] = [:]
    @Published var error: String?
    /// Providers whose sign-in page is open in the browser.
    @Published var connecting: Set<String> = []
    /// The coding plan whose key is being checked and saved.
    @Published var savingPlan: String?
    /// Why a coding plan's key wasn't taken, by plan.
    @Published var planErrors: [String: String] = [:]

    /// Connect coding plan `id`; `done` runs once it's connected.
    func connectPlan(_ id: String, key: String, base: String?, done: @escaping @MainActor () -> Void) {
        if savingPlan != id { savingPlan = id }
        if planErrors[id] != nil { planErrors[id] = nil }
        Task.detached {
            let failure: String?
            do {
                try DinoConnection(path: DinoEnvironment.socketPath).connectPlan(id, key: key, base: base)
                failure = nil
            } catch DinoError.daemon(let message) {
                failure = message
            } catch {
                failure = "\(error)"
            }
            await MainActor.run {
                if self.savingPlan != nil { self.savingPlan = nil }
                if let failure {
                    self.planErrors[id] = failure
                } else {
                    done()
                    self.load()
                }
            }
        }
    }

    func connect(_ id: String) {
        connecting.insert(id)
        run { c in try c.connect(id) } done: { url in
            if let url { NSWorkspace.shared.open(url) }
        }
    }

    func disconnect(_ id: String) {
        run { c in try c.disconnect(id) } done: { self.load() }
    }

    func load() {
        run { c in
            let providers = try c.providers()
            // OpenRouter's list is public; a local server's only while it runs.
            let asked = providers.filter { $0.connected || $0.id == "openrouter" }.map(\.id)
            return (providers, try asked.map { ($0, try c.models($0)) })
        } done: { providers, lists in
            if self.providers != providers { self.providers = providers }
            // Done once it's connected, or said why not.
            for p in providers where self.connecting.contains(p.id) && (p.connected || p.error != nil) {
                self.connecting.remove(p.id)
            }
            let running = Set(providers.filter { $0.connected || $0.id == "openrouter" }.map(\.id))
            // Only what changed: each change redraws every model row.
            for (id, r) in lists {
                if self.models[id] != r.models { self.models[id] = r.models }
                if self.errors[id] != r.error { self.errors[id] = r.error }
                if r.loading != self.loading.contains(id) {
                    if r.loading { self.loading.insert(id) } else { self.loading.remove(id) }
                }
            }
            for id in self.models.keys where !running.contains(id) { self.models[id] = nil }
        }
    }

    private func run<T: Sendable>(_ work: @escaping @Sendable (DinoConnection) throws -> T, done: @escaping @MainActor (T) -> Void) {
        Task.detached {
            do {
                let value = try work(DinoConnection(path: DinoEnvironment.socketPath))
                await MainActor.run {
                    if self.error != nil { self.error = nil }
                    done(value)
                }
            } catch {
                await MainActor.run { if self.error != "\(error)" { self.error = "\(error)" } }
            }
        }
    }
}

/// Settings → Providers: whether traffic goes through dino, OpenRouter, ChatGPT, the model servers
/// on this Mac, and coding plans. Their models are on their own page (`ModelBrowser`).
struct ProvidersPane: View {
    @StateObject private var store = ProvidersStore()
    /// The coding plan whose key is being pasted.
    @State private var editingPlan: String?

    var body: some View {
        Form {
            RoutingSections()
            Section {
                ForEach(Array(store.providers.filter { $0.plan == nil }.enumerated()), id: \.element.id) { i, p in
                    ProviderRow(provider: p, count: store.models[p.id]?.count, loading: store.loading.contains(p.id), error: store.errors[p.id] ?? p.error,
                                connecting: store.connecting.contains(p.id), connect: { store.connect(p.id) }, disconnect: { store.disconnect(p.id) })
                        .settingAnchor(i == 0 ? "providers-list" : "provider-\(p.id)")
                }
                if store.providers.isEmpty {
                    Text("Asking dinod…").foregroundStyle(.secondary).settingAnchor("providers-list")
                }
            } header: {
                Text("Providers")
            } footer: {
                Footnote("Connect signs you in to OpenRouter in your browser and creates a key in your account (see openrouter.ai/keys). The key stays on this Mac. Sign in with ChatGPT lets your agents use your ChatGPT plan, up to the weekly cap you set for dino in ChatGPT. Nothing is billed beyond that cap. Model servers running on this Mac are found automatically. Their models are in Models.")
            }
            CodingPlans(store: store, editing: $editingPlan)
                .settingAnchor("coding-plans")
        }
        .formStyle(.grouped)
        .onAppear { store.load() }
        // A task, not a timer publisher: fresh lists arrive while dinod fetches them.
        .task {
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(store.loading.isEmpty && store.connecting.isEmpty ? 10 : 1))
                store.load()
            }
        }
    }
}

/// A model as the browser shows and searches it: what its row says, worked out once per list
/// rather than on every keystroke and redraw.
struct ModelItem: Identifiable, Equatable {
    let model: ProviderModel
    let id: String
    /// Lowercased id and name, for search.
    let haystack: String
    let local: Bool
    let facts: String

    init(_ m: ProviderModel, local: Bool) {
        model = m
        id = m.provider + "\u{0}" + m.id
        haystack = (m.id + " " + m.name).lowercased()
        self.local = local
        var parts: [String] = []
        if let c = m.context { parts.append("\(Self.tokens(c)) context") }
        if m.tools == false { parts.append("no tools") }
        if m.local {
            parts.append("Local")
        } else if m.free {
            parts.append("Free")
        } else if let i = m.price_in, let o = m.price_out {
            parts.append(String(format: "$%.2f / $%.2f per M", i, o))
        }
        facts = parts.joined(separator: " · ")
    }

    static func tokens(_ n: UInt64) -> String {
        n >= 1_000_000 && n % 1_000_000 == 0 ? "\(n / 1_000_000)M" : "\((n + 512) / 1024)k"
    }
}

/// Settings → Models: every model the providers serve, searchable, with the agents each works in.
/// A lazy list on a page of its own: only the rows on screen exist, each redrawn only when its own
/// model changes, so typing in the search stays immediate however many models there are.
struct ModelBrowser: View {
    @EnvironmentObject var settings: SettingsStore
    @StateObject private var store = ProvidersStore()
    @State private var search = ""
    /// Only models this agent can run ("" for any).
    @State private var worksIn = ""
    @State private var freeOnly = false
    @State private var localOnly = false
    /// Every model, worked out once per list.
    @State private var items: [ModelItem] = []

    private var rows: [ModelItem] {
        let q = search.trimmingCharacters(in: .whitespaces).lowercased()
        guard !q.isEmpty || freeOnly || localOnly || !worksIn.isEmpty else { return items }
        return items.filter { m in
            (q.isEmpty || m.haystack.contains(q))
                && (!freeOnly || m.model.free)
                && (!localOnly || m.local)
                && (worksIn.isEmpty || m.model.agents.contains { $0.agent == worksIn && $0.status != "no" })
        }
    }

    /// Every agent verdicts mention, by name.
    private var agents: [(id: String, name: String)] {
        var seen = Set<String>()
        return (items.first?.model.agents ?? []).sorted { $0.name < $1.name }.filter { seen.insert($0.agent).inserted }.map { ($0.agent, $0.name) }
    }

    /// Agents that can start here: installed and allowed.
    private var startable: Set<String> {
        Set(settings.agents.filter { settings.settings?.policies.allows($0.short) ?? true }.map(\.agent_id))
    }

    var body: some View {
        let rows = rows
        let startable = startable
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                HStack(spacing: 5) {
                    Image(systemName: "magnifyingglass").foregroundStyle(.secondary).font(.system(size: 12))
                    TextField("Search models", text: $search, prompt: Text("Search models"))
                        .textFieldStyle(.plain)
                }
                .padding(.horizontal, 7)
                .frame(height: 26)
                .background(RoundedRectangle(cornerRadius: 7, style: .continuous).fill(.quaternary.opacity(0.7)))
                Picker("Works in", selection: $worksIn) {
                    Text("Any agent").tag("")
                    ForEach(agents, id: \.id) { a in Text(a.name).tag(a.id) }
                }
                .labelsHidden()
                .fixedSize()
                Toggle("Free", isOn: $freeOnly).toggleStyle(.button)
                Toggle("Local", isOn: $localOnly).toggleStyle(.button)
            }
            .padding(.horizontal, 30)
            .padding(.vertical, 10)
            .settingAnchor("model-browser")
            HStack {
                Text(items.isEmpty ? "" : rows.count == items.count ? "\(items.count) models" : "\(rows.count) of \(items.count) models")
                Spacer()
                Text("✓ works · ~ with caveats · ✗ doesn't · hover for why")
            }
            .font(.caption)
            .foregroundStyle(.secondary)
            .padding(.horizontal, 32)
            .padding(.bottom, 6)
            Divider()
            if rows.isEmpty {
                ContentUnavailableView {
                    Label(items.isEmpty ? "Asking providers for their models…" : "No models match", systemImage: items.isEmpty ? "hourglass" : "magnifyingglass")
                } description: {
                    Text(items.isEmpty ? "OpenRouter's list is public. Connect a provider in Providers for its own." : "Try fewer words, or turn off Free or Local.")
                }
                .frame(maxHeight: .infinity)
            } else {
                ScrollView {
                    LazyVStack(spacing: 0) {
                        ForEach(rows) { m in
                            ModelRowView(item: m, highlight: worksIn, startable: startable)
                                .equatable()
                            Divider().padding(.leading, 30)
                        }
                    }
                    .padding(.bottom, 12)
                }
            }
        }
        .onAppear { store.load() }
        .onChange(of: store.models) { _, _ in rebuild() }
        .onChange(of: store.providers) { _, _ in rebuild() }
        .task {
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(store.loading.isEmpty ? 10 : 1))
                store.load()
            }
        }
    }

    /// What the rows show, from the lists as dinod last sent them; nothing changes if they didn't.
    private func rebuild() {
        let local = Set(store.providers.filter(\.local).map(\.id))
        let next = store.providers.flatMap { store.models[$0.id] ?? [] }.map { ModelItem($0, local: local.contains($0.provider)) }
        if next != items { items = next }
    }
}

private struct ProviderRow: View {
    let provider: ProviderInfo
    let count: Int?
    let loading: Bool
    let error: String?
    let connecting: Bool
    let connect: () -> Void
    let disconnect: () -> Void
    /// A coding plan's: paste another key.
    var changeKey: (() -> Void)?
    @State private var confirming = false

    var body: some View {
        HStack(alignment: .center, spacing: 10) {
            VStack(alignment: .leading, spacing: 2) {
                Text(provider.name)
                Text(detail).font(.callout).foregroundStyle(.secondary)
                if let error {
                    Text(error).font(.callout).foregroundStyle(.orange)
                }
            }
            Spacer()
            if loading {
                ProgressView().controlSize(.small).help("Asking \(provider.name) for its models")
            }
            HStack(spacing: 5) {
                Circle().fill(dot).frame(width: 7, height: 7)
                Text(state).font(.callout).foregroundStyle(.secondary)
            }
            if provider.key != nil {
                if connecting {
                    ProgressView().controlSize(.small).help("Finish connecting in your browser")
                } else if provider.connected {
                    if let changeKey {
                        Button("Change Key…", action: changeKey)
                    }
                    if chatgpt, let cap = URL(string: "https://chatgpt.com/#settings/Usage") {
                        Link("Weekly cap…", destination: cap)
                            .help("Set how much of your ChatGPT plan dino may use each week, in ChatGPT → Settings → Usage")
                    }
                    Button(chatgpt ? "Sign Out…" : "Disconnect…") { confirming = true }
                } else {
                    Button(chatgpt ? "Sign in with ChatGPT…" : "Connect…", action: connect)
                        .help(chatgpt ? "Sign in with ChatGPT in your browser, and allow dino to use your plan" : "Sign in to \(provider.name) in your browser")
                }
            }
        }
        .padding(.vertical, 2)
        .confirmationDialog(chatgpt ? "Sign out of ChatGPT?" : "Disconnect \(provider.name)?", isPresented: $confirming) {
            Button(chatgpt ? "Sign Out" : "Disconnect", role: .destructive, action: disconnect)
        } message: {
            Text(chatgpt ? "Your agents will stop using your ChatGPT plan." : "dino deletes its copy of the key. The key stays active in your \(provider.plan?.custom == true ? "provider" : provider.name) account until you delete it there.")
        }
    }

    /// Sign in with ChatGPT: the plan, not a key.
    private var chatgpt: Bool { provider.id == "chatgpt" }

    private var state: String {
        if provider.local { return provider.connected ? "Running" : "Not running" }
        if connecting { return "Waiting for your browser" }
        if chatgpt { return provider.connected ? "Signed in" : "Not signed in" }
        return provider.connected ? "Connected" : "Not connected"
    }

    private var dot: Color { provider.connected ? .green : .secondary.opacity(0.5) }

    private var detail: String {
        var parts: [String] = []
        if provider.local {
            parts.append(provider.connected ? URL(string: provider.base)?.host.map { "\($0):\(URL(string: provider.base)?.port ?? 0)" } ?? provider.base : "Not found at \(provider.base.replacingOccurrences(of: "http://", with: ""))")
        }
        if provider.plan?.custom == true, !provider.base.isEmpty { parts.append(provider.base) }
        if let v = provider.version { parts.append("Version \(v)") }
        if let count { parts.append("\(count) model\(count == 1 ? "" : "s")") }
        if !provider.formats.isEmpty {
            parts.append(provider.formats.map(formatName).joined(separator: ", "))
        }
        if chatgpt, let label = provider.account?.label { parts.append(label) }
        if let a = provider.account, let usage = a.usage {
            parts.append(a.limit.map { String(format: "$%.2f of $%.2f used", usage, $0) } ?? String(format: "$%.2f used", usage))
        }
        return parts.joined(separator: " · ")
    }
}

/// Coding plans: the connected ones, each with its key to change or forget, and the others to add.
private struct CodingPlans: View {
    @ObservedObject var store: ProvidersStore
    @Binding var editing: String?

    var body: some View {
        let plans = store.providers.filter { $0.plan != nil }
        if !plans.isEmpty {
            Section {
                ForEach(plans.filter(\.connected)) { p in
                    ProviderRow(provider: p, count: store.models[p.id]?.count, loading: store.loading.contains(p.id), error: store.errors[p.id] ?? p.error,
                                connecting: false, connect: {}, disconnect: { store.disconnect(p.id) }, changeKey: { editing = p.id })
                    if editing == p.id { PlanEditor(provider: p, store: store) { editing = nil } }
                }
                if let p = plans.first(where: { $0.id == editing && !$0.connected }) {
                    PlanEditor(provider: p, store: store) { editing = nil }
                }
                let unconnected = plans.filter { !$0.connected }
                if !unconnected.isEmpty && editing == nil {
                    Menu("Add a Coding Plan") {
                        ForEach(unconnected) { p in Button(p.name) { editing = p.id } }
                    }
                    .fixedSize()
                    .help("Add a coding plan's key so your agents can use the plan")
                }
            } header: {
                Text("Coding Plans")
            } footer: {
                Footnote("Add a plan's key, and any agent that speaks an API the plan supports can use it: Claude Code with Anthropic's API, Codex with the Responses API. Keys stay on this Mac and never sync. When you reach the plan's limit, the agent shows the plan's message and dino says so. dino never switches you to other billing. Claude subscriptions work only with Claude Code, so they can't be added here.")
            }
        }
    }
}

/// Pasting a coding plan's key (and the generic entry's base URL).
private struct PlanEditor: View {
    let provider: ProviderInfo
    @ObservedObject var store: ProvidersStore
    let close: () -> Void
    @State private var key = ""
    @State private var base = ""

    private var plan: PlanInfo? { provider.plan }
    private var saving: Bool { store.savingPlan == provider.id }
    private var ready: Bool {
        !key.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && (plan?.custom != true || !base.trimmingCharacters(in: .whitespaces).isEmpty)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(provider.connected ? "A new key for \(provider.name)" : provider.name).font(.headline)
            if let blurb = plan?.blurb { Text(blurb).font(.callout).foregroundStyle(.secondary) }
            if let terms = plan?.terms { Text(terms).font(.callout).foregroundStyle(.secondary) }
            HStack(spacing: 14) {
                if let page = plan?.keys_page.flatMap(URL.init(string:)) { Link("Get a key…", destination: page) }
                if let docs = plan.flatMap({ URL(string: $0.docs) }), !(plan?.docs.isEmpty ?? true) { Link("Documentation", destination: docs) }
            }
            .font(.callout)
            if plan?.custom == true {
                TextField("Base URL", text: $base, prompt: Text("https://api.example.com/anthropic, or http://127.0.0.1:11434"))
                    .textFieldStyle(.roundedBorder)
                    .onAppear { if base.isEmpty { base = provider.base } }
            }
            SecureField("API key", text: $key, prompt: Text("Paste the plan's API key"))
                .textFieldStyle(.roundedBorder)
                .onSubmit(connect)
            if let e = store.planErrors[provider.id] {
                Text(e).font(.callout).foregroundStyle(.orange).fixedSize(horizontal: false, vertical: true)
            }
            HStack {
                Spacer()
                if saving { ProgressView().controlSize(.small).help("Checking the key with \(provider.name)") }
                Button("Cancel") {
                    if store.planErrors[provider.id] != nil { store.planErrors[provider.id] = nil }
                    close()
                }
                Button(provider.connected ? "Save" : "Connect", action: connect)
                    .keyboardShortcut(.defaultAction)
                    .disabled(!ready || saving)
            }
        }
        .padding(.vertical, 4)
    }

    private func connect() {
        guard ready, !saving else { return }
        let base = plan?.custom == true ? self.base.trimmingCharacters(in: .whitespaces) : nil
        store.connectPlan(provider.id, key: key.trimmingCharacters(in: .whitespacesAndNewlines), base: base) {
            key = ""
            close()
        }
    }
}

func formatName(_ f: String) -> String {
    switch f {
    case "anthropic": "Anthropic"
    case "chat": "Chat Completions"
    case "responses": "Responses"
    default: f
    }
}

/// One model: its name and id, what it costs, the agent to run it in, and every agent's verdict.
/// Equatable: a redraw of the list skips rows whose model didn't change.
private struct ModelRowView: View, Equatable {
    let item: ModelItem
    /// The agent the list is filtered to, shown first.
    let highlight: String
    /// Agents that can start here.
    let startable: Set<String>

    private var model: ProviderModel { item.model }

    static func == (a: Self, b: Self) -> Bool {
        a.item == b.item && a.highlight == b.highlight && a.startable == b.startable
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .firstTextBaseline) {
                VStack(alignment: .leading, spacing: 1) {
                    Text(model.name).lineLimit(1)
                    Text(model.id).font(.caption.monospaced()).foregroundStyle(.secondary).lineLimit(1).textSelection(.enabled)
                }
                Spacer()
                Text(item.facts).font(.caption).foregroundStyle(.secondary).multilineTextAlignment(.trailing)
            }
            HStack(alignment: .center, spacing: 10) {
                if let r = model.recommended {
                    let others = model.agents.filter { !$0.recommended && $0.status != "no" }
                    HStack(spacing: 2) {
                        Button { run(r) } label: { Label("Run in \(r.name)", systemImage: "play.fill") }
                            .disabled(why(r) != nil)
                            .help(why(r) ?? "Start \(r.name) on \(model.name), through dino")
                        // The other agents' menu, made when it's opened: a Menu in every row cost
                        // the list a pop-up button per row.
                        if !others.isEmpty {
                            Button { otherAgents(others) } label: { Image(systemName: "chevron.down").font(.caption2.weight(.semibold)) }
                                .help("Run in another agent it works in")
                                .accessibilityLabel("Run in another agent")
                        }
                    }
                    .controlSize(.small)
                    .fixedSize()
                }
                // One line of verdicts, their reasons in one tooltip for the row.
                Text(verdicts)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.tail)
                    .help(reasons)
            }
        }
        .padding(.horizontal, 30)
        // One height for every row: the lazy list places rows without measuring them.
        .frame(height: Self.height)
        .contentShape(Rectangle())
    }

    static let height: CGFloat = 66

    /// The other agents it works in, as a menu at the mouse.
    private func otherAgents(_ others: [Verdict]) {
        let menu = NSMenu()
        for v in others {
            let item = NSMenuItem(title: "\(Verdict.mark(v.status)) \(v.name)\(v.reasons.first.map { " — \($0.text)" } ?? "")", action: nil, keyEquivalent: "")
            item.isEnabled = why(v) == nil
            item.target = MenuAction.shared
            item.action = #selector(MenuAction.perform(_:))
            item.representedObject = MenuAction.Box { run(v) }
            menu.addItem(item)
        }
        menu.autoenablesItems = false
        if let e = NSApp.currentEvent, let w = e.window, let v = w.contentView {
            menu.popUp(positioning: nil, at: v.convert(e.locationInWindow, from: nil), in: v)
        }
    }

    /// Why `v` can't be started from here, if it can't.
    private func why(_ v: Verdict) -> String? {
        if !startable.contains(v.agent) { return "\(v.name) isn't installed. Install it in Settings → Agents." }
        if v.translated { return "\(v.name) can't use this provider's API yet" }
        return nil
    }

    /// A new session of `v`'s agent on this model, shown in the main window. The app's model is
    /// asked only now: the rows don't watch it, or every session change would redraw them.
    private func run(_ v: Verdict) {
        guard let dino = (NSApp.delegate as? AppDelegate)?.model,
              let l = dino.launchers.first(where: { $0.agent_id == v.agent }) else { return }
        dino.newSessionHere(l, route: ProviderRoute(provider: model.provider, model: model.id))
        NSApp.windows.first { w in w.isVisible && !(w.identifier?.rawValue.hasPrefix(SettingsView.windowID) ?? false) && w.canBecomeMain }?
            .makeKeyAndOrderFront(nil)
    }

    /// The highlighted agent first, then the rest as dinod ranked them.
    private var ordered: [Verdict] {
        model.agents.filter { $0.agent == highlight } + model.agents.filter { $0.agent != highlight }
    }

    /// "✓ Claude Code   ✓ Codex   ~ OpenCode   ✗ Pi", the recommended one in bold.
    private var verdicts: AttributedString {
        var out = AttributedString()
        for (i, v) in ordered.enumerated() {
            if i > 0 { out += AttributedString("   ") }
            var mark = AttributedString(Verdict.mark(v.status) + " ")
            mark.foregroundColor = Verdict.color(v.status)
            mark.font = .caption.weight(.semibold)
            var name = AttributedString(v.name)
            if v.recommended { name.font = .caption.weight(.semibold) }
            out += mark + name
        }
        return out
    }

    private var reasons: String {
        ordered.map { v in
            var line = "\(Verdict.mark(v.status)) \(v.name)"
            if let r = v.reasons.first { line += ": \(r.text)\(r.source.map { " (\($0))" } ?? "")" }
            if let via = v.via { line += " · \(formatName(via)) API\(v.translated ? ", translated by dino" : "")" }
            return line
        }.joined(separator: "\n")
    }
}

extension Verdict {
    static func mark(_ status: String) -> String {
        switch status {
        case "works": "✓"
        case "caveat": "~"
        default: "✗"
        }
    }

    static func color(_ status: String) -> Color {
        switch status {
        case "works": .green
        case "caveat": .orange
        default: .secondary
        }
    }
}

/// The target of menu items made in code that run a closure.
@MainActor
final class MenuAction: NSObject {
    static let shared = MenuAction()

    final class Box {
        let run: () -> Void
        init(_ run: @escaping () -> Void) { self.run = run }
    }

    @objc func perform(_ item: NSMenuItem) {
        (item.representedObject as? Box)?.run()
    }
}
