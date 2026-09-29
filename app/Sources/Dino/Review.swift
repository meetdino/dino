import SwiftUI

/// A comment on one line of a session's changes, held until the user sends the review to the agent.
struct ReviewComment: Identifiable, Equatable {
    let id = UUID()
    var at: LineRef
    /// The line as it read when commented on, quoted to the agent.
    var code: String
    var text: String

    /// One message for the agent, in file order; any agent reads `path:line`.
    static func message(_ list: [ReviewComment]) -> String {
        let items = list.sorted { ($0.at.path, $0.at.line) < ($1.at.path, $1.at.line) }.map { c in
            let code = c.code.trimmingCharacters(in: .whitespaces)
            let quote = code.isEmpty ? "" : " `\(code.prefix(100))`"
            let body = c.text.split(separator: "\n", omittingEmptySubsequences: false).joined(separator: "\n  ")
            return "- \(c.at.path):\(c.at.line)\(c.at.removed ? " (removed line)" : "")\(quote): \(body)"
        }
        return (["Review comments on the current changes:"] + items).joined(separator: "\n")
    }
}

/// A line of a file: in the new version, or in the old one when it was removed.
struct LineRef: Hashable {
    var path: String
    var line: UInt32
    var removed: Bool

    init?(path: String, _ l: DiffLine) {
        switch l.kind {
        case "del": guard let n = l.old else { return nil }; self.init(path: path, line: n, removed: true)
        case "add", "ctx": guard let n = l.new else { return nil }; self.init(path: path, line: n, removed: false)
        default: return nil
        }
    }

    init(path: String, line: UInt32, removed: Bool) {
        self.path = path
        self.line = line
        self.removed = removed
    }
}

/// What the selected session changed, beside its terminal: click a line to leave the agent a comment.
struct ReviewPanel: View {
    @EnvironmentObject var model: DinoModel
    let session: SessionInfo
    @State private var changes: Changes?
    @State private var collapsed: Set<String> = []
    @State private var composing: LineRef?
    @State private var draft = ""
    @State private var sending = false
    @State private var sendError: String?

