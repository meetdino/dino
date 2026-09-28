import SwiftUI

// MARK: - Fan-out: one prompt, several agents, each in its own worktree; keep the best.

struct FanoutSheet: View {
    @EnvironmentObject var model: DinoModel
    @Environment(\.dismiss) private var dismiss
    @State private var prompt = ""
    @State private var picked: Set<String> = []
    @State private var starting = false
    @State private var error: String?
    @FocusState private var focused: Bool

    private var agents: [LauncherInfo] { model.launchers.filter { $0.agent_id != "shell" } }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Label("Fan out", systemImage: "arrow.triangle.branch").font(.title2.weight(.semibold))
            Text("One prompt, several agents. Each works in its own git worktree, starting from your current changes. Compare the results, then keep the best.")
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            TextEditor(text: $prompt)
                .font(.body)
                .focused($focused)
                .scrollContentBackground(.hidden)
                .padding(8)
                .frame(height: 130)
                .background(RoundedRectangle(cornerRadius: 8).fill(Color(nsColor: .textBackgroundColor)))
                .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(.quaternary))
                .overlay(alignment: .topLeading) {
                    if prompt.isEmpty {
                        Text("What should they do?").foregroundStyle(.tertiary).padding(13).allowsHitTesting(false)
                    }
                }
            HStack(spacing: 14) {
                ForEach(agents) { l in
                    Toggle(l.label, isOn: Binding(
                        get: { picked.contains(l.short) },
                        set: { if $0 { picked.insert(l.short) } else { picked.remove(l.short) } }
                    ))
                }
            }
            HStack(spacing: 6) {
                Image(systemName: "folder").foregroundStyle(.secondary)
                Text(shortPath(model.folder.path))
                Button("Change…") { model.chooseFolder() }.buttonStyle(.link)
            }
            .font(.callout)
            if let error {
                Text(error).foregroundStyle(SessionStatus.exited.color).font(.callout)
            }
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
                Button {
                    start()
                } label: {
                    if starting { ProgressView().controlSize(.small).frame(width: 90) } else { Text("Fan Out").frame(width: 90) }
                }
                .keyboardShortcut(.return, modifiers: .command)
                .buttonStyle(.borderedProminent)
                .tint(Brand.green)
                .disabled(prompt.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || picked.isEmpty || starting)
            }
        }
        .padding(22)
        .frame(width: 560)
        .onAppear {
            picked = Set(agents.map(\.short))
            focused = true
        }
    }

    private func start() {
        starting = true
        error = nil
        let order = agents.map(\.short).filter { picked.contains($0) }
        Task {
            do {
                try await model.fanout(prompt: prompt, launchers: order)
            } catch {
                self.error = error.localizedDescription
            }
            starting = false
        }
    }
}

struct GroupRow: View {
    let group: GroupInfo

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(group.prompt).lineLimit(2)
            Text("Compare \(group.members.count) agents").font(.caption).foregroundStyle(Brand.green)
        }
        .padding(.vertical, 2)
    }
}

/// An agent's last upstream failure, so a dead agent never looks merely idle.
struct ErrorLine: View {
    let message: String

    var body: some View {
        // Not a Label: sidebar rows tint Label icons with the accent color.
        HStack(alignment: .firstTextBaseline, spacing: 5) {
            Image(systemName: "xmark.octagon.fill")
            Text(message).lineLimit(2)
        }
        .font(.caption).foregroundStyle(SessionStatus.exited.color)
        .help(message)
    }
}

struct StatText: View {
    let stat: DiffStat

    var body: some View {
        HStack(spacing: 5) {
            Text("+\(stat.added)").foregroundStyle(Brand.green)
            Text("−\(stat.removed)").foregroundStyle(SessionStatus.exited.color)
            Text("· \(stat.files) file\(stat.files == 1 ? "" : "s")").foregroundStyle(.secondary)
        }
    }
}

struct CompareView: View {
    @EnvironmentObject var model: DinoModel
    let group: GroupInfo
    @State private var shown: String?
    @State private var diff = ""

