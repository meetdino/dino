import SwiftUI

/// One entry of an agent conversation: `role` is "task" (what a subagent was asked), "user",
/// "assistant", "tool" (a call, in brief) or "note" (something that happened, like an interruption).
struct ConversationTurn: Codable, Equatable, Identifiable {
    var role: String
    var text: String
    var id = UUID()

    private enum CodingKeys: String, CodingKey { case role, text }

    /// The same words, whichever read they came from.
    static func == (a: Self, b: Self) -> Bool { a.role == b.role && a.text == b.text }
}

extension ConversationPage {
    /// The part of conversation `id` of `agent` (a session's, or a Claude subagent's) ending at
    /// byte `before`, off the main thread.
    static func fetch(agent: String, id: String, before: UInt64? = nil) async -> Result<ConversationPage, Error> {
        await Task.detached {
            Result { try DinoConnection(path: DinoEnvironment.socketPath).conversation(agent: agent, sessionID: id, before: before) }
        }.value
    }
}

/// A read-only agent conversation, newest at the bottom, from the newest part of its transcript
/// (`page`, which the owner keeps fresh). `earlier` (when set) loads the part before what's shown;
/// once the user has, what's shown stays put while newer turns wait behind a button.
struct ConversationView: View {
    /// Nil while loading, or when it can't be read (then `unreadable` says why).
    let page: ConversationPage?
    var agent = "claude"
    /// What a subagent was asked, shown first when the part shown starts after it.
    var task: String?
    var loading = false
    var unreadable = "This conversation can’t be read."
    var earlier: ((UInt64) async -> ConversationPage?)?

    /// Earlier parts the user loaded, joined to the newest part as it was then.
    @State private var loaded: ConversationPage?
    @State private var loadingEarlier = false

    private var shown: ConversationPage? { loaded ?? page }
    /// Turns arrived since the user loaded earlier ones.
    private var newer: Bool { loaded != nil && page?.turns.last != frozenLast }
    @State private var frozenLast: ConversationTurn?

    var body: some View {
        if let shown, !shown.turns.isEmpty || shown.start > 0 || task != nil {
            turnsView(shown)
        } else {
            VStack(spacing: 8) {
                if loading {
                    ProgressView().controlSize(.small)
                } else {
                    Text(page == nil ? unreadable : "No messages yet.")
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                }
            }
            .padding(40)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    private func turnsView(_ shown: ConversationPage) -> some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 12) {
                    if let task, !shown.turns.contains(where: { $0.role == "task" }) {
                        TaskBox(text: task)
                    }
                    if earlier != nil && shown.start > 0 {
                        HStack {
                            Spacer()
                            if loadingEarlier {
                                ProgressView().controlSize(.small)
                            } else {
                                Button("Load Earlier") { loadEarlier() }.buttonStyle(.link).font(.caption)
                            }
                            Spacer()
                        }
                    }
                    ForEach(blocks(shown.turns)) { block in
                        switch block {
                        case .turn(let t): TurnBubble(turn: t, agent: agent)
                        case .tools(_, let calls): ToolRun(calls: calls)
                        }
                    }
                    Color.clear.frame(height: 1).id("end")
                }
                .padding(14)
            }
            // Links in what the agent wrote: web pages open, files show in the Finder, other
            // apps' links only after asking.
            .environment(\.openURL, OpenURLAction { url in
                LinkPolicy.open(LinkPolicy.link(url), cwd: nil)
                return .handled
            })
            .overlay(alignment: .bottom) {
                if newer {
                    Button { loaded = nil } label: { Label("Newer turns", systemImage: "arrow.down") }
                        .buttonStyle(.borderedProminent)
                        .controlSize(.small)
                        .padding(10)
                }
            }
            .onAppear { proxy.scrollTo("end", anchor: .bottom) }
            .onChange(of: shown.turns.last?.id) { _, _ in
                // New turns at the end follow; earlier ones loaded at the top don't move the view.
                if !loadingEarlier { proxy.scrollTo("end", anchor: .bottom) }
            }
        }
    }

    private func loadEarlier() {
        guard let earlier, let base = shown, !loadingEarlier else { return }
        loadingEarlier = true
        Task {
            if let p = await earlier(base.start) {
                if loaded == nil { frozenLast = base.turns.last }
                loaded = ConversationPage(turns: p.turns + base.turns, start: p.start, path: base.path)
            }
            loadingEarlier = false
        }
    }

    /// Runs of tool calls fold into one row; the text around them is the conversation.
    private func blocks(_ turns: [ConversationTurn]) -> [Block] {
        var out: [Block] = []
        for t in turns {
            if t.role == "tool", case .tools(let id, let calls) = out.last {
                out[out.count - 1] = .tools(id, calls + [t.text])
            } else if t.role == "tool" {
                out.append(.tools(t.id, [t.text]))
            } else {
                out.append(.turn(t))
            }
        }
        return out
    }

    enum Block: Identifiable {
        case turn(ConversationTurn)
        case tools(UUID, [String])

        var id: UUID {
            switch self {
            case .turn(let t): t.id
            case .tools(let id, _): id
            }
        }
    }
}

/// Very long messages are cut; the transcript file has the rest.
private func markdown(_ text: String) -> AttributedString {
    let limit = 4000
    let s = text.count > limit ? String(text.prefix(limit)) + "…" : text
    let md = try? AttributedString(markdown: s, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace))
    return md ?? AttributedString(s)
}

private struct TaskBox: View {
    let text: String

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("Task").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
            Text(markdown(text)).textSelection(.enabled)
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Brand.green.opacity(0.08), in: RoundedRectangle(cornerRadius: 8))
    }
}

private struct TurnBubble: View {
    let turn: ConversationTurn
    let agent: String

    var body: some View {
        switch turn.role {
        case "note":
            Text(markdown(turn.text)).font(.caption.italic()).foregroundStyle(.tertiary).frame(maxWidth: .infinity)
        case "task":
            TaskBox(text: turn.text)
        case "user":
            HStack {
                Spacer(minLength: 60)
                Text(markdown(turn.text))
                    .textSelection(.enabled)
                    .padding(.horizontal, 10).padding(.vertical, 7)
                    .background(RoundedRectangle(cornerRadius: 10).fill(Color.accentColor.opacity(0.14)))
            }
        default:
            VStack(alignment: .leading, spacing: 3) {
                Text(AgentNames.of(agent)).font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                Text(markdown(turn.text)).textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }
}

/// Tool calls in a row, folded to a count past a few.
private struct ToolRun: View {
    let calls: [String]
    @State private var open = false

    var body: some View {
        let shown = open || calls.count <= 3 ? calls : Array(calls.prefix(2))
        VStack(alignment: .leading, spacing: 2) {
            ForEach(Array(shown.enumerated()), id: \.offset) { _, call in
                Label(call, systemImage: "wrench.and.screwdriver")
                    .labelStyle(ToolLabel())
                    .lineLimit(1).truncationMode(.tail)
            }
            if shown.count < calls.count {
                Button("\(calls.count - shown.count) more tool calls") { open = true }
                    .buttonStyle(.link).font(.caption)
            }
        }
    }

    private struct ToolLabel: LabelStyle {
        func makeBody(configuration: Configuration) -> some View {
            HStack(spacing: 5) {
                configuration.icon.font(.system(size: 9)).foregroundStyle(.tertiary)
                configuration.title.font(.system(size: 11, design: .monospaced)).foregroundStyle(.secondary)
            }
        }
    }
}
