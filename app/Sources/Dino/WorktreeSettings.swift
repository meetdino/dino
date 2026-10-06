import AppKit
import SwiftUI

extension DinoSettings {
    /// Where dino's worktrees go and what their branches are called; nil from an older dinod.
    struct Worktrees: Codable, Equatable {
        /// Relative: inside each repo. Absolute or `~/…`: a folder per repo under it.
        var location: String
        var branch_prefix: String

        static let defaultLocation = "~/.dino/worktrees"
        static let defaultPrefix = "dino/"
    }
}

/// Settings → Workspaces → Worktrees: where they go, trust, archiving after a PR,
/// and the ones on disk.
struct WorktreesPane: View {
    @EnvironmentObject var store: SettingsStore
    @State private var location = ""
    @State private var prefix = ""
    @State private var stored: [StoredWorktree]?
    @State private var removing: Set<String> = []
    @State private var error: String?
    @State private var confirmFree = false
    @State private var freeing = false
    @State private var freed: String?

    private var current: DinoSettings.Worktrees? { store.settings?.worktrees }
    private var reclaimable: [StoredWorktree] { (stored ?? []).filter { $0.reclaimable == true && !removing.contains($0.path) } }
    private var reclaimableSize: String? {
        let sizes = reclaimable.compactMap(\.size)
        guard !sizes.isEmpty else { return nil }
        return ByteCountFormatter.string(fromByteCount: Int64(sizes.reduce(0, +)), countStyle: .file)
    }

    var body: some View {
        Form {
            Section {
                LabeledContent("Worktree location") {
                    HStack {
                        TextField("", text: $location, prompt: Text(DinoSettings.Worktrees.defaultLocation))
                            .onSubmit { saveLocation() }
                            .frame(minWidth: 180)
                        Button("Choose…") { chooseLocation() }
                    }
                }
                .orgLocked("worktrees.location")
                LabeledContent("Branch prefix") {
                    TextField("", text: $prefix, prompt: Text(DinoSettings.Worktrees.defaultPrefix))
                        .onSubmit { savePrefix() }
                        .frame(width: 140)
                }
                .orgLocked("worktrees.branch_prefix")
            } footer: {
                Footnote("A relative location is created inside each repo and kept out of git status. An absolute location gets one folder per repo. Changes apply to new worktrees. Branches are named like \(prefixShown)claude-3f2a; an automation's runs are named after the automation.")
            }
            Section {
                Toggle("Trust worktrees when the repo is trusted", isOn: Binding(
                    get: { store.settings?.policies.worktree_trust ?? true },
                    set: { on in store.update { $0.policies.worktree_trust = on } }
                ))
                .orgLocked("policies.worktree_trust")
            } header: {
                Text("Trust")
            } footer: {
                Footnote("Claude Code asks whether to trust every new folder. It takes a worktree's trust from its repo, but not a trusted folder inside the repo: dino marks that same folder trusted in each worktree it makes, and removes the mark when the worktree is removed. Codex handles this itself.")
            }
            Section {
                Toggle("Archive sessions after their PR merges or closes", isOn: Binding(
                    get: { store.settings?.policies.close_merged ?? false },
                    set: { on in store.update { $0.policies.close_merged = on } }
                ))
                .orgLocked("policies.close_merged")
            } header: {
                Text("Pull Requests")
            } footer: {
                Footnote("When a session's pull request is merged or closed, dino archives the session once its agent is idle. After a merge, dino also removes the session's worktree if nothing in it would be lost. After a close, the worktree is kept, since the work wasn't merged. Unarchive a session to pick up where it left off, worktree included. Sessions outside a dino worktree aren't archived.")
            }
            BuildCacheSection()
            Section {
                if let stored {
                    if stored.isEmpty {
                        Text("There are no worktrees made by dino on disk.").foregroundStyle(.secondary)
                    }
                    ForEach(stored) { w in row(w) }
                } else {
                    HStack { ProgressView().controlSize(.small); Text("Looking…").foregroundStyle(.secondary) }
                }
            } header: {
                HStack {
                    Text("Storage")
                    if let total { Text(total).foregroundStyle(.secondary).monospacedDigit() }
                    Spacer()
                    if freeing {
                        ProgressView().controlSize(.small)
                    } else {
                        Button(reclaimableSize.map { "Free Up \($0)…" } ?? "Free Up Space…") { confirmFree = true }
                            .disabled(reclaimable.isEmpty)
                            .help("Removes worktrees that are safe to delete: no running session, no uncommitted changes, and all commits merged or pushed")
                    }
                }
            } footer: {
                VStack(alignment: .leading, spacing: 4) {
                    if let freed { Text(freed).font(.callout).foregroundStyle(.secondary) }
                    Footnote("Lists only worktrees dino made. dino never removes a worktree with uncommitted changes, and keeps branches that aren't merged. When you restart an ended or archived session, dino recreates its worktree from its branch.")
                }
            }
            if let error {
                Label(error, systemImage: "exclamationmark.triangle.fill").foregroundStyle(.red).font(.callout)
            }
        }
        .formStyle(.grouped)
        .disabled(store.settings == nil)
        .onAppear { sync() }
        // Typed but not submitted when the pane closes: keep it.
        .onDisappear {
            if location != (current?.location ?? DinoSettings.Worktrees.defaultLocation) { saveLocation() }
            if prefix != (current?.branch_prefix ?? DinoSettings.Worktrees.defaultPrefix) { savePrefix() }
        }
        .onChange(of: current) { _, _ in sync() }
        .task {
            // Sizes arrive as dinod measures them, off its main thread.
            while !Task.isCancelled {
                if let list = try? await Task.detached(operation: { try DinoConnection(path: DinoEnvironment.socketPath).storage() }).value {
                    stored = list
                    removing.formIntersection(list.map(\.path))
                }
                try? await Task.sleep(for: .seconds(2))
            }
        }
        .confirmationDialog("Remove \(reclaimable.count) worktree\(reclaimable.count == 1 ? "" : "s")?", isPresented: $confirmFree) {
            Button("Remove \(reclaimable.count)", role: .destructive) { freeUpSpace() }
        } message: {
            Text("No sessions are running in them, and their work is merged or pushed, so nothing is lost. Ended sessions in them are archived. Their merged branches are deleted too.")
        }
    }