    var body: some View {
        let current = shown.flatMap { id in group.members.first { $0.session == id } } ?? group.members.first
        VStack(alignment: .leading, spacing: 14) {
            HStack(alignment: .top) {
                VStack(alignment: .leading, spacing: 4) {
                    Text(group.prompt).font(.title3.weight(.semibold)).lineLimit(3)
                    Text("\(shortPath(group.repo)) · each agent in its own worktree").font(.caption).foregroundStyle(.secondary)
                }
                Spacer()
                Button("Discard All", role: .destructive) { model.discard(group) }
                    .help("Stop these agents and remove their worktrees and branches")
            }
            HStack(alignment: .top, spacing: 12) {
                ForEach(group.members) { m in
                    MemberCard(member: m, selected: m.session == current?.session)
                        .onTapGesture { shown = m.session }
                }
            }
            Divider()
            if let current {
                DiffView(text: diff, empty: "\(current.launcher) hasn't changed anything yet")
                    // Reload when another member is shown or this one's changes grow.
                    .task(id: "\(current.session) \(current.stat.map { "\($0.files) \($0.added) \($0.removed)" } ?? "")") {
                        diff = await model.diff(current.session)
                    }
            }
        }
        .padding(20)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(Color(nsColor: .windowBackgroundColor))
    }
}

struct MemberCard: View {
    @EnvironmentObject var model: DinoModel
    let member: MemberInfo
    let selected: Bool

    var body: some View {
        let session = model.sessions.first { $0.id == member.session }
        let status = session.map { model.status(of: $0) } ?? .exited
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 6) {
                StatusDot(status: status)
                Text(member.launcher).font(.system(.body, design: .monospaced).weight(.semibold))
                Spacer()
                Text(status.label).font(.caption).foregroundStyle(status.color)
            }
            Group {
                if let stat = member.stat, stat.files > 0 {
                    StatText(stat: stat)
                } else {
                    Text(member.stat == nil ? "worktree missing" : "no changes yet").foregroundStyle(.tertiary)
                }
            }
            .font(.callout.monospacedDigit())
            if let needs = session?.needs {
                Label(needs, systemImage: "exclamationmark.triangle.fill")
                    .font(.caption).foregroundStyle(SessionStatus.needsYou.color).lineLimit(1)
            }
            if let error = session?.error {
                ErrorLine(message: error)
            }
            HStack {
                Button("Open") { model.select(member.session) }
                    .help("Talk to \(member.launcher) in its terminal")
                Spacer()
                Button("Keep") { model.confirmKeep = member }
                    .buttonStyle(.borderedProminent)
                    .tint(Brand.green)
                    .disabled((member.stat?.files ?? 0) == 0)
            }
            .controlSize(.small)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 10).fill(Color(nsColor: .controlBackgroundColor)))
        .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(selected ? Brand.green : Color.secondary.opacity(0.2), lineWidth: selected ? 2 : 1))
        .contentShape(Rectangle())
    }
}

/// A unified diff, colored; binary patches collapse to one line.
struct DiffView: View {
    let text: String
    let empty: String

    private var lines: [(Int, String)] {
        var out: [String] = []
        var inBinary = false
        for line in text.split(separator: "\n", omittingEmptySubsequences: false) {
            if line.hasPrefix("GIT binary patch") {
                inBinary = true
                out.append("  (binary file)")
            } else if inBinary {
                if line.hasPrefix("diff --git") { inBinary = false; out.append(String(line)) }
            } else {
                out.append(String(line))
            }
        }
        return Array(out.prefix(5000).enumerated())
    }

    var body: some View {
        if text.isEmpty {
            Text(empty).foregroundStyle(.tertiary).frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            ScrollView([.vertical, .horizontal]) {
                LazyVStack(alignment: .leading, spacing: 0) {
                    ForEach(lines, id: \.0) { _, line in
                        Text(line.isEmpty ? " " : line)
                            .font(.system(size: 12, design: .monospaced))
                            .foregroundStyle(color(line))
                            .fontWeight(line.hasPrefix("diff --git") ? .semibold : .regular)
                            .padding(.top, line.hasPrefix("diff --git") ? 10 : 0)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .background(background(line))
                    }
                }
                .padding(8)
                .textSelection(.enabled)
            }
            .background(RoundedRectangle(cornerRadius: 8).fill(Color(nsColor: .textBackgroundColor)))
        }
    }

    private func color(_ l: String) -> Color {
        if l.hasPrefix("+++") || l.hasPrefix("---") || l.hasPrefix("index ") || l.hasPrefix("new file") { return .secondary }
        if l.hasPrefix("+") { return Brand.green }
        if l.hasPrefix("-") { return SessionStatus.exited.color }
        if l.hasPrefix("@@") { return SessionStatus.done.color }
        return .primary
    }

    private func background(_ l: String) -> Color {
        if l.hasPrefix("+"), !l.hasPrefix("+++") { return Brand.green.opacity(0.08) }
        if l.hasPrefix("-"), !l.hasPrefix("---") { return SessionStatus.exited.color.opacity(0.08) }
        return .clear
    }
}
