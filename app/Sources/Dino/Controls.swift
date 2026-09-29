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

    /// The choices besides Default, as (value, label, explanation).
    func options(_ k: Knobs, seen: [String] = [], current: String? = nil) -> [(String, String, String?)] {
        switch self {
        case .mode:
            return k.modes.map { ($0, Mode.label($0), Mode.help($0)) }
        case .model:
            // The agent's aliases, then models its sessions have answered with, then one typed by hand.
            var names = k.models
            for m in seen + [current].compactMap({ $0 }) where !names.contains(where: { same(m, $0) }) {
                names.append(m)
            }
            return names.map { ($0, shortModel($0), nil) }
        case .effort:
            return k.efforts.map { ($0, $0.capitalized, nil) }
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

/// `answered` is the model `chosen` names: the alias "haiku" is claude-haiku-4-5-20251001.
func same(_ answered: String, _ chosen: String) -> Bool {
    answered == chosen || answered.lowercased().contains(chosen.lowercased())
}

/// Pickers for a Form: New Session and Settings → Agents.
struct ControlFields: View {
    let knobs: Knobs
    @Binding var controls: Controls
    /// What Default means here (Settings → Agents), when it says more than "the agent's own".
    var defaults = Controls()
    var seen: [String] = []
    @State private var typing = false
    @State private var typed = ""

    private static let other = "\u{1}other"

    private func picker(_ kind: ControlKind) -> some View {
        let options = kind.options(knobs, seen: seen, current: kind.value(controls))
        let fallback = kind.value(defaults).map { v in options.first { $0.0 == v }?.1 ?? shortModel(v) }
        return Picker(kind == .mode ? "Mode" : kind.title, selection: Binding(
            get: { typing && kind == .model ? Self.other : kind.value(controls) ?? "" },
            set: { v in
                if v == Self.other {
                    typed = controls.model ?? ""
                    typing = true
                } else {
                    typing = false
                    kind.set(&controls, v.isEmpty ? nil : v)
                }
            }
        )) {
            Text(fallback.map { "Default (\($0))" } ?? "Default").tag("")
            ForEach(options, id: \.0) { o in
                Text(o.1).tag(o.0).help(o.2 ?? o.0)
            }
            if kind == .model {
                Divider()
                Text("Other…").tag(Self.other)
            }
        }
    }

    var body: some View {
        if ControlKind.mode.offered(by: knobs) {
            picker(.mode)
        }
        if ControlKind.model.offered(by: knobs) {
            picker(.model)
            if typing {
                TextField("Model name", text: $typed, prompt: Text("e.g. claude-opus-5-5"))
                    .font(.body.monospaced())
                    .onSubmit {
                        let name = typed.trimmingCharacters(in: .whitespaces)
                        controls.model = name.isEmpty ? nil : name
                        typing = false
                    }
            }
        }
        if ControlKind.effort.offered(by: knobs) {
            picker(.effort)
        }
    }
}

extension SessionInfo {
    /// What it runs with, or will once its turn is over.
    var shownControls: Controls { pending ?? controls ?? Controls() }

    /// Tokens in the context window and the window's size; nil until dino knows both.
    var contextUse: (used: UInt64, limit: UInt64)? {
        guard let used = context_tokens, used > 0, let limit = context_limit, limit > 0 else { return nil }
        return (used, limit)
    }

    /// The model it last answered with, when that isn't the one chosen.
    var otherModel: String? {
        guard let last = last_model, let chosen = controls?.model, !same(last, chosen) else { return nil }
        return last
    }
}

extension DinoModel {
    func knobs(for s: SessionInfo) -> Knobs? {
        launchers.first { $0.agent_id == s.agent_id }?.knobs
    }

    /// Mid-turn, a change waits until the turn is over.
    func busy(_ s: SessionInfo) -> Bool {
        [.thinking, .working].contains(status(of: s))
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

/// The selected session's mode, model and effort, compact, each opening its picker.
struct SessionControlsBar: View {
    @EnvironmentObject var model: DinoModel
    let session: SessionInfo

    var body: some View {
        let knobs = model.knobs(for: session) ?? Knobs(modes: [], model: false, models: [], efforts: [], restart: false)
        let c = session.shownControls
        HStack(spacing: 2) {
            if ControlKind.mode.offered(by: knobs) {
                chip(.mode, knobs, icon: ControlKind.icon(mode: c.mode), text: Mode.label(c.mode),
                     tint: c.mode == "bypass" ? .red : nil)
            }
            if ControlKind.model.offered(by: knobs) {
                let text = c.model.map(shortModel) ?? session.last_model.map { "Default · \(shortModel($0))" } ?? "Default"
                chip(.model, knobs, icon: "cpu", text: text, tint: session.otherModel == nil ? nil : .orange)
            }
            if ControlKind.effort.offered(by: knobs) {
                chip(.effort, knobs, icon: "gauge.with.dots.needle.50percent", text: c.effort?.capitalized ?? "Default")
            }
            if session.pending != nil {
                Image(systemName: "clock")
                    .font(.caption)
                    .foregroundStyle(.orange)
                    .padding(.horizontal, 3)
                    .help("Applies after this turn: the agent restarts with it, keeping its conversation")
            }
            if let ctx = session.contextUse {
                ContextRing(used: ctx.used, limit: ctx.limit).padding(.horizontal, 5)
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
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .contentShape(Rectangle())
        }
        .buttonStyle(.borderless)
        .help(help(kind))
        .popover(isPresented: Binding(get: { model.controlPicker == kind }, set: { if !$0, model.controlPicker == kind { model.controlPicker = nil } }), arrowEdge: .bottom) {
            ControlPopover(kind: kind, session: session, knobs: knobs)
        }
    }

    private func help(_ kind: ControlKind) -> String {
        let key = "⇧⌘\(kind.shortcut.character.uppercased())"
        switch kind {
        case .model:
            if let other = session.otherModel, let chosen = session.controls?.model {
                return "You chose \(chosen); the agent answered with \(other) (\(key))"
            }
            return "Model: \(session.last_model.map { "answering with \($0)" } ?? "the agent's default") (\(key))"
        case .mode:
            return "\(Mode.label(session.shownControls.mode)): \(session.shownControls.mode.map(Mode.help) ?? "the agent's own setting") (\(key))"
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

    private var current: String? { kind.value(session.shownControls) }

    var body: some View {
        let options: [(String?, String, String?)] = [(nil, "Default", "The agent's own setting")]
            + kind.options(knobs, seen: model.seenModels(session.agent_id), current: current).map { ($0.0, $0.1, $0.2) }
        VStack(alignment: .leading, spacing: 2) {
            Text(kind.title).font(.headline).padding(.bottom, 6)
            ForEach(Array(options.enumerated()), id: \.offset) { i, o in
                row(i, o)
            }
            if kind == .model {
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
            Text(model.busy(session)
                ? "Applies after this turn. The agent restarts with it and keeps its conversation."
                : "The agent restarts with it and keeps its conversation.")
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            if kind == .model, let other = session.otherModel {
                Text("Its last reply came from \(other).")
                    .font(.caption)
                    .foregroundStyle(.orange)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(12)
        .frame(width: 290)
    }

    @ViewBuilder private func row(_ i: Int, _ o: (String?, String, String?)) -> some View {
        let button = Button { choose(o.0) } label: {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Image(systemName: "checkmark")
                    .font(.caption.weight(.semibold))
                    .opacity(o.0 == current ? 1 : 0)
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

    private func choose(_ value: String?) {
        var c = session.shownControls
        kind.set(&c, value)
        model.controlPicker = nil
        model.setControls(session.id, c)
    }
}

/// Session → Permission Mode…, Model…, Effort…: open the toolbar's pickers.
struct ControlMenuItems: View {
    @EnvironmentObject var model: DinoModel

    var body: some View {
        let knobs = model.selectedSession.flatMap { model.knobs(for: $0) }
        ForEach([ControlKind.mode, .model, .effort]) { kind in
            Button("\(kind.title)…") { model.controlPicker = kind }
                .keyboardShortcut(kind.shortcut, modifiers: [.command, .shift])
                .disabled(!(knobs.map(kind.offered) ?? false))
        }
    }
}

/// New Session with everything: agent, where, mode, model and effort.
struct NewSessionSheet: View {
    @EnvironmentObject var model: DinoModel
    @Environment(\.dismiss) private var dismiss
    @State private var agent = ""
    @State private var worktree = false
    @State private var controls = Controls()
    @State private var defaults: [String: Controls] = [:]

    private var launcher: LauncherInfo? { model.launchers.first { $0.short == agent } ?? model.launchers.first }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("New Session").font(.title3.weight(.semibold)).padding([.horizontal, .top], 20)
            Form {
                Section {
                    Picker("Agent", selection: Binding(get: { launcher?.short ?? "" }, set: { agent = $0 })) {
                        ForEach(model.launchers) { l in Text(l.label).tag(l.short) }
                    }
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
                        .help("Its own worktree and branch: its edits stay off your checkout until you apply them")
                }
                if let l = launcher, let k = l.knobs, k.any {
                    Section {
                        ControlFields(knobs: k, controls: $controls, defaults: defaults[l.agent_id] ?? Controls(), seen: model.seenModels(l.agent_id))
                    } footer: {
                        Text("Default follows Settings → Agents, then the agent's own settings. You can change these later from the toolbar.")
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
                Button("Start") {
                    if let l = launcher { model.newSession(l, worktree: worktree, controls: controls) }
                    dismiss()
                }
                .keyboardShortcut(.defaultAction)
                .disabled(launcher == nil)
            }
            .padding([.horizontal, .bottom], 20)
        }
        .frame(width: 460)
        .onChange(of: launcher?.agent_id) { controls = Controls() }
        .task {
            let s = await Task.detached { try? DinoConnection(path: DinoEnvironment.socketPath).settings() }.value
            defaults = s?.agents ?? [:]
        }
    }
}
