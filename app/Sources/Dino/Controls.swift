import SwiftUI

/// Permission mode, model and effort: chosen in New Session, defaulted in Settings → Agents,
/// changed on a running session from its toolbar. dinod turns them into each agent's own flags.

enum ControlKind: String, Identifiable {
    case mode, model, effort
    var id: String { rawValue }

    var title: String {
        switch self {
        case .mode: "Permission Mode"
        case .model: "Model"
        case .effort: "Effort"
        }
    }

    var shortcut: KeyEquivalent {
        switch self {
        case .mode: "m"
        case .model: "i"
        case .effort: "e"
        }
    }

    func offered(by k: Knobs) -> Bool {
        switch self {
        case .mode: !k.modes.isEmpty
        case .model: k.model
        case .effort: !k.efforts.isEmpty
        }
    }

    func value(_ c: Controls) -> String? {
        switch self {
        case .mode: c.mode
        case .model: c.model
        case .effort: c.effort
        }
    }

    func set(_ c: inout Controls, _ v: String?) {
        switch self {
        case .mode: c.mode = v
        case .model: c.model = v
        case .effort: c.effort = v
        }
    }

    /// The choices besides Default. `model` is the one chosen, for the efforts it takes.
    func options(_ k: Knobs, seen: [String] = [], current: String? = nil, model: String? = nil) -> [ControlOption] {
        switch self {
        case .mode:
            return k.modes.map { ControlOption(value: $0, label: k.modeLabel($0), help: Mode.help($0)) }
        case .model:
            // The agent's own list, then models its sessions have answered with, then one typed by hand.
            var out = k.models.map { ControlOption(value: $0.id, label: $0.label, help: $0.id, group: $0.group) }
            let known = { (m: String) in k.listed(m) != nil || out.contains { same(m, $0.value) } }
            for m in seen where !known(m) {
                out.append(ControlOption(value: m, label: shortModel(m), help: m))
            }
            // One typed by hand needs its own entry, or the picker shows nothing; an alias is its model's.
            if let current, k.listed(current) == nil, !out.contains(where: { $0.value == current }) {
                out.append(ControlOption(value: current, label: k.label(current), help: current))
            }
            return out
        case .effort:
            let recommended = k.listed(model ?? k.default_model)?.default_effort
            return k.efforts(for: model).map { ControlOption(value: $0, label: $0.capitalized, help: $0 == recommended ? "Recommended for this model" : nil) }
        }
    }

    static func icon(mode: String?) -> String {
        switch mode {
        case "ask": "hand.raised"
        case "edits": "pencil"
        case "plan": "list.bullet.clipboard"
        case "auto": "wand.and.stars"
        case "bypass": "exclamationmark.shield"
        default: "shield"
        }
    }
}

/// One choice in a control's picker.
struct ControlOption {
    var value: String
    var label: String
    var help: String?
    /// Listed after the rest, under this heading ("More models").
    var group: String?
}

extension Controls {
    /// `self` with `model` chosen, and its effort brought within what that model takes.
    func choosing(model: String?, in k: Knobs) -> Controls {
        var c = self
        c.model = model
        c.effort = k.clamp(effort, for: model)
        return c
    }
}

/// `answered` is the model `chosen` names: the alias "haiku" is claude-haiku-4-5-20251001.
func same(_ answered: String, _ chosen: String) -> Bool {
    // A variant (Claude's `opus[1m]`) answers as the model it's a variant of.
    let chosen = chosen.hasSuffix("]") ? String(chosen[..<(chosen.lastIndex(of: "[") ?? chosen.endIndex)]) : chosen
    return answered == chosen || answered.lowercased().contains(chosen.lowercased())
}

/// Pickers for a Form: New Session and Settings → Agents.
struct ControlFields: View {
    let knobs: Knobs
    @Binding var controls: Controls
    /// What Default means here (Settings → Agents), when it says more than "the agent's own".
    var defaults = Controls()
    var seen: [String] = []
    /// Settings → Agents: the key path under which the organization can lock each control.
    var lockPath: String?
    @State private var typing = false
    @State private var typed = ""
    @State private var choosingModel = false

    private static let other = "\u{1}other"

    /// The model efforts are for: the one chosen here, else the default.
    private var model: String? { controls.model ?? defaults.model }

    private func set(_ kind: ControlKind, _ v: String?) {
        if kind == .model {
            controls = controls.choosing(model: v, in: knobs)
        } else {
            kind.set(&controls, v)
        }
    }

    @ViewBuilder private func picker(_ kind: ControlKind) -> some View {
        let options = kind.options(knobs, seen: seen, current: kind.value(controls), model: model)
        let fallback = kind == .model
            ? (defaults.model ?? knobs.default_model).map(knobs.label)
            : kind.value(defaults).map { v in options.first { $0.value == v }?.label ?? v }
        if kind == .model, options.count > ControlKind.longList {
            longPicker(options, fallback: fallback)
        } else {
            menuPicker(kind, options, fallback: fallback)
        }
    }

