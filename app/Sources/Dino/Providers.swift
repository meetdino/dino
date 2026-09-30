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
}

/// Settings → Providers' view of dinod: asks now and then while the pane is open.
@MainActor
final class ProvidersStore: ObservableObject {
    @Published var providers: [ProviderInfo] = []
    @Published var models: [String: [ProviderModel]] = [:]
    @Published var loading: Set<String> = []
    @Published var errors: [String: String] = [:]
    @Published var error: String?

    func load() {
        run { c in
            let providers = try c.providers()
            // OpenRouter's list is public; a local server's only while it runs.
            let asked = providers.filter { $0.connected || $0.id == "openrouter" }.map(\.id)
            return (providers, try asked.map { ($0, try c.models($0)) })
        } done: { providers, lists in
            if self.providers != providers { self.providers = providers }
            let running = Set(providers.filter { $0.connected || $0.id == "openrouter" }.map(\.id))
            for (id, r) in lists {
                if self.models[id] != r.models { self.models[id] = r.models }
                self.errors[id] = r.error
                if r.loading { self.loading.insert(id) } else { self.loading.remove(id) }
            }
            for id in self.models.keys where !running.contains(id) { self.models[id] = nil }
        }
    }

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
                ForEach(store.providers) { p in ProviderRow(provider: p, count: store.models[p.id]?.count, loading: store.loading.contains(p.id), error: store.errors[p.id] ?? p.error) }
            } header: {
                Text("Providers")
            } footer: {
                Footnote("dino asks each provider which APIs it serves and what its models can do, and reads model servers on this Mac at their usual ports. Nothing here comes from a list dino keeps.")
            }
            Section {
                HStack(spacing: 8) {
                    TextField("Search models", text: $search)
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
                try? await Task.sleep(for: .seconds(store.loading.isEmpty ? 10 : 1))
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
        }
        .padding(.vertical, 2)
    }

    private var state: String {
        if provider.local { return provider.connected ? "Running" : "Not running" }
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
            HStack(spacing: 5) {
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
            if verdict.recommended {
                Text("· Recommended").fontWeight(.medium)
            }
        }
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
