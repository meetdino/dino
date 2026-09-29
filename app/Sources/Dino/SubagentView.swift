import AppKit
import SwiftUI

/// A subagent and what it has said and done so far.
struct SubagentDetail: Decodable, Equatable {
    var id: String
    var session: String
    /// Its own worktree, when it has one.
    var worktree: String?
    var agent_type: String?
    var description: String?
    var running: Bool
    /// Nil when its conversation can't be read (not Claude's, or not written yet).
    var turns: [TurnInfo]?
}

struct TurnInfo: Decodable, Equatable {
    /// "task", "user", "agent", "tool" or "note".
    var role: String
    var text: String
}

private struct SubagentResponse: Decodable {
    var subagent: SubagentDetail
}

/// Which subagent to show: the one that made a worktree, or one of a session's by id.
enum SubagentRef: Hashable {
    case worktree(String)
    case agent(session: String, id: String)

    /// As `DinoModel.selected` holds it.
    var selection: String {
        switch self {
        case .worktree(let path): "dir:\(path)"
        case .agent(let session, let id): "agent:\(session)/\(id)"
        }
    }

    var worktreePath: String? {
        if case .worktree(let path) = self { return path }
        return nil
    }
}

extension DinoConnection {
    func readSubagent(_ ref: SubagentRef) throws -> SubagentDetail {
        var body: [String: Any] = ["type": "read_subagent"]
        switch ref {
        case .worktree(let path): body["worktree"] = path
        case .agent(let session, let id):
            body["session"] = session
            body["agent"] = id
        }
        return try JSONDecoder().decode(SubagentResponse.self, from: send(body)).subagent
    }
}

extension RepoNode {
    /// Every place in the repo, wherever the sidebar files it.
    var allPlaces: [PlaceNode] { places + subagents.values.flatMap { $0 } + others + merged }
}

extension DinoModel {
    /// The worktree `path` is, when dino's tree knows it.
    func worktree(at path: String) -> Worktree? {
        repos.lazy.flatMap(\.worktrees).first { $0.path == path }
    }

    /// The subagent the main area shows: one picked in the Tasks pane, or the one that made the
    /// selected worktree.
    var shownSubagent: SubagentRef? {
        guard let sel = selected else { return nil }
        if sel.hasPrefix("agent:") {
            let parts = sel.dropFirst(6).split(separator: "/", maxSplits: 1).map(String.init)
            return parts.count == 2 ? .agent(session: parts[0], id: parts[1]) : nil
        }
        if sel.hasPrefix("dir:") {
            let path = String(sel.dropFirst(4))
            return worktree(at: path)?.owner != nil ? .worktree(path) : nil
        }
        return nil
    }
}

/// The main area for a subagent: who started it, its worktree if it has one, and its
/// conversation so far, read-only. A subagent runs inside its parent's agent, so there's no
/// terminal of its own to show.
struct SubagentPane: View {
    @EnvironmentObject var model: DinoModel
    let ref: SubagentRef
    @State private var detail: SubagentDetail?
    @State private var failed: String?
    @State private var startHere = false

    /// What the sidebar and Tasks pane already know, until its detail arrives.
    private var known: (session: String, description: String?, agentType: String?, running: Bool)? {
        switch ref {
        case .worktree(let path):
            return model.worktree(at: path)?.owner.map { ($0.session, $0.description, $0.agentType, $0.running) }
        case .agent(let session, let id):
            let s = model.sessions.first { $0.id == session }
            return s?.tasks?.subagents.first { $0.id == id }.map { (session, $0.description, $0.agent_type, $0.running && s?.exited != true) }
        }
    }

    private var session: String? { detail?.session ?? known?.session }
    private var path: String? { detail?.worktree ?? ref.worktreePath }
    private var worktree: Worktree? { path.flatMap { model.worktree(at: $0) } }
    private var parent: SessionInfo? { model.sessions.first { $0.id == session } }
    private var running: Bool { known?.running ?? detail?.running ?? false }
    private var place: PlaceNode? {
        guard let path else { return nil }
        return SessionTree.build(repos: model.repos, sessions: model.sessions, groups: model.groups)
            .repos.lazy.flatMap(\.allPlaces).first { $0.path == path }
    }