    /// A model list too long for a menu: a button that opens it searched and grouped.
    private func longPicker(_ options: [ControlOption], fallback: String?) -> some View {
        let defaultLabel = fallback.map { "Default (\($0))" } ?? "Default"
        let current = controls.model.map(knobs.canonical)
        return LabeledContent(ControlKind.model.title) {
            Button { choosingModel = true } label: {
                HStack(spacing: 4) {
                    Text(current.map(knobs.label) ?? defaultLabel).lineLimit(1)
                    Image(systemName: "chevron.up.chevron.down").font(.caption2)
                }
            }
            .popover(isPresented: $choosingModel, arrowEdge: .trailing) {
                ModelSearchList(options: options, current: current, defaultLabel: defaultLabel) { v in
                    choosingModel = false
                    typing = false
                    set(.model, v)
                }
                .padding(12)
                .frame(width: 340)
            }
        }
    }

    private func menuPicker(_ kind: ControlKind, _ options: [ControlOption], fallback: String?) -> some View {
        Picker(kind == .mode ? "Mode" : kind.title, selection: Binding(
            get: {
                if typing && kind == .model { return Self.other }
                let v = kind.value(controls) ?? ""
                return kind == .model ? knobs.canonical(v) : v
            },
            set: { v in
                if v == Self.other {
                    typed = controls.model ?? ""
                    typing = true
                } else {
                    typing = false
                    set(kind, v.isEmpty ? nil : v)
                }
            }
        )) {
            Text(fallback.map { "Default (\($0))" } ?? "Default").tag("")
            ForEach(options.filter { $0.group == nil }, id: \.value) { o in
                Text(o.label).tag(o.value).help(o.help ?? o.value)
            }
            ForEach(Array(Set(options.compactMap(\.group))).sorted(), id: \.self) { g in
                Section(g) {
                    ForEach(options.filter { $0.group == g }, id: \.value) { o in
                        Text(o.label).tag(o.value).help(o.help ?? o.value)
                    }
                }
            }
            if kind == .model {
                Divider()
                Text("Other…").tag(Self.other)
            }
        }
    }

    @ViewBuilder private func field(_ kind: ControlKind) -> some View {
        if let lockPath {
            picker(kind).orgLocked("\(lockPath).\(kind.rawValue)")
        } else {
            picker(kind)
        }
    }

    var body: some View {
        if ControlKind.mode.offered(by: knobs) {
            field(.mode)
        }
        if ControlKind.model.offered(by: knobs) {
            field(.model)
            if typing {
                TextField("Model name", text: $typed, prompt: Text("e.g. claude-opus-5-5"))
                    .font(.body.monospaced())
                    .onSubmit {
                        let name = typed.trimmingCharacters(in: .whitespaces)
                        set(.model, name.isEmpty ? nil : name)
                        typing = false
                    }
            }
        }
        // A model without effort levels (Haiku) has no effort to pick.
        if ControlKind.effort.offered(by: knobs), !knobs.efforts(for: model).isEmpty {
            field(.effort)
        }
    }
}

extension SessionInfo {
    /// What the session runs with now: the mode and model the agent says it's on. Never what's
    /// only asked for (`pending`): that's shown as on its way.
    var shownControls: Controls {
        var c = controls ?? Controls()
        c.mode = agent_mode ?? c.mode
        c.model = agent_model ?? c.model
        return c
    }

    /// The model it's on now, to show beside it: what the agent says, unless dino answers it with
    /// another (a fallback route, the free tier's pick); else what its last model call asked for.
    var modelNow: String? {
        guard fallback == nil, tier == nil, let said = agent_model else { return last_model }
        return said
    }

    /// What's been chosen: what's on its way, else what it runs with. A choice builds on it.
    var wantedControls: Controls { pending ?? shownControls }

    /// A control chosen that isn't in effect yet, when it differs from what is.
    func pending(_ kind: ControlKind) -> String?? {
        guard let pending, kind.value(pending) != kind.value(shownControls) else { return nil }
        return .some(kind.value(pending))
    }

    /// Tokens in the context window and the window's size; nil until dino knows both.
    var contextUse: (used: UInt64, limit: UInt64)? {
        guard let used = context_tokens, used > 0, let limit = context_limit, limit > 0 else { return nil }
        return (used, limit)
    }

    /// The model it last answered with, when that isn't the one chosen. Not once the agent says
    /// which it's on: that's the one shown, and a reply from before a switch, or a side call's,
    /// is no news.
    var otherModel: String? {
        guard agent_model == nil, let last = last_model, let chosen = controls?.model, !same(last, chosen) else { return nil }
        return last
    }
}

extension DinoModel {
    func knobs(for s: SessionInfo) -> Knobs? {
        launchers.first { $0.agent_id == s.agent }?.knobs
    }