    private var prefixShown: String {
        let p = prefix.trimmingCharacters(in: .whitespaces)
        return p.isEmpty ? DinoSettings.Worktrees.defaultPrefix : p
    }

    private var total: String? {
        guard let stored, !stored.isEmpty else { return nil }
        let sizes = stored.compactMap(\.size)
        guard sizes.count == stored.count else { return nil }
        return ByteCountFormatter.string(fromByteCount: Int64(sizes.reduce(0, +)), countStyle: .file)
    }

    @ViewBuilder private func row(_ w: StoredWorktree) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(w.branch).font(.body.monospaced()).lineLimit(1)
                    StateChip(worktree: w)
                }
                Text(shortPath(w.path)).font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle)
                    .help(w.path)
            }
            Spacer()
            Text(w.size.map { ByteCountFormatter.string(fromByteCount: Int64($0), countStyle: .file) } ?? "—")
                .font(.callout.monospacedDigit())
                .foregroundStyle(.secondary)
                .help(w.size == nil ? "Measuring…" : "On disk")
            Button { NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: w.path)]) } label: { Image(systemName: "magnifyingglass") }
                .buttonStyle(.borderless)
                .help("Show in Finder")
                .accessibilityLabel("Show \(w.branch) in Finder")
            if removing.contains(w.path) {
                ProgressView().controlSize(.small)
            } else {
                Button("Remove") { remove([w]) }
                    .disabled(!w.removable)
                    .help(removeHelp(w))
            }
        }
    }

    private func removeHelp(_ w: StoredWorktree) -> String {
        if let s = w.session { return "\(s) is running in it" }
        if w.session_state == "ended" { return "Remove it. Its ended session is archived, and gets the worktree back from \(w.branch) when you restart it." }
        if w.dirty { return "Has uncommitted changes, so dino won't remove it" }
        return w.archived
            ? "Remove it. Its archived session gets the worktree back from \(w.branch) when you unarchive it."
            : "Remove it, and its branch if it's merged"
    }

    private func remove(_ list: [StoredWorktree]) {
        let paths = list.map(\.path)
        removing.formUnion(paths)
        error = nil
        Task {
            let failed = await Task.detached { () -> [String] in
                paths.compactMap { p in
                    do {
                        try DinoConnection(path: DinoEnvironment.socketPath).removeStored(p)
                        return nil
                    } catch {
                        return error.localizedDescription
                    }
                }
            }.value
            if let list = try? await Task.detached(operation: { try DinoConnection(path: DinoEnvironment.socketPath).storage() }).value {
                stored = list
            }
            removing.subtract(paths)
            error = failed.isEmpty ? nil : failed.joined(separator: "\n")
        }
    }

    private func freeUpSpace() {
        freeing = true
        error = nil
        freed = nil
        Task {
            let result = await Task.detached { () -> Result<Freed, Error> in
                Result { try DinoConnection(path: DinoEnvironment.socketPath).freeUpSpace() }
            }.value
            if let list = try? await Task.detached(operation: { try DinoConnection(path: DinoEnvironment.socketPath).storage() }).value {
                stored = list
            }
            freeing = false
            switch result {
            case .success(let f):
                let n = f.removed.count
                freed = n == 0
                    ? "Nothing to remove. Every remaining worktree has unmerged work or a running session."
                    : "Removed \(n) worktree\(n == 1 ? "" : "s"), freeing \(ByteCountFormatter.string(fromByteCount: Int64(f.bytes), countStyle: .file))."
            case .failure(let e):
                error = e.localizedDescription
            }
        }
    }

    private func sync() {
        location = current?.location ?? DinoSettings.Worktrees.defaultLocation
        prefix = current?.branch_prefix ?? DinoSettings.Worktrees.defaultPrefix
    }

    private func saveLocation() {
        let l = location.trimmingCharacters(in: .whitespaces)
        store.update { s in
            var w = s.worktrees ?? .init(location: DinoSettings.Worktrees.defaultLocation, branch_prefix: DinoSettings.Worktrees.defaultPrefix)
            w.location = l.isEmpty ? DinoSettings.Worktrees.defaultLocation : l
            s.worktrees = w
        }
    }

    private func savePrefix() {
        let p = prefix.trimmingCharacters(in: .whitespaces)
        store.update { s in
            var w = s.worktrees ?? .init(location: DinoSettings.Worktrees.defaultLocation, branch_prefix: DinoSettings.Worktrees.defaultPrefix)
            w.branch_prefix = p.isEmpty ? DinoSettings.Worktrees.defaultPrefix : p
            s.worktrees = w
        }
    }

    private func chooseLocation() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.canCreateDirectories = true
        panel.prompt = "Use Folder"
        panel.message = "dino creates a folder here for each repo's worktrees."
        if panel.runModal() == .OK, let url = panel.url {
            location = (url.path as NSString).abbreviatingWithTildeInPath
            saveLocation()
        }
    }
}

