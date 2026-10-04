import SwiftUI

/// Any JSON value: settings this app shows but must hand back whole, with what a newer dinod
/// added to them (see `DinoSettings.fallbacks`).
enum JSONValue: Codable, Equatable {
    case null
    case bool(Bool)
    case number(Double)
    case string(String)
    case array([JSONValue])
    case object([String: JSONValue])

    init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() {
            self = .null
        } else if let b = try? c.decode(Bool.self) {
            self = .bool(b)
        } else if let n = try? c.decode(Double.self) {
            self = .number(n)
        } else if let s = try? c.decode(String.self) {
            self = .string(s)
        } else if let a = try? c.decode([JSONValue].self) {
            self = .array(a)
        } else {
            self = .object(try c.decode([String: JSONValue].self))
        }
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .null: try c.encodeNil()
        case .bool(let b): try c.encode(b)
        case .number(let n): try c.encode(n)
        case .string(let s): try c.encode(s)
        case .array(let a): try c.encode(a)
        case .object(let o): try c.encode(o)
        }
    }

    var string: String? { if case .string(let s) = self { s } else { nil } }
    var bool: Bool? { if case .bool(let b) = self { b } else { nil } }
    var array: [JSONValue]? { if case .array(let a) = self { a } else { nil } }
    var object: [String: JSONValue]? { if case .object(let o) = self { o } else { nil } }
}

/// One agent's "When it hits a limit" (see crates/dino-core/src/settings.rs `Fallback`), read and
/// changed in place: fields this app doesn't know stay as they came.
struct FallbackSetting: Equatable {
    var raw: [String: JSONValue]

    struct Step: Equatable {
        var raw: [String: JSONValue]
        var provider: String { raw["provider"]?.string ?? "" }
        var model: String { raw["model"]?.string ?? "" }
    }

    var steps: [Step] {
        get { raw["steps"]?.array?.compactMap { $0.object.map(Step.init) } ?? [] }
        set { raw["steps"] = .array(newValue.map { .object($0.raw) }) }
    }

    var onOutage: Bool {
        get { raw["on_outage"]?.bool ?? false }
        set { raw["on_outage"] = .bool(newValue) }
    }

    /// The agent new sessions start with while this one is at its limit; nil: this one anyway.
    var newSessions: String? {
        get { raw["new_sessions"]?.object?["agent"]?.string }
        set {
            if let newValue {
                var o = raw["new_sessions"]?.object ?? [:]
                if o["agent"]?.string != newValue { o["model"] = nil }
                o["agent"] = .string(newValue)
                raw["new_sessions"] = .object(o)
            } else {
                raw["new_sessions"] = nil
            }
        }
    }

    var newSessionsModel: String? { raw["new_sessions"]?.object?["model"]?.string }
    var isEmpty: Bool { steps.isEmpty && newSessions == nil && !onOutage }
}

extension DinoSettings {
    func fallback(_ agent: String) -> FallbackSetting {
        FallbackSetting(raw: fallbacks?[agent] ?? [:])
    }

    mutating func setFallback(_ agent: String, _ f: FallbackSetting) {
        var all = fallbacks ?? [:]
        all[agent] = f.isEmpty && f.raw.keys.allSatisfy({ ["steps", "on_outage", "new_sessions"].contains($0) }) ? nil : f.raw
        fallbacks = all
    }
}

/// Settings → Agents: where an agent's calls go when the route it uses hits its limit, in order,
/// and the agent new sessions start with meanwhile.
struct FallbackSection: View {
    @EnvironmentObject var store: SettingsStore
    let launcher: LauncherInfo
    /// Settings → Models & Providers' providers, as last asked.
    let providers: [ProviderInfo]

    @State private var newProvider = ""
    @State private var newModel = ""
    @State private var models: [ProviderModel] = []
    @State private var asking = false
    @State private var choosing = false

    private var agent: String { launcher.agent_id }
    private var setting: FallbackSetting { store.settings?.fallback(agent) ?? FallbackSetting(raw: [:]) }

    private func change(_ edit: (inout FallbackSetting) -> Void) {
        var f = setting
        edit(&f)
        store.update { $0.setFallback(agent, f) }
    }

    /// What can answer for it: connected, serving an API it speaks, and allowed by the policies.
    private var offered: [ProviderInfo] {
        let allowed = store.settings?.policies.fallback_providers ?? []
        let speaks = Set(launcher.formats ?? [])
        return providers.filter { p in
            p.connected && (allowed.isEmpty || allowed.contains(p.id)) && !speaks.isDisjoint(with: p.formats)
        }
    }

