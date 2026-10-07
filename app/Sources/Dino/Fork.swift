import SwiftUI

// MARK: - Fork a session: a new session on a copy of its conversation, by the agent's own fork.

extension DinoModel {
    /// Its agent forks conversations (Claude Code, Codex), it runs on this Mac and has one to fork.
    func canFork(_ s: SessionInfo) -> Bool {
        guard s.host == nil, s.conversation != nil else { return false }
        return launchers.contains { $0.agent_id == s.agent && $0.forks == true }
    }

    /// Why Fork Session is off for `s`, in a few words; nil when it's on.
    func whyNoFork(_ s: SessionInfo) -> String? {
        if s.host != nil { return "Forking runs on this Mac" }
        if !launchers.contains(where: { $0.agent_id == s.agent && $0.forks == true }) { return "This agent can't fork a conversation" }
        if s.conversation == nil { return "Nothing to fork yet: send it a prompt first" }
        return nil
    }

    /// The git repo `s` runs in, as the sidebar last saw it; nil for a plain folder.
    func gitRepo(of s: SessionInfo) -> RepoInfo? {
        guard let cwd = s.cwd else { return nil }
        return repos.filter { !$0.worktrees.isEmpty && $0.worktrees.contains { SessionTree.contains($0.path, cwd) } }
            .max { $0.path.count < $1.path.count }
    }

    /// Fork (⌥⌘B): `s` forked at once, no questions: a new tab beside it on a copy of its
    /// conversation, same agent, model, mode, account and folder, waiting for its next prompt.
    /// Fork with Options… (`forking`) asks for a name, a worktree and a first prompt.
    func forkNow(_ s: SessionInfo) {
        guard canFork(s), !forksStarting.contains(s.id) else { return }
        forksStarting.insert(s.id)
        Task {
            defer { forksStarting.remove(s.id) }
            do {
                try await fork(s.id, name: "\(s.display) (fork)", worktree: false, prompt: "")
            } catch {
                self.error = error.localizedDescription
            }
        }
    }

    /// Fork session `id`; throws dinod's reason. The fork is selected once it's there, its tab
    /// right after the original's.
    func fork(_ id: String, name: String, worktree: Bool, prompt: String) async throws {
        var body: [String: Any] = ["type": "fork", "id": id, "worktree": worktree]
        let name = name.trimmingCharacters(in: .whitespacesAndNewlines)
        let prompt = prompt.trimmingCharacters(in: .whitespacesAndNewlines)
        if !name.isEmpty { body["name"] = name }
        if !prompt.isEmpty { body["prompt"] = prompt }
        let request = body
        let new = try await Task.detached {
            try DinoConnection(path: DinoEnvironment.socketPath).request(request).id
        }.value
        forking = nil
        guard let new else { return }
        if !tabs.contains(new), let at = tabs.firstIndex(of: id) { tabs.insert(new, at: at + 1) }
        select(new)
    }
}

/// Fork with Options…: what to call the fork, whether it gets a worktree of its own, and what to ask it first.
struct ForkSheet: View {
    @EnvironmentObject var model: DinoModel
    @Environment(\.dismiss) private var dismiss
    let session: SessionInfo
    @State private var name = ""
    @State private var worktree = true
    @State private var prompt = ""
    @State private var starting = false
    @State private var error: String?
    @FocusState private var focused: Bool

    private var agentName: String { model.launchers.first { $0.agent_id == session.agent }?.label ?? session.agent }

    var body: some View {
        let repo = model.gitRepo(of: session)
        // No worktree to be had: not a git repo, or one with no commits yet. Shown off, not on.
        let noWorktree = repo == nil || repo?.noCommits == true
        VStack(alignment: .leading, spacing: 14) {
            Label("Fork “\(session.display)”", systemImage: "arrow.triangle.branch")
                .font(.title2.weight(.semibold))
                .lineLimit(1)
            Text("Starts a new session with a copy of this conversation, made by \(agentName), with the same model and mode. The original session doesn't change.\(session.agent.hasPrefix("claude") ? " Permissions you allowed only for this session don't carry over." : "")")
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            LabeledContent("Name") {
                TextField("Name", text: $name, prompt: Text("\(session.display) (fork)"))
                    .textFieldStyle(.roundedBorder)
                    .labelsHidden()
            }
            VStack(alignment: .leading, spacing: 3) {
                Toggle("In a new worktree", isOn: noWorktree ? .constant(false) : $worktree)
                    .disabled(noWorktree)
                Text(repo == nil
                    ? "Not in a git repository: the fork works in the same folder."
                    : repo?.noCommits == true
                    ? "A worktree needs a first commit: the fork works in the same folder."
                    : "Its own git worktree off this session's checkout, uncommitted changes included, so the two don't edit the same files.")
                    .font(.caption).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            TextEditor(text: $prompt)
                .font(.body)
                .focused($focused)
                .scrollContentBackground(.hidden)
                .padding(8)
                .frame(height: 90)
                .background(RoundedRectangle(cornerRadius: 8).fill(Color(nsColor: .textBackgroundColor)))
                .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(.quaternary))
                .overlay(alignment: .topLeading) {
                    if prompt.isEmpty {
                        Text("First prompt for the fork (optional)").foregroundStyle(.tertiary).padding(13).allowsHitTesting(false)
                    }
                }
                .accessibilityLabel("First prompt")
            if let error {
                Text(error).foregroundStyle(SessionStatus.exited.color).font(.callout)
                    .fixedSize(horizontal: false, vertical: true)
            }
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
                Button {
                    start(worktree: worktree && !noWorktree)
                } label: {
                    if starting { ProgressView().controlSize(.small).frame(width: 70) } else { Text("Fork").frame(width: 70) }
                }
                .keyboardShortcut(.return, modifiers: .command)
                .buttonStyle(.borderedProminent)
                .tint(Brand.green)
                .disabled(starting)
            }
        }
        .padding(22)
        .frame(width: 520)
        .onAppear { focused = true }
    }

    private func start(worktree: Bool) {
        starting = true
        error = nil
        let name = name.isEmpty ? "\(session.display) (fork)" : name
        Task {
            do {
                try await model.fork(session.id, name: name, worktree: worktree, prompt: prompt)
            } catch {
                self.error = error.localizedDescription
            }
            starting = false
        }
    }
}
