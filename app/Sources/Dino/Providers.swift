import AppKit
import SwiftUI

/// Where models come from besides the agents' own accounts: OpenRouter and model servers on this
/// Mac, as dinod last looked (see `crates/dino-core/src/providers.rs`).
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
private struct ModelsResponse: Decodable {
    let models: [ProviderModel]
    let loading: Bool
    let error: String?
}

extension DinoConnection {
    func providers() throws -> [ProviderInfo] {
        try JSONDecoder().decode(ProvidersResponse.self, from: send(["type": "providers"])).providers
    }

    fileprivate func models(_ provider: String) throws -> ModelsResponse {
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

/// OpenRouter and the model servers on this Mac, and every model they serve with the agents it
/// works in, the one to run it in first.
struct ProvidersPane: View {
    @StateObject private var store = ProvidersStore()
    @State private var search = ""
    /// Only models this agent can run ("" for any).
    @State private var worksIn = ""
    @State private var freeOnly = false
    @State private var localOnly = false

    /// How many rows to draw before asking for a narrower search.
    private static let shown = 150

    private var rows: [ProviderModel] {
        let q = search.trimmingCharacters(in: .whitespaces).lowercased()
        let local = Set(store.providers.filter(\.local).map(\.id))
        return store.providers.flatMap { store.models[$0.id] ?? [] }.filter { m in
            (q.isEmpty || m.id.lowercased().contains(q) || m.name.lowercased().contains(q))
                && (!freeOnly || m.free)
                && (!localOnly || local.contains(m.provider))
                && (worksIn.isEmpty || m.agents.contains { $0.agent == worksIn && $0.status != "no" })
        }
    }

    /// Every agent verdicts mention, in the order dinod gives them for the first model.
    private var agents: [(id: String, name: String)] {
        var seen = Set<String>()
        return store.models.values.lazy.flatMap { $0 }.first.map { m in
            m.agents.sorted { $0.name < $1.name }.filter { seen.insert($0.agent).inserted }.map { ($0.agent, $0.name) }
        } ?? []
    }

    var body: some View {
        Form {
            Section {
                ForEach(store.providers) { p in
                    ProviderRow(provider: p, count: store.models[p.id]?.count, loading: store.loading.contains(p.id), error: store.errors[p.id] ?? p.error,
                                connecting: store.connecting.contains(p.id), connect: { store.connect(p.id) }, disconnect: { store.disconnect(p.id) })
                }
            } header: {
                Text("Providers")
            } footer: {
                Footnote("Connect opens OpenRouter's sign-in in your browser; the key it makes is yours (see openrouter.ai/keys), stays on this Mac, and dino never shows it. Sign in with ChatGPT lets agents in dino use your ChatGPT plan, up to the weekly cap you set for dino in ChatGPT; nothing is billed beyond it. dino reads model servers on this Mac at their usual ports, and asks every provider which APIs it serves and what its models can do: nothing here comes from a list dino keeps.")
            }
            Section {
                HStack(spacing: 8) {
                    TextField("Search models", text: $search, prompt: Text("Search models"))
                        .labelsHidden()
                        .textFieldStyle(.roundedBorder)
                    Picker("Works in", selection: $worksIn) {
                        Text("Any agent").tag("")
                        ForEach(agents, id: \.id) { a in Text(a.name).tag(a.id) }
                    }
                    .labelsHidden()
                    .fixedSize()
                    Toggle("Free", isOn: $freeOnly).toggleStyle(.button)
                    Toggle("Local", isOn: $localOnly).toggleStyle(.button)
                }
                let rows = rows
                if rows.isEmpty {
                    Text(store.models.isEmpty ? "Asking providers for their models…" : "No models match")
                        .foregroundStyle(.secondary)
                }
                ForEach(rows.prefix(Self.shown)) { m in ModelRowView(model: m, highlight: worksIn) }
                if rows.count > Self.shown {
                    Text("Showing \(Self.shown) of \(rows.count). Search or filter to narrow it down.")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
            } header: {
                Text("Models")
            } footer: {
                Footnote("Each model shows the agents it works in, the recommended one first. ✓ works, ~ works with a caveat (hover for why), ✗ doesn't. What providers don't say comes from dino's curated notes, with their sources.")
            }
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

private struct ProviderRow: View {
    let provider: ProviderInfo
    let count: Int?
    let loading: Bool
    let error: String?
    let connecting: Bool
    let connect: () -> Void
    let disconnect: () -> Void
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
            Text(chatgpt ? "dino forgets its sign-in. Agents stop using your ChatGPT plan through dino." : "dino forgets the key. It stays on your \(provider.name) account until you delete it there.")
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
            parts.append(provider.connected ? URL(string: provider.base)?.host.map { "\($0):\(URL(string: provider.base)?.port ?? 0)" } ?? provider.base : "Looked for at \(provider.base.replacingOccurrences(of: "http://", with: ""))")
        }
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

func formatName(_ f: String) -> String {
    switch f {
    case "anthropic": "Anthropic"
    case "chat": "Chat Completions"
    case "responses": "Responses"
    default: f
    }
}

private struct ModelRowView: View {
    let model: ProviderModel
    /// The agent the list is filtered to, shown first.
    let highlight: String

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack(alignment: .firstTextBaseline) {
                VStack(alignment: .leading, spacing: 1) {
                    Text(model.name).lineLimit(1)
                    Text(model.id).font(.caption.monospaced()).foregroundStyle(.secondary).lineLimit(1).textSelection(.enabled)
                }
                Spacer()
                Text(facts).font(.caption).foregroundStyle(.secondary).multilineTextAlignment(.trailing)
            }
            if let r = model.recommended {
                Label {
                    Text("Run in \(r.name)").fontWeight(.medium) + Text(r.reasons.first.map { " — \($0.text)" } ?? "").foregroundColor(.secondary)
                } icon: {
                    Image(systemName: "star.fill").foregroundStyle(.green)
                }
                .font(.callout)
                .lineLimit(2)
                .help(r.reasons.first?.source ?? "")
            }
            Wrap(spacing: 5) {
                ForEach(chips) { v in VerdictChip(verdict: v) }
            }
        }
        .padding(.vertical, 2)
    }

    /// The recommended agent first, then the rest as dinod ranked them; ones that can't, last.
    private var chips: [Verdict] {
        model.agents.filter { $0.agent == highlight } + model.agents.filter { $0.agent != highlight }
    }

    private var facts: String {
        var parts: [String] = []
        if let c = model.context { parts.append("\(tokens(c)) context") }
        if model.tools == false { parts.append("no tools") }
        if model.local {
            parts.append("Local")
        } else if model.free {
            parts.append("Free")
        } else if let i = model.price_in, let o = model.price_out {
            parts.append(String(format: "$%.2f / $%.2f per M", i, o))
        }
        return parts.joined(separator: " · ")
    }

    private func tokens(_ n: UInt64) -> String {
        n >= 1_000_000 && n % 1_000_000 == 0 ? "\(n / 1_000_000)M" : "\((n + 512) / 1024)k"
    }
}

/// One agent's verdict on a model: ✓, ~ or ✗ with its name, the reasons on hover.
private struct VerdictChip: View {
    let verdict: Verdict

    var body: some View {
        HStack(spacing: 3) {
            Text(mark).fontWeight(.semibold)
            Text(verdict.name)
        }
        .lineLimit(1)
        .fixedSize()
        .font(.caption)
        .padding(.horizontal, 7)
        .padding(.vertical, 2)
        .foregroundStyle(color)
        .background(color.opacity(verdict.recommended ? 0.2 : 0.1), in: Capsule())
        .overlay(Capsule().strokeBorder(color.opacity(verdict.recommended ? 0.6 : 0), lineWidth: 1))
        .help(help)
    }

    private var mark: String {
        switch verdict.status {
        case "works": "✓"
        case "caveat": "~"
        default: "✗"
        }
    }

    private var color: Color {
        switch verdict.status {
        case "works": .green
        case "caveat": .orange
        default: .secondary
        }
    }

    private var help: String {
        var lines = verdict.reasons.map { r in r.source.map { "\(r.text) (\($0))" } ?? r.text }
        if let via = verdict.via { lines.append("Talks to it in \(formatName(via))\(verdict.translated ? ", translated by dino" : "")") }
        return lines.isEmpty ? "\(verdict.name) works with it" : lines.joined(separator: "\n")
    }
}

/// Its children in rows, as many to a row as fit.
private struct Wrap: Layout {
    var spacing: CGFloat = 5

    /// Each child's size, measured once per change rather than on every layout pass: a list of
    /// models lays out many rows of these.
    func makeCache(subviews: Subviews) -> [CGSize] {
        subviews.map { $0.sizeThatFits(.unspecified) }
    }

    func updateCache(_ cache: inout [CGSize], subviews: Subviews) {
        cache = makeCache(subviews: subviews)
    }

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout [CGSize]) -> CGSize {
        let rows = rows(width: proposal.width ?? .infinity, cache)
        var height: CGFloat = 0
        var width: CGFloat = 0
        for (n, row) in rows.enumerated() {
            let tallest: CGFloat = row.map { $0.1.height }.max() ?? 0
            height += tallest + (n > 0 ? spacing : 0)
            var w: CGFloat = 0
            for (i, item) in row.enumerated() { w += item.1.width + (i > 0 ? spacing : 0) }
            width = max(width, w)
        }
        return CGSize(width: min(width, proposal.width ?? width), height: height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout [CGSize]) {
        var y = bounds.minY
        for row in rows(width: bounds.width, cache) {
            var x = bounds.minX
            let height = row.map { $0.1.height }.max() ?? 0
            for (i, size) in row {
                subviews[i].place(at: CGPoint(x: x, y: y), proposal: ProposedViewSize(size))
                x += size.width + spacing
            }
            y += height + spacing
        }
    }

    private func rows(width: CGFloat, _ sizes: [CGSize]) -> [[(Int, CGSize)]] {
        var rows: [[(Int, CGSize)]] = [[]]
        var x: CGFloat = 0
        for (i, size) in sizes.enumerated() {
            if x > 0, x + size.width > width {
                rows.append([])
                x = 0
            }
            rows[rows.count - 1].append((i, size))
            x += size.width + spacing
        }
        return rows
    }
}