    /// Mid-turn, or with subagents or background commands running, a change waits: restarting
    /// the agent would end them.
    func busy(_ s: SessionInfo) -> Bool {
        [.thinking, .working].contains(status(of: s)) || (s.tasks?.running ?? 0) > 0
    }
}

/// How full a session's context window is.
struct ContextRing: View {
    let used: UInt64
    let limit: UInt64
    var size: CGFloat = 13

    private var fraction: Double { min(1, Double(used) / Double(limit)) }
    private var color: Color {
        switch fraction {
        case ..<0.6: .secondary
        case ..<0.85: .orange
        default: .red
        }
    }

    var body: some View {
        ZStack {
            Circle().stroke(.quaternary, lineWidth: 2.2)
            Circle()
                .trim(from: 0, to: fraction)
                .stroke(color, style: StrokeStyle(lineWidth: 2.2, lineCap: .round))
                .rotationEffect(.degrees(-90))
        }
        .frame(width: size, height: size)
        .help("Context: \(roundTokens(used)) of \(roundTokens(limit)) tokens (\(Int((fraction * 100).rounded()))%)")
        .accessibilityLabel("Context \(Int((fraction * 100).rounded())) percent full")
    }
}

/// 46k, 200k, 1.05M: for sizes, where tokens()'s decimal would be noise.
func roundTokens(_ n: UInt64) -> String {
    switch n {
    case ..<1000: "\(n)"
    case ..<1_000_000: "\(Int((Double(n) / 1e3).rounded()))k"
    default: (Double(n) / 1e6).formatted(.number.precision(.fractionLength(0...2))) + "M"
    }
}

/// What leads the toolbar for the selected session: what it's set to (SessionControlsBar), an
/// agent typed into a shell's as any other's, or Resume for one that exited. A plain shell has
/// none of these, and leaves the toolbar bare.
struct SessionToolbarItem: View {
    let session: SessionInfo

    var body: some View {
        HStack(spacing: 8) {
            if let host = session.host { HostChip(host: host) }
            if session.exited {
                ResumeButton(session: session)
            } else if session.agent != "shell" {
                SessionControlsBar(session: session)
            }
        }
        .fixedSize()
    }
}

/// The selected session's mode, model and effort as one segmented control, each segment opening
/// its picker, with what qualifies them beside it: a fallback answering, a change on its way, how
/// full its context is.
struct SessionControlsBar: View {
    @EnvironmentObject var model: DinoModel
    let session: SessionInfo

    var body: some View {
        let knobs = model.knobs(for: session) ?? .none
        HStack(spacing: 8) {
            ControlGroup { pickers(knobs) }
            if let f = session.fallback {
                FallbackChip(fallback: f, usage: session.usage_by_route ?? [])
            }
            if session.pending != nil {
                Image(systemName: "clock")
                    .font(.caption)
                    .foregroundStyle(.orange)
                    .padding(.horizontal, 3)
                    .help(Self.pendingHelp(knobs, only: session.pending(.model) == nil && session.pending(.effort) == nil))
            }
            if let ctx = session.contextUse {
                ContextRing(used: ctx.used, limit: ctx.limit).padding(.horizontal, 5)
            }
        }
    }

    /// Mode, model and effort, each opening its picker.
    @ViewBuilder private func pickers(_ knobs: Knobs) -> some View {
        let c = session.shownControls
        Group {
            if ControlKind.mode.offered(by: knobs) {
                // The mode it's in, and the one chosen on its way: "Auto → Bypass".
                let next = session.pending(.mode).map { " → " + knobs.modeLabel($0) } ?? ""
                chip(.mode, knobs, icon: ControlKind.icon(mode: c.mode), text: knobs.modeLabel(c.mode) + next,
                     tint: c.mode == "bypass" ? .red : nil)
            }
            if knobs.keeps("model"), session.route != nil || ControlKind.model.offered(by: knobs) {
                // It keeps its conversation's model: shown, not offered.
                let text = session.route?.label ?? c.model.map(knobs.label) ?? knobs.default_model.map(knobs.label) ?? "Default"
                HStack(spacing: 4) {
                    Image(systemName: session.route == nil ? "cpu" : "desktopcomputer").font(.caption)
                    Text(text).lineLimit(1)
                }
                .font(.callout)
                .foregroundStyle(.secondary)
                .padding(.horizontal, 2)
                .padding(.vertical, 2)
                .help(Self.keptModel(session))
            } else if let route = session.route {
                // On a provider's model: that provider's models, not the agent's own.
                Button { model.controlPicker = .model } label: {
                    HStack(spacing: 4) {
                        Image(systemName: route.provider == "openrouter" || route.provider == "chatgpt" ? "cloud" : "desktopcomputer").font(.caption)
                        Text(route.label).lineLimit(1)
                    }
                    .font(.callout)
                    .padding(.horizontal, 2)
                    .padding(.vertical, 2)
                    .contentShape(Rectangle())
                }
                .help("Runs on \(route.label) through dino, not \(AgentNames.of(session.agent))'s own account (⇧⌘M)")
                .popover(isPresented: Binding(get: { model.controlPicker == .model }, set: { if !$0, model.controlPicker == .model { model.controlPicker = nil } }), arrowEdge: .bottom) {
                    ProviderModelPopover(session: session, route: route)
                }
            } else if ControlKind.model.offered(by: knobs) {
                let text = c.model.map(knobs.label) ?? (session.last_model ?? knobs.default_model).map(knobs.label) ?? "Default"
                let next = session.pending(.model).map { " → " + ($0.map(knobs.label) ?? "Default") } ?? ""
                chip(.model, knobs, icon: "cpu", text: text + next, tint: session.otherModel == nil ? nil : .orange)
            }
            // A model without effort levels (Haiku) has none to show.
            if ControlKind.effort.offered(by: knobs), !knobs.efforts(for: c.model).isEmpty {
                let next = session.pending(.effort).map { " → " + ($0?.capitalized ?? "Default") } ?? ""
                chip(.effort, knobs, icon: "gauge.with.dots.needle.50percent", text: (c.effort?.capitalized ?? "Default") + next)
            }
        }
    }