    /// dino's free models answer Anthropic Messages and Chat Completions, while they're on.
    private var freeOffered: Bool {
        let allowed = store.settings?.policies.fallback_providers ?? []
        let speaks = Set(launcher.formats ?? [])
        return store.settings?.experimental?["free_models"] == true && (allowed.isEmpty || allowed.contains("free"))
            && !speaks.isDisjoint(with: ["anthropic", "chat"])
    }

    private func name(_ provider: String) -> String {
        provider == "free" ? "Free models" : providers.first { $0.id == provider }?.name ?? provider
    }

    var body: some View {
        let locked = store.isLocked("fallbacks.\(agent)")
        Section {
            if locked {
                HStack(spacing: 6) {
                    OrgLock()
                    Text("Set by your organization").foregroundStyle(.secondary)
                }
            }
            let steps = setting.steps
            if steps.isEmpty {
                Text("Nothing: a session sees its route's limit, as it would without dino.")
                    .foregroundStyle(.secondary)
            }
            ForEach(Array(steps.enumerated()), id: \.offset) { i, step in
                HStack(spacing: 6) {
                    Text("\(i + 1).").monospacedDigit().foregroundStyle(.secondary)
                    Text(name(step.provider))
                    Text(step.model).font(.callout.monospaced()).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle)
                    if step.provider != "free", !providers.isEmpty, !offered.contains(where: { $0.id == step.provider }) {
                        Image(systemName: "exclamationmark.triangle").foregroundStyle(.orange)
                            .help("Not used now: \(name(step.provider)) isn't connected, isn't allowed, or doesn't serve an API \(launcher.label) speaks")
                    }
                    Spacer()
                    Button { change { f in f.steps.swapAt(i, i - 1) } } label: { Image(systemName: "chevron.up") }
                        .buttonStyle(.borderless).disabled(i == 0).help("Try it earlier").accessibilityLabel("Move up")
                    Button { change { f in f.steps.swapAt(i, i + 1) } } label: { Image(systemName: "chevron.down") }
                        .buttonStyle(.borderless).disabled(i == steps.count - 1).help("Try it later").accessibilityLabel("Move down")
                    Button { change { f in f.steps.remove(at: i) } } label: { Image(systemName: "minus.circle") }
                        .buttonStyle(.borderless).help("Remove").accessibilityLabel("Remove \(name(step.provider))")
                }
            }
            addRow
            Toggle("Also when a route is down", isOn: Binding(get: { setting.onOutage }, set: { on in change { $0.onOutage = on } }))
                .help("Server errors several times in a row, or unreachable: the next route answers for a few minutes")
            Picker("New sessions while it's at its limit", selection: Binding(get: { setting.newSessions ?? "" }, set: { v in change { $0.newSessions = v.isEmpty ? nil : v } })) {
                Text("Start \(launcher.label) anyway").tag("")
                ForEach(otherAgents) { l in Text("Start \(l.label)").tag(l.agent_id) }
            }
        } header: {
            Text("When \(launcher.label) Hits a Limit")
        } footer: {
            Footnote("When the route \(launcher.label) uses hits its limit (a plan's window, a subscription's limit, a balance), dino sends its calls to these routes in turn, each with its own key, and the session says so until the limit resets; then the route takes over again at the start of a turn. Short rate limits it waits out itself don't count. Only routes that serve the API it speaks are offered. A Claude subscription is only ever used by Claude Code with Anthropic.")
        }
        .disabled(locked)
    }

    /// Agents a new session can start with instead: one per agent, not this one.
    private var otherAgents: [LauncherInfo] {
        var seen: Set<String> = [agent, "shell"]
        return store.agents.filter { !$0.agent_id.hasSuffix("-free") && seen.insert($0.agent_id).inserted }
    }

    @ViewBuilder private var addRow: some View {
        HStack(spacing: 8) {
            Picker("Add a route", selection: $newProvider) {
                Text("Choose…").tag("")
                ForEach(offered) { p in Text(p.name).tag(p.id) }
                if freeOffered { Text("Free models (dino picks)").tag("free") }
            }
            .onChange(of: newProvider) { loadModels() }
            if newProvider == "free" {
                Text("auto").font(.callout.monospaced()).foregroundStyle(.secondary)
            } else if !newProvider.isEmpty {
                if models.count > ControlKind.longList {
                    Button { choosing = true } label: {
                        HStack(spacing: 4) {
                            Text(models.first { $0.id == newModel }?.name ?? "Model…").lineLimit(1)
                            Image(systemName: "chevron.up.chevron.down").font(.caption2)
                        }
                    }
                    .popover(isPresented: $choosing, arrowEdge: .trailing) {
                        ModelSearchList(options: models.map { ControlOption(value: $0.id, label: $0.name, help: $0.id) },
                                        current: newModel.isEmpty ? nil : newModel, defaultLabel: "", allowsOther: false, showsDefault: false) { v in
                            choosing = false
                            if let v { newModel = v }
                        }
                        .padding(12)
                        .frame(width: 340)
                    }
                } else {
                    Picker("Model", selection: $newModel) {
                        Text(asking ? "Asking…" : models.isEmpty ? "No model it can use" : "Model…").tag("")
                        ForEach(models) { m in Text(m.name).tag(m.id) }
                    }
                    .labelsHidden()
                }
            }
            Button("Add") {
                let model = newProvider == "free" ? "auto" : newModel
                change { $0.steps.append(.init(raw: ["provider": .string(newProvider), "model": .string(model)])) }
                newProvider = ""
                newModel = ""
            }
            .disabled(newProvider.isEmpty || (newProvider != "free" && newModel.isEmpty))
        }
    }

    /// The provider's models this agent runs without dino translating: the API it speaks.
    private func loadModels() {
        models = []
        newModel = ""
        guard !newProvider.isEmpty, newProvider != "free" else { return }
        let id = newProvider, agent = agent
        asking = true
        Task {
            let list = await Task.detached { (try? DinoConnection(path: DinoEnvironment.socketPath).models(id).models) ?? [] }.value
            guard id == newProvider else { return }
            asking = false
            models = list.filter { m in m.agents.contains { $0.agent == agent && $0.status != "no" && !$0.translated } }
        }
    }
}