    private var comments: [ReviewComment] { model.comments[session.id] ?? [] }

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            content.frame(maxWidth: .infinity, maxHeight: .infinity)
            if !comments.isEmpty {
                Divider()
                footer
            }
        }
        .background(Color(nsColor: .windowBackgroundColor))
        // Agents keep editing while you read: follow along.
        .task(id: session.id) {
            changes = nil
            composing = nil
            while !Task.isCancelled {
                if let c = await model.changes(session.id), c != changes { changes = c }
                try? await Task.sleep(for: .seconds(2))
            }
        }
    }

    private var header: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            VStack(alignment: .leading, spacing: 2) {
                Text("Changes").font(.headline)
                if let c = changes, c.note == nil {
                    Text("\(shortPath(c.root)) · since \(c.base)").font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.head)
                }
            }
            Spacer()
            if let files = changes?.files, !files.isEmpty {
                StatText(stat: DiffStat(
                    files: UInt32(files.count),
                    added: files.reduce(0) { $0 + $1.added },
                    removed: files.reduce(0) { $0 + $1.removed }
                ))
                .font(.caption.monospacedDigit())
            }
            Button { model.showReview = false } label: { Image(systemName: "xmark") }
                .buttonStyle(.borderless)
                .help("Hide changes (⇧⌘D)")
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 9)
    }

    @ViewBuilder
    private var content: some View {
        if let c = changes {
            if let note = c.note {
                Placeholder(text: note)
            } else if c.files.isEmpty {
                Placeholder(text: "\(session.name) hasn't changed anything since \(c.base)")
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 0, pinnedViews: [.sectionHeaders]) {
                        ForEach(c.files) { f in
                            Section {
                                if !collapsed.contains(f.path) { fileBody(f) }
                            } header: {
                                FileHeader(file: f, collapsed: collapsed.contains(f.path)) {
                                    if collapsed.contains(f.path) { collapsed.remove(f.path) } else { collapsed.insert(f.path) }
                                }
                            }
                        }
                    }
                }
            }
        } else {
            ProgressView().controlSize(.small)
        }
    }

    @ViewBuilder
    private func fileBody(_ f: FileDiff) -> some View {
        if f.binary {
            Placeholder(text: "Binary file").padding(.vertical, 8)
        }
        ForEach(Array(f.lines.enumerated()), id: \.offset) { _, line in
            let ref = LineRef(path: f.path, line)
            DiffRow(line: line, commenting: ref != nil && ref == composing) {
                guard let ref else { return }
                draft = ""
                composing = composing == ref ? nil : ref
            }
            if let ref {
                ForEach(comments.filter { $0.at == ref }) { c in CommentCard(comment: c) { remove(c) } }
                if composing == ref {
                    CommentComposer(text: $draft, onAdd: { add(at: ref, code: line.text) }, onCancel: { composing = nil })
                }
            }
        }
        if f.truncated {
            Placeholder(text: "Too long to show in full").padding(.vertical, 8)
        }
        // Comments on lines that have since changed; they still go to the agent.
        let shown = Set(f.lines.compactMap { LineRef(path: f.path, $0) })
        ForEach(comments.filter { $0.at.path == f.path && !shown.contains($0.at) }) { c in
            CommentCard(comment: c, stale: true) { remove(c) }
        }
    }

    private var footer: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let sendError {
                ErrorLine(message: sendError)
            }
            HStack {
                Text("\(comments.count) comment\(comments.count == 1 ? "" : "s")").font(.callout).foregroundStyle(.secondary)
                Spacer()
                Button("Clear") { model.comments[session.id] = nil }
                Button {
                    sending = true
                    Task {
                        do {
                            try await model.sendComments(to: session.id)
                            sendError = nil
                        } catch {
                            sendError = error.localizedDescription
                        }
                        sending = false
                    }
                } label: {
                    Text("Send to \(session.name)")
                }
                .buttonStyle(.borderedProminent)
                .tint(Brand.green)
                .keyboardShortcut(.return, modifiers: .command)
                .disabled(sending || session.exited)
                .help("Type the comments into \(session.name) as one message (⌘↩)")
            }
        }
        .padding(12)
    }

    private func add(at ref: LineRef, code: String) {
        let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        model.comments[session.id, default: []].append(ReviewComment(at: ref, code: code, text: text))
        composing = nil
        draft = ""
    }

    private func remove(_ c: ReviewComment) {
        model.comments[session.id]?.removeAll { $0.id == c.id }
        if model.comments[session.id]?.isEmpty == true { model.comments[session.id] = nil }
    }
}

private let diffFont = Font.system(size: 11.5, design: .monospaced)

private struct Placeholder: View {
    let text: String

    var body: some View {
        Text(text).font(.callout).foregroundStyle(.tertiary).multilineTextAlignment(.center)
            .padding(20).frame(maxWidth: .infinity)
    }
}

private struct FileHeader: View {
    let file: FileDiff
    let collapsed: Bool
    let toggle: () -> Void

    var body: some View {
        let dir = (file.path as NSString).deletingLastPathComponent
        Button(action: toggle) {
            HStack(spacing: 6) {
                Image(systemName: "chevron.right")
                    .rotationEffect(.degrees(collapsed ? 0 : 90))
                    .font(.caption2.weight(.semibold)).foregroundStyle(.secondary).frame(width: 10)
                StatusLetter(status: file.status)
                Text((file.path as NSString).lastPathComponent).fontWeight(.semibold).lineLimit(1)
                if !dir.isEmpty {
                    Text(dir).foregroundStyle(.secondary).lineLimit(1).truncationMode(.head)
                }
                if let old = file.old_path {
                    Text("← \(old)").foregroundStyle(.tertiary).lineLimit(1).truncationMode(.head)
                }
                Spacer(minLength: 8)
                Text("+\(file.added)").foregroundStyle(Brand.green)
                Text("−\(file.removed)").foregroundStyle(SessionStatus.exited.color)
            }
            .font(.callout.monospacedDigit())
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        // Opaque, so lines scroll under the pinned header rather than through it.
        .background(Color(nsColor: .windowBackgroundColor))
        .overlay(alignment: .bottom) { Divider() }
        .help(file.path)
    }
}

private struct StatusLetter: View {
    let status: String

