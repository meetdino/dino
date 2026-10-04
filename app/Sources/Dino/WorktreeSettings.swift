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

/// Settings → Workspaces → Worktrees: where they go, trust for fan-outs, archiving after a PR,
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
                Footnote("A relative location is inside each repo, and dino keeps it out of git status. An absolute one gets a folder per repo. Both apply to worktrees dino makes from now on: sessions, fan-outs and automations. Branches are named like \(prefixShown)claude-3f2a, or after the automation for its runs.")
            }
            Section {
                Toggle("Trust fan-out worktrees when the repo is trusted", isOn: Binding(
                    get: { store.settings?.policies.worktree_trust ?? true },
                    set: { on in store.update { $0.policies.worktree_trust = on } }
                ))
                .orgLocked("policies.worktree_trust")
            } header: {
                Text("Fan-out")
            } footer: {
                Footnote("Claude asks whether to trust each new folder, and every fan-out worktree is one. When you've trusted the repo, dino tells Claude its worktrees are trusted too, and forgets them when the fan-out closes. Codex does this on its own.")
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
                Footnote("When a session's PR merges or is closed, dino archives it once its agent is idle, so the conversation can be picked up again. After a merge, the worktree dino made for it is removed too if nothing in it would be lost; after a close it stays (see Storage below), since the work never landed. Unarchive it to pick up where it left off, worktree and all. Sessions outside a dino worktree stay open.")
            }
            Section {
                if let stored {
                    if stored.isEmpty {
                        Text("dino hasn't made any worktrees that are still on disk.").foregroundStyle(.secondary)
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
                            .help("Remove every worktree nothing would be lost from: no session running in it, no uncommitted changes, and its commits merged or pushed")
                    }
                }
            } footer: {
                VStack(alignment: .leading, spacing: 4) {
                    if let freed { Text(freed).font(.callout).foregroundStyle(.secondary) }
                    Footnote("Only worktrees dino made. dino never removes one with uncommitted changes, and keeps each branch that isn't merged. A session that ended or was archived gets its worktree back from its branch when you start it again.")
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
            Text("Nothing runs in them, and their work is merged or pushed, so nothing is lost. Sessions that ended in them are archived. Branches git sees as merged go too.")
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
        if w.fanout { return "It belongs to a fan-out: keep or discard the fan-out instead" }
        if let s = w.session { return "\(s) is running in it" }
        if w.session_state == "ended" { return "Remove it; the session that ended in it is archived, and gets it back from \(w.branch) when you start it again" }
        if w.dirty { return "It has uncommitted changes, so dino won't remove it" }
        return w.archived
            ? "Remove it; its archived session gets it back from \(w.branch) when you unarchive it"
            : "Remove it, and its branch if git sees it merged"
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
                    ? "Nothing to remove: every worktree left has work in it or a session running."
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
        panel.message = "dino puts each repo's worktrees in a folder of its own here."
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
            if worktree.fanout { chip("fan-out", .blue) }
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