    var body: some View {
        if startHere {
            EmptyState()
                .overlay(alignment: .topLeading) {
                    Button { startHere = false } label: { Label("Back to the Subagent", systemImage: "chevron.left") }
                        .buttonStyle(.link)
                        .padding(16)
                }
        } else {
            VStack(spacing: 0) {
                header
                Divider()
                TranscriptView(turns: detail?.turns, loading: detail == nil && failed == nil,
                               unreadable: failed ?? "Its conversation can’t be read: only Claude’s subagents write one dino can show.")
            }
            // Polls while it runs; once more when it stops.
            .task(id: "\(ref.selection)|\(running)") { await follow() }
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                if running {
                    ProgressView().controlSize(.small)
                } else {
                    Image(systemName: "checkmark.circle").foregroundStyle(.secondary)
                }
                Text(known?.description ?? detail?.description ?? "Subagent")
                    .font(.title3.weight(.semibold))
                    .lineLimit(2)
                    .textSelection(.enabled)
            }
            HStack(spacing: 6) {
                if let t = known?.agentType ?? detail?.agent_type {
                    Text(t)
                    Text("·")
                }
                Text(running ? "Running" : "Finished")
                if let session {
                    Text("·")
                    Text("in")
                    Button(parent?.name ?? "session \(session)") { model.select(session) }
                        .buttonStyle(.link)
                        .disabled(parent == nil)
                        .help(parent == nil ? "That session is gone" : "Go to the session it runs in")
                }
                if let branch = worktree?.branch {
                    Text("·")
                    Label(branch, systemImage: "arrow.triangle.branch").labelStyle(.titleAndIcon).lineLimit(1)
                }
                if let git = worktree?.git {
                    if git.added > 0 { Text("+\(git.added)").foregroundStyle(.green) }
                    if git.removed > 0 { Text("−\(git.removed)").foregroundStyle(.red) }
                }
            }
            .font(.callout)
            .foregroundStyle(.secondary)
            if let path, let session {
                HStack(spacing: 8) {
                    Button {
                        model.folder = URL(fileURLWithPath: path)
                        startHere = true
                    } label: { Label("Start a Session Here…", systemImage: "plus") }
                        .help("A new session of its own in this worktree")
                    if let editor = ExternalEditor.preferred(for: path) {
                        Button("Open in \(editor.name)") { editor.openFolder(path) }
                    }
                    Menu {
                        if let place { WorktreeClosingItems(place: place) }
                        WorktreeMenuItems(path: path, session: session, inSidebar: ref.worktreePath == nil)
                    } label: {
                        Label("Worktree", systemImage: "ellipsis.circle")
                    }
                    .menuStyle(.borderlessButton)
                    .fixedSize()
                    .help(shortPath(path))
                }
            }
            Text(path == nil
                ? "Read-only: it runs inside \(parent?.name ?? "its parent session")’s agent, so it can’t be typed into."
                : "Read-only: it runs inside \(parent?.name ?? "its parent session")’s agent, so it can’t be typed into. Start a session here to work in its worktree yourself.")
                .font(.caption)
                .foregroundStyle(.tertiary)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(16)
    }

    private func follow() async {
        while !Task.isCancelled {
            let ref = ref
            let result = await Task.detached { () -> Result<SubagentDetail, Error> in
                Result { try DinoConnection(path: DinoEnvironment.socketPath).readSubagent(ref) }
            }.value
            switch result {
            case .success(let d):
                if d != detail { detail = d }
                failed = nil
            case .failure(let e):
                failed = e.localizedDescription
            }
            guard running || detail?.running == true else { return }
            try? await Task.sleep(for: .seconds(2))
        }
    }
}

/// A conversation read from an agent's transcript, oldest first, kept scrolled to the newest.
struct TranscriptView: View {
    let turns: [TurnInfo]?
    var loading = false
    /// Shown when there's nothing to show and it isn't loading.
    var unreadable = "This conversation can’t be read."

    var body: some View {
        if let turns, !turns.isEmpty {
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 10) {
                    ForEach(turns.indices, id: \.self) { i in TurnRow(turn: turns[i]) }
                }
                .padding(16)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .defaultScrollAnchor(.bottom)
        } else {
            VStack(spacing: 8) {
                if loading {
                    ProgressView().controlSize(.small)
                } else {
                    Text(turns == nil ? unreadable : "Nothing said yet.")
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                }
            }
            .padding(40)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }
}

private struct TurnRow: View {
    let turn: TurnInfo

    var body: some View {
        switch turn.role {
        case "tool":
            Label(turn.text, systemImage: "wrench.and.screwdriver")
                .font(.system(.caption, design: .monospaced))
                .foregroundStyle(.secondary)
                .lineLimit(2)
                .truncationMode(.tail)
        case "note":
            Text(turn.text)
                .font(.caption.italic())
                .foregroundStyle(.tertiary)
                .frame(maxWidth: .infinity)
        case "task", "user":
            VStack(alignment: .leading, spacing: 4) {
                Text(turn.role == "task" ? "Task" : "Message").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                Text(turn.text).textSelection(.enabled)
            }
            .padding(10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Brand.green.opacity(0.08), in: RoundedRectangle(cornerRadius: 8))
        default:
            Text((try? AttributedString(markdown: turn.text, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace))) ?? AttributedString(turn.text))
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}