    var body: some View {
        let (letter, color): (String, Color) = switch status {
        case "added": ("A", Brand.green)
        case "deleted": ("D", SessionStatus.exited.color)
        case "renamed": ("R", SessionStatus.done.color)
        default: ("M", SessionStatus.needsYou.color)
        }
        Text(letter)
            .font(.system(size: 9, weight: .bold, design: .monospaced))
            .frame(width: 14, height: 14)
            .background(RoundedRectangle(cornerRadius: 3).fill(color.opacity(0.18)))
            .foregroundStyle(color)
    }
}

private struct DiffRow: View {
    let line: DiffLine
    let commenting: Bool
    let onComment: () -> Void
    @State private var hovering = false

    var body: some View {
        if line.kind == "hunk" {
            Text(line.text)
                .font(diffFont).foregroundStyle(.secondary).lineLimit(1)
                .padding(.horizontal, 10).padding(.vertical, 3)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(SessionStatus.done.color.opacity(0.07))
        } else {
            HStack(alignment: .top, spacing: 0) {
                number(line.old)
                number(line.new)
                ZStack {
                    if hovering || commenting {
                        Image(systemName: "plus.bubble.fill").font(.system(size: 10)).foregroundStyle(Brand.green)
                    } else {
                        Text(line.kind == "add" ? "+" : line.kind == "del" ? "−" : " ").foregroundStyle(tint)
                    }
                }
                .frame(width: 18)
                Text(line.text.isEmpty ? " " : line.text)
                    .foregroundStyle(line.kind == "ctx" ? Color.primary : tint)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .font(diffFont)
            .padding(.vertical, 0.5)
            .background(hovering ? Color.primary.opacity(0.06) : background)
            .contentShape(Rectangle())
            .onHover { hovering = $0 }
            .onTapGesture(perform: onComment)
            .help("Click to comment on this line")
        }
    }

    private var tint: Color {
        switch line.kind {
        case "add": Brand.green
        case "del": SessionStatus.exited.color
        default: .secondary
        }
    }

    private var background: Color {
        switch line.kind {
        case "add": Brand.green.opacity(0.1)
        case "del": SessionStatus.exited.color.opacity(0.1)
        default: .clear
        }
    }

    private func number(_ n: UInt32?) -> some View {
        Text(n.map(String.init) ?? "")
            .foregroundStyle(.tertiary)
            .frame(width: 38, alignment: .trailing)
            .padding(.trailing, 4)
    }
}

private struct CommentCard: View {
    let comment: ReviewComment
    var stale = false
    let onDelete: () -> Void

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: "text.bubble.fill").foregroundStyle(Brand.green).font(.caption)
            VStack(alignment: .leading, spacing: 2) {
                if stale {
                    Text("line \(comment.at.line), since changed").font(.caption).foregroundStyle(.tertiary)
                }
                Text(comment.text).font(.callout).textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading)
            }
            Button(action: onDelete) { Image(systemName: "trash") }
                .buttonStyle(.borderless).foregroundStyle(.secondary)
                .help("Delete comment")
        }
        .padding(8)
        .background(RoundedRectangle(cornerRadius: 7).fill(Color(nsColor: .controlBackgroundColor)))
        .overlay(RoundedRectangle(cornerRadius: 7).strokeBorder(Brand.green.opacity(0.35)))
        .padding(.horizontal, 12).padding(.vertical, 4)
    }
}

private struct CommentComposer: View {
    @Binding var text: String
    let onAdd: () -> Void
    let onCancel: () -> Void
    @FocusState private var focused: Bool

    var body: some View {
        VStack(alignment: .trailing, spacing: 6) {
            TextField("Comment for the agent", text: $text, axis: .vertical)
                .textFieldStyle(.plain)
                .lineLimit(1 ... 8)
                .focused($focused)
                .onSubmit(onAdd)
                .padding(7)
                .background(RoundedRectangle(cornerRadius: 6).fill(Color(nsColor: .textBackgroundColor)))
                .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(Brand.green.opacity(0.6)))
            HStack {
                Text("↵ add · ⌥↵ new line").font(.caption).foregroundStyle(.tertiary)
                Spacer()
                Button("Cancel", action: onCancel).keyboardShortcut(.cancelAction)
                Button("Comment", action: onAdd)
                    .buttonStyle(.borderedProminent).tint(Brand.green)
                    .disabled(text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
            .controlSize(.small)
        }
        .padding(.horizontal, 12).padding(.vertical, 6)
        .onAppear { focused = true }
    }
}
