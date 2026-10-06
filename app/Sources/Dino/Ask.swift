import SwiftUI

/// Side chat (⇧⌘;): ask Claude about a session without disturbing it. dinod runs a one-shot
/// `claude -p` that reads the session through `dino mcp --read-only` and can't change anything.
struct AskSheet: View {
    @EnvironmentObject var model: DinoModel
    @Environment(\.dismiss) private var dismiss
    let session: SessionInfo

    struct Exchange: Identifiable {
        let id = UUID()
        var question: String
        /// nil while Claude is answering.
        var answer: String?
        var failed = false
    }

    @State private var question = ""
    @State private var exchanges: [Exchange] = []
    @State private var run: UUID?
    @FocusState private var focused: Bool

    private var asking: Bool { run != nil }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            VStack(alignment: .leading, spacing: 4) {
                Label("Ask about \(session.display)", systemImage: "bubble.left.and.text.bubble.right").font(.title2.weight(.semibold))
                Text("Claude reads the session and its folder to answer. It can't type into the session or change files. Each question starts fresh.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if !exchanges.isEmpty {
                ScrollViewReader { proxy in
                    ScrollView {
                        VStack(alignment: .leading, spacing: 16) {
                            ForEach(exchanges) { e in
                                ExchangeView(exchange: e, session: session.display).id(e.id)
                            }
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(12)
                    }
                    // Links in answers go where a ⌘-click in the session's terminal would.
                    .environment(\.openURL, OpenURLAction { url in
                        model.openLink(LinkPolicy.link(url), from: session.id)
                        return .handled
                    })
                    .frame(minHeight: 120, maxHeight: 380)
                    .background(RoundedRectangle(cornerRadius: 8).fill(Color(nsColor: .textBackgroundColor)))
                    .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(.quaternary))
                    .onChange(of: exchanges.last?.answer) { _, _ in
                        if let last = exchanges.last { withAnimation { proxy.scrollTo(last.id, anchor: .top) } }
                    }
                }
            }
            TextField("What's it doing? Is it stuck? What did it change?", text: $question, axis: .vertical)
                .lineLimit(1...5)
                .textFieldStyle(.roundedBorder)
                .focused($focused)
                .onSubmit(ask)
                .disabled(asking)
            HStack {
                Spacer()
                if asking {
                    Button("Stop") { cancel() }
                }
                Button(exchanges.isEmpty ? "Cancel" : "Done") {
                    cancel()
                    dismiss()
                }
                .keyboardShortcut(.cancelAction)
                Button("Ask") { ask() }
                    .keyboardShortcut(.return, modifiers: .command)
                    .buttonStyle(.borderedProminent)
                    .tint(Brand.green)
                    .disabled(asking || question.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .padding(22)
        .frame(width: 600)
        .onAppear { focused = true }
    }

    private func ask() {
        let q = question.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !q.isEmpty, !asking else { return }
        let id = session.id
        let this = UUID()
        run = this
        question = ""
        exchanges.append(Exchange(question: q))
        let index = exchanges.count - 1
        Task {
            let result: Result<String, Error> = await Task.detached {
                Result { try DinoConnection(path: DinoEnvironment.socketPath).ask(session: id, question: q) }
            }.value
            // Stopped: the exchange already says so.
            guard run == this else { return }
            switch result {
            case let .success(a): exchanges[index].answer = a
            case let .failure(e):
                exchanges[index].answer = e.localizedDescription
                exchanges[index].failed = true
            }
            run = nil
            focused = true
        }
    }

    private func cancel() {
        guard asking else { return }
        run = nil
        if let i = exchanges.indices.last, exchanges[i].answer == nil {
            exchanges[i].answer = "Stopped."
            exchanges[i].failed = true
        }
        let id = session.id
        Task.detached { try? DinoConnection(path: DinoEnvironment.socketPath).cancelAsk(session: id) }
    }
}

private struct ExchangeView: View {
    let exchange: AskSheet.Exchange
    let session: String

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(exchange.question).font(.body.weight(.semibold)).textSelection(.enabled)
            if let answer = exchange.answer {
                if exchange.failed {
                    Text(answer).font(.callout).foregroundStyle(SessionStatus.exited.color)
                } else {
                    Text(Self.markdown(answer)).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
                }
            } else {
                HStack(spacing: 6) {
                    ProgressView().controlSize(.small)
                    Text("Reading \(session)…").font(.callout).foregroundStyle(.secondary)
                }
            }
        }
    }

    /// Inline Markdown (bold, code, links), line breaks kept; block syntax shows as written.
    private static func markdown(_ s: String) -> AttributedString {
        (try? AttributedString(markdown: s, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace))) ?? AttributedString(s)
    }
}

/// Sessions that started or messaged each other through `dino mcp`, under a sidebar row:
/// "from claude" on the one it came from, the other's name on the one that reached out. Each selects that session.
struct PeerChips: View {
    @EnvironmentObject var model: DinoModel
    let session: SessionInfo

    private struct Link: Identifiable {
        var id: String
        var text: String
        var icon: String
        var help: String
    }

    private var links: [Link] {
        let find = { (id: String) in model.sessions.first { $0.id == id } }
        var out: [Link] = []
        if let by = session.started_by, let p = find(by) {
            out.append(Link(id: p.id, text: "from \(p.display)", icon: "arrow.turn.left.up", help: "Started by \(p.display)'s agent"))
        }
        if let by = session.messaged_by, by != session.started_by, let p = find(by) {
            out.append(Link(id: p.id, text: "from \(p.display)", icon: "text.bubble", help: "Last messaged by \(p.display)'s agent"))
        }
        for c in model.sessions where c.id != session.id {
            if c.started_by == session.id {
                out.append(Link(id: c.id, text: c.display, icon: "arrow.turn.right.down", help: "Its agent started \(c.display)"))
            } else if c.messaged_by == session.id {
                out.append(Link(id: c.id, text: c.display, icon: "text.bubble", help: "Its agent last messaged \(c.display)"))
            }
        }
        return out
    }

    var body: some View {
        let links = links
        if !links.isEmpty {
            HStack(spacing: 4) {
                ForEach(links) { l in
                    Button { model.select(l.id) } label: {
                        HStack(spacing: 2) {
                            Image(systemName: l.icon).imageScale(.small)
                            Text(l.text).lineLimit(1).truncationMode(.middle)
                        }
                        .padding(.horizontal, 5)
                        .padding(.vertical, 1)
                        .background(Capsule().fill(.quaternary.opacity(0.6)))
                    }
                    .buttonStyle(.plain)
                    .help(l.help)
                }
            }
            .font(.caption)
            .foregroundStyle(.secondary)
        }
    }
}
