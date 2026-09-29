import SwiftUI

/// One entry of an agent conversation: `role` is "user", "assistant", "tool" (a call, in brief) or
/// "note" (something that happened, like an interruption).
struct ConversationTurn: Codable, Equatable, Identifiable {
    var role: String
    var text: String
    var id = UUID()

    private enum CodingKeys: String, CodingKey { case role, text }

    /// The same words, whichever read they came from.
    static func == (a: Self, b: Self) -> Bool { a.role == b.role && a.text == b.text }
}

/// A read-only agent conversation, newest at the bottom. Knows nothing about where turns come
/// from: `earlier` (when set) loads the part before the first turn.
struct ConversationView: View {
    let turns: [ConversationTurn]
    var agent = "claude"
    var earlier: (() -> Void)?
    var loadingEarlier = false

    /// Runs of tool calls fold into one row; the text around them is the conversation.
    private var blocks: [Block] {
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

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 12) {
                    if let earlier {
                        HStack {
                            Spacer()
                            if loadingEarlier {
                                ProgressView().controlSize(.small)
                            } else {
                                Button("Load earlier", action: earlier).buttonStyle(.link).font(.caption)
                            }
                            Spacer()
                        }
                    }
                    ForEach(blocks) { block in
                        switch block {
                        case .turn(let t): TurnBubble(turn: t, agent: agent)
                        case .tools(_, let calls): ToolRun(calls: calls)
                        }
                    }
                    Color.clear.frame(height: 1).id("end")
                }
                .padding(14)
            }
            .onAppear { proxy.scrollTo("end", anchor: .bottom) }
            .onChange(of: turns.last?.id) { _, _ in
                // New turns at the end follow; earlier ones loaded at the top don't move the view.
                if !loadingEarlier { proxy.scrollTo("end", anchor: .bottom) }
            }
        }
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

private struct TurnBubble: View {
    let turn: ConversationTurn
    let agent: String

    /// Very long messages are cut; the transcript file has the rest.
    private var text: AttributedString {
        let limit = 4000
        let s = turn.text.count > limit ? String(turn.text.prefix(limit)) + "…" : turn.text
        let md = try? AttributedString(markdown: s, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace))
        return md ?? AttributedString(s)
    }

    var body: some View {
        if turn.role == "note" {
            Text(text).font(.caption).foregroundStyle(.tertiary).frame(maxWidth: .infinity)
        } else if turn.role == "user" {
            HStack {
                Spacer(minLength: 60)
                Text(text)
                    .textSelection(.enabled)
                    .padding(.horizontal, 10).padding(.vertical, 7)
                    .background(RoundedRectangle(cornerRadius: 10).fill(Color.accentColor.opacity(0.14)))
            }
        } else {
            VStack(alignment: .leading, spacing: 3) {
                Text(agent == "codex" ? "Codex" : "Claude").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                Text(text).textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading)
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