/// The toolbar's word on a session answered by a fallback: from where, and until when.
struct FallbackChip: View {
    let fallback: FallbackInfo
    var usage: [RouteUsage] = []

    var body: some View {
        HStack(spacing: 4) {
            Image(systemName: "arrow.triangle.branch").font(.caption)
            Text("On fallback: \(fallback.name) · \(fallback.why)").lineLimit(1)
        }
        .font(.callout)
        .foregroundStyle(.orange)
        .padding(.horizontal, 6)
        .padding(.vertical, 2)
        .background(Color.orange.opacity(0.12), in: Capsule())
        .help(FallbackChip.detail(fallback, usage))
        .accessibilityElement(children: .combine)
    }

    static func detail(_ f: FallbackInfo, _ usage: [RouteUsage]) -> String {
        var lines = ["\(f.from) said: \(f.said)", "Answering: \(f.name) · \(f.model), since \(Clock.short(f.since))"]
        if let r = f.retry_at {
            lines.append("Back on \(f.from) at the start of a turn after \(Clock.short(r))")
        }
        if !usage.isEmpty {
            lines.append(usage.map { "\($0.name): ↑\(roundTokens($0.input_tokens)) ↓\(roundTokens($0.output_tokens))" }.joined(separator: " · "))
        }
        lines.append("Settings → Agents says where each agent goes")
        return lines.joined(separator: "\n")
    }
}

/// The sidebar row's line for a session on a fallback.
struct FallbackLine: View {
    let fallback: FallbackInfo

    var body: some View {
        Label("On fallback: \(fallback.name) · \(fallback.why)", systemImage: "arrow.triangle.branch")
            .font(.caption)
            .foregroundStyle(.orange)
            .lineLimit(1)
            .truncationMode(.tail)
    }
}

/// New Session's word on an agent at its limit, and the choice to start it anyway.
struct LimitNotice: View {
    let limit: AgentLimit
    let label: String
    let insteadLabel: String?
    @Binding var stay: Bool

    var body: some View {
        Section {
            if let other = insteadLabel {
                Label(stay ? "\(limit.sentence)." : "\(limit.sentence) — starting with \(other)", systemImage: "arrow.triangle.branch")
                    .foregroundStyle(.orange)
                Toggle("Start \(label) anyway", isOn: $stay)
            } else {
                Label("\(limit.sentence).", systemImage: "exclamationmark.triangle").foregroundStyle(.orange)
            }
        } footer: {
            Text(insteadLabel == nil
                ? "Its fallbacks in Settings → Agents answer for it, if it has any."
                : "Settings → Agents says what new sessions start with while it's at its limit.")
                .font(.callout)
                .foregroundStyle(.secondary)
        }
    }
}