/// Merged, ready, in progress; plus why it's held on to.
private struct StateChip: View {
    let worktree: StoredWorktree

    var body: some View {
        HStack(spacing: 4) {
            chip(label, color)
            if worktree.dirty, worktree.state != "in_progress" { chip("uncommitted", SessionStatus.needsYou.color) }
            if worktree.archived { chip("archived", .secondary) }
            switch worktree.session_state {
            case "working": chip("working", Brand.green)
            case "idle": chip("idle", Brand.green)
            case "ended": chip("ended", .secondary)
            default: if worktree.session != nil { chip("in use", Brand.green) }
            }
        }
    }

    private var label: String {
        switch worktree.state {
        case "merged": "merged"
        case "ready": "ready"
        case "empty": "no changes"
        default: worktree.dirty ? "uncommitted" : "in progress"
        }
    }

    private var color: Color {
        switch worktree.state {
        case "merged": .purple
        case "ready": Brand.green
        case "empty": .secondary
        default: SessionStatus.needsYou.color
        }
    }

    private func chip(_ text: String, _ color: Color) -> some View {
        Text(text)
            .font(.caption2.weight(.medium))
            .padding(.horizontal, 5).padding(.vertical, 1)
            .background(Capsule().fill(color.opacity(0.15)))
            .foregroundStyle(color)
    }
}