    private func chip(_ kind: ControlKind, _ knobs: Knobs, icon: String, text: String, tint: Color? = nil) -> some View {
        Button { model.controlPicker = kind } label: {
            HStack(spacing: 4) {
                Image(systemName: icon).font(.caption)
                Text(text).lineLimit(1)
            }
            .font(.callout)
            .foregroundStyle(tint ?? .primary)
            .padding(.horizontal, 2)
            .padding(.vertical, 2)
            .contentShape(Rectangle())
        }
        .help(help(kind))
        .popover(isPresented: Binding(get: { model.controlPicker == kind }, set: { if !$0, model.controlPicker == kind { model.controlPicker = nil } }), arrowEdge: .bottom) {
            ControlPopover(kind: kind, session: session, knobs: knobs)
        }
    }

    /// When a choice not in effect yet applies; `mode` when only the mode is on its way.
    static func pendingHelp(_ knobs: Knobs, only mode: Bool) -> String {
        if mode, knobs.live_modes == true {
            return "Not in effect yet. The agent switches as soon as it safely can. If it can't switch in place, it restarts with the new mode once idle and keeps its conversation."
        }
        return "Not in effect yet. Once the agent, its subagents and its background commands are done, the agent restarts with the new setting and keeps its conversation."
    }

    /// Why a session's model can't be changed from here.
    static func keptModel(_ s: SessionInfo) -> String {
        "\(AgentNames.of(s.agent)) can't change a conversation's model. Start a new session to use another one."
    }

    private func help(_ kind: ControlKind) -> String {
        let key = "⇧⌘\(kind.shortcut.character.uppercased())"
        switch kind {
        case .model:
            if let other = session.otherModel, let chosen = session.controls?.model {
                return "You chose \(chosen), but the agent answered with \(other) (\(key))"
            }
            if let said = session.agent_model {
                return "Model: \(said), as \(AgentNames.of(session.agent)) says (\(key))"
            }
            return "Model: \(session.last_model.map { "answering with \($0)" } ?? "the agent's default") (\(key))"
        case .mode:
            let k = model.knobs(for: session) ?? .none
            let next = session.pending(.mode).map { "; switching to \(k.modeLabel($0))" } ?? ""
            return "\(k.modeLabel(session.shownControls.mode)): \(session.shownControls.mode.map(Mode.help) ?? "the agent's own setting")\(next) (\(key))"
        case .effort:
            return "How hard the model thinks (\(key))"
        }
    }
}

/// One control's choices, numbered for the keyboard: ⇧⌘M then 3 picks Plan.
struct ControlPopover: View {
    @EnvironmentObject var model: DinoModel
    let kind: ControlKind
    let session: SessionInfo
    let knobs: Knobs
    @State private var typed = ""
    @State private var showMore = false

    /// Default, and what it comes to when that's known: "Default (Manual)".
    private var defaultLabel: String {
        let resolved: String? = switch kind {
        // The agent reports its mode; it is its own default only while dino hasn't set one.
        case .mode: (session.controls?.mode == nil ? session.agent_mode : nil).map(knobs.modeLabel)
        case .model: (knobs.default_model ?? session.last_model).map(knobs.label)
        case .effort: knobs.listed(session.shownControls.model ?? knobs.default_model)?.default_effort?.capitalized
        }
        return resolved.map { "Default (\($0))" } ?? "Default"
    }

    /// What's chosen, an alias as the model it names so its row is checked.
    private var current: String? {
        let v = kind.value(session.shownControls)
        return kind == .model ? v.map(knobs.canonical) : v
    }

