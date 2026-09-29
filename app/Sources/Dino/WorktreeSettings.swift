import AppKit
import SwiftUI

extension DinoSettings {
    /// Where dino's worktrees go and what their branches are called; nil from an older dinod.
    struct Worktrees: Codable, Equatable {
        /// Relative: inside each repo. Absolute or `~/…`: a folder per repo under it.
        var location: String
        var branch_prefix: String

        static let defaultLocation = ".dino/worktrees"
        static let defaultPrefix = "dino/"
    }
}

/// Settings → Worktrees: where they go, and the ones on disk.
struct WorktreesPane: View {
    @EnvironmentObject var store: SettingsStore
    @State private var location = ""
    @State private var prefix = ""
    @State private var stored: [StoredWorktree]?
    @State private var removing: Set<String> = []
    @State private var error: String?
    @State private var confirmAll = false

    private var current: DinoSettings.Worktrees? { store.settings?.worktrees }
    private var merged: [StoredWorktree] { (stored ?? []).filter { $0.state == "merged" && $0.removable && !removing.contains($0.path) } }

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
                Footnote("A relative location is inside each repo, and dino keeps it out of git status. An absolute one gets a folder per repo. Both apply to worktrees dino makes from now on: sessions, fan-outs and scheduled tasks. Branches are named like \(prefixShown)claude-3f2a.")
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
                    Button("Remove All Merged…") { confirmAll = true }
                        .disabled(merged.isEmpty)
                        .help("Remove every worktree whose work is merged, has no uncommitted changes, and has no session running in it")
                }
            } footer: {
                Footnote("Only worktrees dino made. dino never removes one with uncommitted changes. An archived session whose worktree is removed gets it back from its branch when you unarchive it.")
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
        .confirmationDialog("Remove \(merged.count) merged worktree\(merged.count == 1 ? "" : "s")?", isPresented: $confirmAll) {
            Button("Remove \(merged.count)", role: .destructive) { remove(merged) }
        } message: {
            Text("Their work is merged, so nothing is lost. Branches git sees as merged go too.")
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
            if worktree.session != nil { chip("in use", Brand.green) }
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