    var body: some View {
        let listed = kind.options(knobs, seen: model.seenModels(session.agent), current: current, model: session.shownControls.model)
            .filter { kind != .mode || !knobs.keeps($0.value) || $0.value == current }
        let dropped = kind == .mode ? knobs.modes.filter { knobs.keeps($0) && $0 != current } : []
        // Ungrouped first, so the numbers follow what's shown; older models fold away unless
        // one of them is chosen.
        let main = listed.filter { $0.group == nil }
        let more = listed.filter { $0.group != nil }
        let open = showMore || more.contains { $0.value == current }
        let options: [(String?, String, String?)] = [(nil, defaultLabel, defaultHelp)] + (main + (open ? more : [])).map { ($0.value, $0.label, $0.help) }
        let long = kind == .model && listed.count > ControlKind.longList
        VStack(alignment: .leading, spacing: 2) {
            Text(kind.title).font(.headline).padding(.bottom, 6)
            if long {
                // Hundreds of models (Pi across providers): searched, grouped, kept on screen.
                ModelSearchList(options: listed, current: current, defaultLabel: defaultLabel, defaultHelp: defaultHelp, choose: choose)
            } else {
            ForEach(Array(options.enumerated()), id: \.offset) { i, o in
                if i == main.count + 1, open, let g = more.first?.group {
                    Text(g).font(.caption).foregroundStyle(.secondary).padding(.top, 6)
                }
                row(i, o)
            }
            if !more.isEmpty, !open {
                Button { showMore = true } label: {
                    Label("\(more.first?.group ?? "More") (\(more.count))", systemImage: "chevron.right")
                        .labelStyle(.titleAndIcon)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .padding(.vertical, 3).padding(.horizontal, 4)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .padding(.top, 2)
            }
            }
            if kind == .model, !long {
                TextField("Other model", text: $typed, prompt: Text("Other model name"))
                    .textFieldStyle(.roundedBorder)
                    .font(.callout.monospaced())
                    .padding(.top, 6)
                    .onSubmit {
                        let name = typed.trimmingCharacters(in: .whitespaces)
                        if !name.isEmpty { choose(name) }
                    }
            }
            Divider().padding(.vertical, 6)
            if !dropped.isEmpty {
                Text("\(AgentNames.of(session.agent)) can't keep \(dropped.map(knobs.modeLabel).joined(separator: ", ")) when it resumes a conversation. Start a new session to use \(dropped.count == 1 ? "it" : "them").")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.bottom, 4)
            }
            Text(kind == .mode && knobs.live_modes == true
                ? "The agent switches in place, as if you pressed Shift+Tab in it. During a turn, it waits rather than pass through a less restrictive mode. A mode Shift+Tab can't reach restarts the agent once it's idle, keeping its conversation."
                : model.busy(session)
                ? "Applies once the agent, its subagents and its background commands are done. The agent restarts with the new setting and keeps its conversation."
                : "The agent restarts with the new setting and keeps its conversation.")
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            if let next = session.pending(kind) {
                Text("Not in effect yet: \(next.map { kind == .mode ? knobs.modeLabel($0) : kind == .model ? knobs.label($0) : $0.capitalized } ?? "Default").")
                    .font(.caption)
                    .foregroundStyle(.orange)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if kind == .model, let other = session.otherModel {
                Text("Its last reply came from \(other).")
                    .font(.caption)
                    .foregroundStyle(.orange)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(12)
        .frame(width: long ? 340 : 290)
    }

    @ViewBuilder private func row(_ i: Int, _ o: (String?, String, String?)) -> some View {
        let button = Button { choose(o.0) } label: {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                // In effect: a check; chosen and on its way: a clock.
                Image(systemName: session.pending(kind).map { $0 == o.0 } == true ? "clock" : "checkmark")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(session.pending(kind).map { $0 == o.0 } == true ? Color.orange : Color.primary)
                    .opacity(o.0 == current || session.pending(kind).map { $0 == o.0 } == true ? 1 : 0)
                VStack(alignment: .leading, spacing: 1) {
                    Text(o.1)
                    if let help = o.2 {
                        Text(help).font(.caption).foregroundStyle(.secondary)
                    }
                }
                Spacer()
                if i < 9 {
                    Text("\(i + 1)").font(.caption.monospacedDigit()).foregroundStyle(.tertiary)
                }
            }
            .padding(.vertical, 3)
            .padding(.horizontal, 4)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        if i < 9 {
            button.keyboardShortcut(KeyEquivalent(Character("\(i + 1)")), modifiers: [])
        } else {
            button
        }
    }

    /// What Default is here, when the agent's settings say.
    private var defaultHelp: String {
        if kind == .model, let d = knobs.default_model { return "The agent's own setting: \(knobs.label(d))" }
        return "The agent's own setting"
    }

    private func choose(_ value: String?) {
        var c = session.wantedControls
        if kind == .model {
            c = c.choosing(model: value, in: knobs)
        } else {
            kind.set(&c, value)
        }
        model.controlPicker = nil
        model.setControls(session.id, c)
    }
}

/// The models a session's provider serves that its agent can use, for a session on a provider's
/// model: picking one restarts the agent on it, keeping its conversation.
struct ProviderModelPopover: View {
    @EnvironmentObject var model: DinoModel
    let session: SessionInfo
    let route: ProviderRoute
    @State private var models: [ProviderModel]?
    @State private var search = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(route.name ?? route.provider).font(.headline)
            TextField("Search models", text: $search, prompt: Text("Search \(route.name ?? route.provider)"))
                .textFieldStyle(.roundedBorder)
            let shown = (models ?? []).filter { m in search.isEmpty || m.id.localizedCaseInsensitiveContains(search) }.prefix(80)
            ScrollView {
                VStack(alignment: .leading, spacing: 0) {
                    if models != nil {
                        ForEach(shown) { m in row(m) }
                        if shown.isEmpty { Text("No models match").foregroundStyle(.secondary).padding(4) }
                    } else {
                        ProgressView().controlSize(.small).padding(4)
                    }
                }
            }
            // A popover sizes a scroll view to nothing: as tall as its rows, up to a point.
            .frame(height: min(260, max(40, CGFloat(shown.count) * 44)))
            Divider()
            Text(model.busy(session)
                ? "Applies once the agent is idle. The agent restarts with the new model and keeps its conversation."
                : "The agent restarts with the new model and keeps its conversation.")
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(12)
        .frame(width: 320)
        .task {
            let provider = route.provider, agent = session.agent
            let list = await Task.detached { try? DinoConnection(path: DinoEnvironment.socketPath).models(provider).models }.value ?? []
            // What its agent can use, the best for it first.
            let usable = list.filter { m in m.agents.first { $0.agent == agent }.map { $0.status != "no" && !$0.translated } ?? true }
            models = usable.sorted { a, b in rank(a, agent) < rank(b, agent) }
        }
    }

    private func rank(_ m: ProviderModel, _ agent: String) -> Int {
        guard let v = m.agents.first(where: { $0.agent == agent }) else { return 3 }
        return v.recommended ? 0 : v.status == "works" ? 1 : 2
    }

    private func row(_ m: ProviderModel) -> some View {
        let v = m.agents.first { $0.agent == session.agent }
        return Button {
            var c = session.wantedControls
            c.model = m.id
            model.controlPicker = nil
            model.setControls(session.id, c)
        } label: {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Image(systemName: "checkmark").font(.caption.weight(.semibold)).opacity(m.id == route.model ? 1 : 0)
                VStack(alignment: .leading, spacing: 1) {
                    Text(m.name).lineLimit(1)
                    Text(v?.status == "caveat" ? v?.reasons.first?.text ?? m.id : m.id)
                        .font(.caption)
                        .foregroundStyle(v?.status == "caveat" ? .orange : .secondary)
                        .lineLimit(1)
                }
                Spacer()
                if v?.recommended == true {
                    Image(systemName: "star.fill").font(.caption).foregroundStyle(.green).help("Recommended for this agent")
                }
            }
            .padding(.vertical, 3)
            .padding(.horizontal, 4)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}

/// Session → Permission Mode…, Model…, Effort…: open the toolbar's pickers.
struct ControlMenuItems: View {
    @EnvironmentObject var model: DinoModel

    var body: some View {
        let session = model.selectedSession
        let knobs = session.flatMap { model.knobs(for: $0) }
        ForEach([ControlKind.mode, .model, .effort]) { kind in
            // A model without effort levels (Haiku) has none to pick.
            let none = kind == .effort && (knobs?.efforts(for: session?.shownControls.model).isEmpty ?? true)
            // On a provider's model, its provider's models.
            let offered = ((knobs.map(kind.offered) ?? false) || (kind == .model && session?.route != nil)) && !(kind == .model && (knobs?.keeps("model") ?? false))
            Button("\(kind.title)…") { model.controlPicker = kind }
                .keyboardShortcut(kind.shortcut, modifiers: [.command, .shift])
                .disabled(!offered || none)
        }
    }
}

/// New Session with everything: agent, where (this Mac or an SSH host), mode, model and effort.
struct NewSessionSheet: View {
    @EnvironmentObject var model: DinoModel
    @Environment(\.dismiss) private var dismiss
    @Environment(\.openWindow) private var openWindow
    @State private var agent = ""
    @State private var worktree = false
    @State private var controls = Controls()
    @State private var defaults: [String: Controls] = [:]
    /// Settings → Workspaces → SSH Hosts; an empty `host` is this Mac.
    @State private var hosts: [String: DinoSettings.SshHost] = [:]
    @State private var host = ""
    @State private var remoteFolder = ""
    /// Settings → Models & Providers' providers that can serve now; `provider` empty is the agent's own account.
    @State private var providers: [ProviderInfo] = []
    @State private var provider = ""
    @State private var providerModels: [ProviderModel] = []
    @State private var providerModel = ""
    @State private var choosingProviderModel = false
    /// Start the chosen agent even while it's at its limit.
    @State private var stay = false

    private static let addHost = "\u{0}add"

    /// The chosen provider model, with what each agent can make of it.
    private var chosen: ProviderModel? { providerModels.first { $0.id == providerModel } }
    /// What the chosen agent can make of the chosen model.
    private var verdict: Verdict? { launcher.flatMap { l in chosen?.agents.first { $0.agent == l.agent_id } } }
    /// Why it can't start on the chosen model, if it can't.
    private var routeProblem: String? {
        guard !provider.isEmpty else { return nil }
        guard chosen != nil else { return "Pick a model" }
        guard let v = verdict else { return "\(launcher?.label ?? "This agent") can't run on another provider's model" }
        if v.status == "no" { return v.reasons.first?.text ?? "\(v.name) can't use this model" }
        if v.translated { return "\(v.name) would need dino to translate its API for this provider, which starting a session doesn't do yet" }
        return nil
    }
    /// Claude Code on the free pool goes through dino on this Mac, so it doesn't run over SSH.
    private var launchers: [LauncherInfo] { host.isEmpty ? model.launchers : model.launchers.filter { !$0.agent_id.hasSuffix("-free") } }
    private var launcher: LauncherInfo? { launchers.first { $0.short == agent } ?? launchers.first }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("New Session").font(.title3.weight(.semibold)).padding([.horizontal, .top], 20)
            Form {
                // The agent is at its limit: what starts instead, and the choice to start it anyway.
                if host.isEmpty, provider.isEmpty, let l = launcher, let limit = model.limits.first(where: { $0.agent_id == l.agent_id }) {
                    LimitNotice(limit: limit, label: l.label, insteadLabel: limit.instead.flatMap { a in model.launchers.first { $0.agent_id == a }?.label }, stay: $stay)
                }
                Section {
                    Picker("Agent", selection: Binding(get: { launcher?.short ?? "" }, set: { agent = $0 })) {
                        ForEach(launchers) { l in Text(l.label).tag(l.short) }
                    }
                    Picker("Runs on", selection: Binding(get: { host }, set: pickHost)) {
                        Label("This Mac", systemImage: "laptopcomputer").tag("")
                        ForEach(hosts.keys.sorted(), id: \.self) { h in Label(h, systemImage: "server.rack").tag(h) }
                        Divider()
                        Text("Add SSH Host…").tag(Self.addHost)
                    }
                    if host.isEmpty {
                        LabeledContent("Folder") {
                            HStack {
                                Text((model.folder.path as NSString).abbreviatingWithTildeInPath)
                                    .lineLimit(1)
                                    .truncationMode(.head)
                                    .foregroundStyle(.secondary)
                                Button("Choose…") { model.chooseFolder() }
                            }
                        }
                        Toggle("In a new worktree", isOn: $worktree)
                            .help("Work in a separate worktree and branch. Changes stay out of your checkout until you apply them.")
                    } else {
                        LabeledContent("Folder") {
                            HStack(spacing: 4) {
                                TextField("Folder", text: $remoteFolder, prompt: Text(hostDefault))
                                    .textFieldStyle(.roundedBorder)
                                    .font(.body.monospaced())
                                    .labelsHidden()
                                let recent = model.recentFolders(on: host)
                                Menu {
                                    ForEach(recent, id: \.self) { f in Button(f) { remoteFolder = f } }
                                } label: {
                                    Image(systemName: "clock")
                                }
                                .menuStyle(.borderlessButton)
                                .fixedSize()
                                .disabled(recent.isEmpty)
                                .help(recent.isEmpty ? "No recent folders on \(host)" : "Recent folders on \(host)")
                            }
                        }
                    }
                } footer: {
                    if !host.isEmpty {
                        Text("A folder on \(host). ~ means your home folder there. The agent must be installed on \(host).")
                            .font(.callout)
                            .foregroundStyle(.secondary)
                    }
                }
                if host.isEmpty, !providers.isEmpty {
                    Section {
                        Picker("Model from", selection: $provider) {
                            Text("\(launcher?.label ?? "The agent")'s own account").tag("")
                            ForEach(providers) { p in Text(p.name).tag(p.id) }
                        }
                        if !provider.isEmpty, providerModels.count > ControlKind.longList {
                            // A provider's whole catalog (OpenRouter: hundreds): searched, not a menu.
                            LabeledContent("Model") {
                                Button { choosingProviderModel = true } label: {
                                    HStack(spacing: 4) {
                                        Text(chosen?.name ?? "Choose…").lineLimit(1)
                                        Image(systemName: "chevron.up.chevron.down").font(.caption2)
                                    }
                                }
                                .popover(isPresented: $choosingProviderModel, arrowEdge: .trailing) {
                                    ModelSearchList(
                                        options: providerModels.map { ControlOption(value: $0.id, label: $0.name, help: $0.id) },
                                        current: providerModel.isEmpty ? nil : providerModel,
                                        defaultLabel: "", allowsOther: false, showsDefault: false
                                    ) { v in
                                        choosingProviderModel = false
                                        if let v { providerModel = v }
                                    }
                                    .padding(12)
                                    .frame(width: 340)
                                }
                            }
                        } else if !provider.isEmpty {
                            Picker("Model", selection: $providerModel) {
                                if providerModels.isEmpty { Text("Asking \(providers.first { $0.id == provider }?.name ?? provider)…").tag("") }
                                ForEach(providerModels) { m in Text(m.name).tag(m.id) }
                            }
                            if let v = verdict {
                                let mark = v.status == "works" ? "✓" : v.status == "caveat" ? "~" : "✗"
                                Text("\(mark) \(v.name)\(v.recommended ? " is recommended for this model" : "")\(v.reasons.first.map { ": \($0.text)" } ?? "")")
                                    .font(.callout)
                                    .foregroundStyle(v.status == "works" ? .green : v.status == "caveat" ? .orange : .secondary)
                                    .fixedSize(horizontal: false, vertical: true)
                            }
                            if let r = chosen?.recommended, r.agent != launcher?.agent_id,
                               let l = model.launchers.first(where: { $0.agent_id == r.agent }) {
                                Button("Use \(r.name) Instead") { agent = l.short }
                                    .buttonStyle(.link)
                            }
                        }
                    } footer: {
                        Text(provider.isEmpty
                            ? "You can also run the agent on a model from a provider in Settings → Models & Providers."
                            : "The agent runs on this model through dino. Its own sign-in and settings don't change.")
                            .font(.callout)
                            .foregroundStyle(.secondary)
                    }
                }
                if let l = launcher, let k = l.knobs.map({ provider.isEmpty ? $0 : Knobs(modes: $0.modes, model: false, models: [], default_model: nil, efforts: [], restart: $0.restart) }), k.any {
                    Section {
                        ControlFields(knobs: k, controls: $controls, defaults: defaults[l.agent_id] ?? Controls(), seen: model.seenModels(l.agent_id))
                    } footer: {
                        Text("Default uses your choice in Settings → Agents, or else the agent's own setting. You can change these later from the toolbar.")
                            .font(.callout)
                            .foregroundStyle(.secondary)
                    }
                    .id(l.agent_id)
                }
            }
            .formStyle(.grouped)
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
                if let problem = routeProblem, chosen != nil {
                    Text(problem).font(.callout).foregroundStyle(.secondary).lineLimit(2)
                }
                Button("Start") {
                    if let l = launcher {
                        if host.isEmpty {
                            let route = provider.isEmpty ? nil : ProviderRoute(provider: provider, model: providerModel)
                            model.newSession(l, worktree: worktree, controls: controls, route: route, stay: stay)
                        } else {
                            model.newSession(l, controls: controls, host: host, remoteFolder: remoteFolder.trimmingCharacters(in: .whitespaces))
                        }
                    }
                    dismiss()
                }
                .keyboardShortcut(.defaultAction)
                .disabled(launcher == nil || (host.isEmpty && routeProblem != nil))
            }
            .padding([.horizontal, .bottom], 20)
        }
        .frame(width: 460)
        .onChange(of: launcher?.agent_id) {
            controls = Controls()
            stay = false
        }
        .onChange(of: provider) { loadModels() }
        // A model picked: its recommended agent, when it's here.
        .onChange(of: providerModel) {
            if let r = chosen?.recommended, let l = model.launchers.first(where: { $0.agent_id == r.agent }) { agent = l.short }
        }
        .task {
            let s = await Task.detached { try? DinoConnection(path: DinoEnvironment.socketPath).settings() }.value
            defaults = s?.agents ?? [:]
            hosts = s?.ssh ?? [:]
            let all = await Task.detached { (try? DinoConnection(path: DinoEnvironment.socketPath).providers()) ?? [] }.value
            providers = all.filter(\.connected)
        }
    }

    /// The chosen provider's models that some agent here can run, those with a recommendation first.
    private func loadModels() {
        providerModels = []
        providerModel = ""
        guard !provider.isEmpty else { return }
        let id = provider, here = Set(model.launchers.map(\.agent_id))
        Task {
            let list = await Task.detached { (try? DinoConnection(path: DinoEnvironment.socketPath).models(id).models) ?? [] }.value
            guard id == provider else { return }
            providerModels = list.filter { m in m.agents.contains { here.contains($0.agent) && $0.status != "no" } }
                .sorted { ($0.recommended == nil ? 1 : 0, $0.name) < ($1.recommended == nil ? 1 : 0, $1.name) }
            providerModel = providerModels.first?.id ?? ""
        }
    }

    private var hostDefault: String {
        let f = hosts[host]?.folder ?? ""
        return f.isEmpty ? "~" : f
    }

    /// "Add SSH Host…" opens Settings → Workspaces → SSH Hosts instead of choosing.
    private func pickHost(_ picked: String) {
        guard picked != Self.addHost else {
            SettingsPart.ssh.select()
            openWindow(id: SettingsView.windowID)
            dismiss()
            return
        }
        host = picked
        remoteFolder = ""
    }
}
