import AppKit
import SwiftUI

// MARK: - Wire types (see crates/dino-core/src/ipc.rs)

/// A stopped session dinod keeps to start again: its conversation, folder and branch.
struct ArchivedInfo: Codable, Identifiable, Equatable {
    var id: String
    var name: String
    var label: String?
    var launcher: String
    var cwd: String
    var branch: String?
    var archived_at: UInt64
    /// The agent's conversation comes back, not just its folder.
    var resumable: Bool
    /// Its worktree was removed; starting it again brings it back from `branch`.
    var worktree_removed: Bool
    /// The agent and its conversation id, to show the conversation; nil from an older dinod.
    var agent: String?
    var agent_session: String?
    var pinned: Bool?

    var display: String { label ?? name }
}

/// A worktree dino made, for Settings → Workspaces → Worktrees.
struct StoredWorktree: Codable, Identifiable, Equatable {
    var path: String
    var repo: String
    var branch: String
    /// "in_progress", "ready", "merged" or "empty".
    var state: String
    var dirty: Bool
    /// Bytes on disk; nil until dinod has measured it.
    var size: UInt64?
    /// The live session running in it.
    var session: String?
    /// "working", "idle", or "ended" (removing the worktree archives it); nil with no session there.
    var session_state: String?
    var archived: Bool
    var fanout: Bool
    /// Free Up Space would remove it: nothing running in it, and its work merged or pushed.
    var reclaimable: Bool?
    var id: String { path }

    /// Nothing in it would be lost, and nothing is using it.
    var removable: Bool { !dirty && session == nil && !fanout }
}

private struct ArchivedResponse: Decodable { var sessions: [ArchivedInfo] }
private struct StorageResponse: Decodable { var worktrees: [StoredWorktree] }
/// What Free Up Space removed.
struct Freed: Decodable { var removed: [String]; var bytes: UInt64 }

extension DinoConnection {
    /// An empty name goes back to the agent's own title.
    func rename(_ id: String, to name: String) throws {
        _ = try send(["type": "rename", "id": id, "name": name])
    }

    func pin(_ id: String, _ pinned: Bool) throws {
        _ = try send(["type": "pin", "id": id, "pinned": pinned])
    }

    func archive(_ id: String) throws {
        _ = try send(["type": "archive", "id": id])
    }

    func archived() throws -> [ArchivedInfo] {
        try JSONDecoder().decode(ArchivedResponse.self, from: send(["type": "archived"])).sessions
    }

    /// Start it again; returns the new session's id.
    func unarchive(_ id: String) throws -> String? {
        try request(["type": "unarchive", "id": id]).id
    }

    func deleteArchived(_ id: String) throws {
        _ = try send(["type": "delete_archived", "id": id])
    }

    func storage() throws -> [StoredWorktree] {
        try JSONDecoder().decode(StorageResponse.self, from: send(["type": "storage"])).worktrees
    }

    /// Remove every worktree nothing would be lost from; dinod checks each again with git first.
    func freeUpSpace() throws -> Freed {
        try JSONDecoder().decode(Freed.self, from: send(["type": "free_up_space"]))
    }

    /// Never forced: dinod refuses one with uncommitted work or a session in it.
    func removeStored(_ path: String) throws {
        _ = try send(["type": "remove_stored", "path": path])
    }
}

extension SessionInfo {
    /// What the user named it, else dino's name for it.
    var display: String { label ?? name }
}

// MARK: - Model

extension DinoModel {
    func rename(_ id: String, to name: String) {
        renaming = nil
        let name = name.trimmingCharacters(in: .whitespacesAndNewlines)
        if let i = sessions.firstIndex(where: { $0.id == id }) {
            // Show it now; the next poll agrees.
            sessions[i].label = name.isEmpty ? nil : name
        }
        terminals[id]?.requestFocus()
        lifecycle { try $0.rename(id, to: name) }
    }

    func pin(_ id: String, _ pinned: Bool) {
        if let i = sessions.firstIndex(where: { $0.id == id }) {
            // Show it now; the next poll agrees.
            sessions[i].pinned = pinned
        }
        lifecycle { try $0.pin(id, pinned) }
    }

    func archive(_ id: String) {
        guard sessions.contains(where: { $0.id == id }) else { return }
        if selected == id {
            // Land on the neighbour rather than the top of the list.
            let order = sidebarOrder
            let i = order.firstIndex(of: id) ?? 0
            let next = order.indices.contains(i + 1) ? order[i + 1] : i > 0 ? order[i - 1] : nil
            select(next)
        }
        lifecycle { try $0.archive(id) }
    }

    /// A fan-out member goes with its fan-out: it's kept or discarded instead.
    func canArchive(_ id: String) -> Bool {
        sessions.contains { $0.id == id } && !groups.contains { $0.members.contains { $0.session == id } }
    }

    /// The sidebar's filter lives in the defaults (`@AppStorage("sidebar.filter")`).
    func showArchived() {
        UserDefaults.standard.set(SessionFilter.archived.rawValue, forKey: "sidebar.filter")
    }

    func unarchive(_ a: ArchivedInfo) {
        archived.removeAll { $0.id == a.id }
        // Back to every session, where the one coming back is selected.
        UserDefaults.standard.set(SessionFilter.all.rawValue, forKey: "sidebar.filter")
        lifecycle { conn in
            let id = try conn.unarchive(a.id)
            await MainActor.run { self.pendingSelect = id }
        }
    }

    func deleteArchived(_ a: ArchivedInfo) {
        archived.removeAll { $0.id == a.id }
        lifecycle { try $0.deleteArchived(a.id) }
    }

    /// Socket work off the main thread; the archive list is re-read after it, whatever happened.
    private func lifecycle(_ work: @escaping @Sendable (DinoConnection) async throws -> Void) {
        Task.detached {
            do {
                try await work(DinoConnection(path: DinoEnvironment.socketPath))
            } catch {
                await MainActor.run { self.error = error.localizedDescription }
            }
            await self.refreshArchived()
        }
    }

    nonisolated func refreshArchived() async {
        guard let list = try? DinoConnection(path: DinoEnvironment.socketPath).archived() else { return }
        await MainActor.run { if list != self.archived { self.archived = list } }
    }

    /// Sessions top to bottom as the sidebar lists them.
    var sidebarOrder: [String] {
        let tree = SessionTree.build(repos: repos, sessions: sidebarSessions, groups: groups)
        var ids: [String] = []
        for node in tree.repos {
            ids += node.places.flatMap { $0.sessions.map(\.id) }
            ids += node.groups.flatMap { $0.members.map(\.session) }
        }
        ids += tree.unfiled.map(\.id)
        let live = Set(sessions.map(\.id))
        ids = ids.filter { live.contains($0) }
        return ids + sidebarSessions.map(\.id).filter { !ids.contains($0) }
    }

    /// Ctrl+Tab: the next session down the sidebar, wrapping; `by: -1` goes up.
    func cycle(by step: Int) {
        let order = sidebarOrder
        guard !order.isEmpty else { return }
        let at = order.firstIndex(of: selected ?? "") ?? (step > 0 ? -1 : 0)
        select(order[((at + step) % order.count + order.count) % order.count])
    }
}

// MARK: - Renaming

/// A session's name that turns into a text field while it's being renamed.
struct SessionName: View {
    @EnvironmentObject var model: DinoModel
    let session: SessionInfo
    /// Which place is renaming: the sidebar row and the toolbar both show the name.
    let place: DinoModel.RenamePlace
    let font: Font
    var color: Color = .primary

    @State private var draft = ""
    @FocusState private var focused: Bool

    var body: some View {
        if model.renaming == Renaming(id: session.id, place: place) {
            TextField("Name", text: $draft, prompt: Text(session.name))
                .textFieldStyle(.plain)
                .font(font)
                .focused($focused)
                .onAppear {
                    draft = session.label ?? session.title ?? ""
                    DispatchQueue.main.async { focused = true }
                }
                .onSubmit { model.rename(session.id, to: draft) }
                .onExitCommand { model.renaming = nil; model.terminals[session.id]?.requestFocus() }
                .onChange(of: focused) { _, f in
                    // Clicking away keeps what was typed, like Finder.
                    if !f, model.renaming?.id == session.id { model.rename(session.id, to: draft) }
                }
                .help("Return to rename; empty goes back to the agent's own title")
        } else {
            // In the toolbar, a shell goes by its tab's name (its folder).
            Text(place == .toolbar ? model.tabName(session) : session.display)
                .font(font)
                .foregroundStyle(color)
                .lineLimit(1)
                // Simultaneous, so a single click still selects the row at once.
                .simultaneousGesture(TapGesture(count: 2).onEnded { model.renaming = Renaming(id: session.id, place: place) })
                .help(session.label == nil ? "Double-click to rename" : "\(session.name) · double-click to rename")
        }
    }
}

struct Renaming: Equatable {
    var id: String
    var place: DinoModel.RenamePlace
}

extension DinoModel {
    enum RenamePlace { case sidebar, toolbar }
}

// MARK: - Archived sessions

/// The sidebar under its Archived filter: every archived session, newest first, to find,
/// unarchive or delete.
struct ArchivedSection: View {
    @EnvironmentObject var model: DinoModel
    @State private var query = ""
    @State private var deleting: ArchivedInfo?

    private var shown: [ArchivedInfo] {
        let q = query.trimmingCharacters(in: .whitespaces)
        guard !q.isEmpty else { return model.archived }
        return model.archived.filter { a in
            [a.display, a.name, a.cwd, a.branch ?? "", model.launcherLabel(a.launcher)].contains { $0.localizedCaseInsensitiveContains(q) }
        }
    }

    var body: some View {
        Section {
            if model.archived.isEmpty {
                Text("Nothing archived. Archive a session from its row, its menu or ⇧⌘A: it stops, and waits here to pick up again.")
                    .font(.callout).foregroundStyle(.tertiary)
                    .lineLimit(4)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                TextField("Search archived", text: $query)
                    .textFieldStyle(.roundedBorder)
                    .controlSize(.small)
                ForEach(shown) { a in
                    ArchivedRow(session: a, delete: { deleting = a })
                        .contextMenu {
                            Button("Unarchive") { model.unarchive(a) }
                            Button("Show in Finder") { NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: a.cwd)]) }
                                .disabled(a.worktree_removed)
                            Divider()
                            Button("Delete…", role: .destructive) { deleting = a }
                        }
                }
                if shown.isEmpty {
                    Text("No archived session matches “\(query)”").font(.callout).foregroundStyle(.tertiary)
                }
            }
        }
        .alert(
            "Delete “\(deleting?.display ?? "")”?",
            isPresented: Binding(get: { deleting != nil }, set: { if !$0 { deleting = nil } }),
            presenting: deleting
        ) { a in
            Button("Delete", role: .destructive) { model.deleteArchived(a) }
            Button("Cancel", role: .cancel) {}
        } message: { a in
            Text(a.branch.map { "It leaves the archive for good. Its branch \($0) stays in the repo." } ?? "It leaves the archive for good.")
        }
    }
}

struct ArchivedRow: View {
    @EnvironmentObject var model: DinoModel
    let session: ArchivedInfo
    let delete: () -> Void
    @State private var hovering = false
    @State private var previewing = false

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "archivebox").foregroundStyle(.tertiary).frame(width: 14).accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 4) {
                    Text(session.display).font(.body.weight(.semibold)).foregroundStyle(.secondary).lineLimit(1)
                    if session.pinned == true {
                        Image(systemName: "pin.fill").font(.caption2).foregroundStyle(.tertiary).help("Pinned")
                            .accessibilityLabel("Pinned")
                    }
                }
                Text(detail).font(.caption).foregroundStyle(.tertiary).lineLimit(1)
            }
            Spacer(minLength: 4)
            if hovering {
                Button { model.unarchive(session) } label: { Image(systemName: "arrow.uturn.backward.circle") }
                    .buttonStyle(.plain)
                    .foregroundStyle(Brand.green)
                    .help(session.resumable ? "Unarchive: continue the conversation" : "Unarchive: start \(model.launcherLabel(session.launcher)) again in its folder")
                    .accessibilityLabel("Unarchive")
                Button(action: delete) { Image(systemName: "trash") }
                    .buttonStyle(.plain)
                    .foregroundStyle(.secondary)
                    .help("Delete…")
                    .accessibilityLabel("Delete")
            }
        }
        .padding(.vertical, 2)
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
        // A click shows what it was about; a double-click picks it up again.
        .gesture(TapGesture(count: 2).onEnded { model.unarchive(session) }.exclusively(before: TapGesture().onEnded { previewing = true }))
        .popover(isPresented: $previewing, arrowEdge: .trailing) {
            ArchivedPreview(session: session, delete: {
                previewing = false
                delete()
            })
            .environmentObject(model)
        }
    }

    private var detail: String {
        // Most telling first, so a narrow sidebar cuts the branch rather than when.
        [ago(session.archived_at), model.launcherLabel(session.launcher), session.branch ?? shortPath(session.cwd)]
            .filter { !$0.isEmpty }.joined(separator: " · ")
    }
}

/// An archived session's conversation, read from its transcript, with the way back.
struct ArchivedPreview: View {
    @EnvironmentObject var model: DinoModel
    let session: ArchivedInfo
    let delete: () -> Void
    @State private var page: ConversationPage?
    @State private var failed: String?

    var body: some View {
        VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 2) {
                Text(session.display).font(.headline).lineLimit(1)
                Text("\(model.launcherLabel(session.launcher)) · \(session.branch ?? shortPath(session.cwd))")
                    .font(.caption).foregroundStyle(.secondary).lineLimit(1)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(12)
            Divider()
            if let agent = session.agent, let id = session.agent_session {
                ConversationView(page: page, agent: agent, loading: page == nil && failed == nil, unreadable: failed ?? "This conversation can’t be read.") {
                    try? await ConversationPage.fetch(agent: agent, id: id, before: $0).get()
                }
                .task {
                    switch await ConversationPage.fetch(agent: agent, id: id) {
                    case .success(let p): page = p
                    case .failure(let e): failed = e.localizedDescription
                    }
                }
            } else {
                Text("No conversation was kept for this session. Unarchiving starts \(model.launcherLabel(session.launcher)) again in its folder.")
                    .foregroundStyle(.secondary).multilineTextAlignment(.center)
                    .padding(24).frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            Divider()
            HStack {
                Button("Delete…", role: .destructive, action: delete)
                Spacer()
                Button(session.resumable ? "Continue" : "Unarchive") { model.unarchive(session) }
                    .keyboardShortcut(.defaultAction)
            }
            .padding(12)
        }
        .frame(width: 440, height: 520)
    }
}

// MARK: - Keyboard shortcuts

/// ⌘/: every menu command with a shortcut, read from the menu bar so it's never out of date.
struct ShortcutSheet: View {
    @Environment(\.dismiss) private var dismiss

    struct Entry: Identifiable {
        var title: String
        var keys: String
        var id: String { title + keys }
    }

    struct MenuGroup: Identifiable {
        var menu: String
        var entries: [Entry]
        var id: String { menu }
    }

    /// Keys dino handles outside the menus.
    private static let extra = MenuGroup(menu: "Sidebar", entries: [
        Entry(title: "Rename a session", keys: "double-click"),
        Entry(title: "Unarchive a session", keys: "double-click"),
    ])

    /// The shell's AI line (`dino init`), in a shell at its prompt.
    private static let shell = MenuGroup(menu: "In a shell", entries: [
        Entry(title: "Ask your agent for a command", keys: "⌘I"),
        Entry(title: "Hand the line to an agent", keys: "⌘↩"),
        Entry(title: "Search history and sessions", keys: "⌥R"),
    ])

    /// Claude desktop's keys for what the menus list under dino's own (see `AppDelegate.desktopKey`).
    private static let desktop = MenuGroup(menu: "Also, as in Claude desktop", entries: [
        Entry(title: "Next Session", keys: "⇧⌘]"),
        Entry(title: "Previous Session", keys: "⇧⌘["),
        Entry(title: "Show or Hide Preview", keys: "⇧⌘B"),
        Entry(title: "Ask About This Session…", keys: "⌘;"),
    ])

    private var groups: [MenuGroup] {
        let menus = NSApp.mainMenu?.items.compactMap { item -> MenuGroup? in
            guard let menu = item.submenu else { return nil }
            let entries = Self.entries(in: menu, prefix: "")
            return entries.isEmpty ? nil : MenuGroup(menu: item.title, entries: entries)
        } ?? []
        return menus + [Self.extra, Self.shell, Self.desktop]
    }

    private static func entries(in menu: NSMenu, prefix: String) -> [Entry] {
        menu.items.flatMap { item -> [Entry] in
            if let sub = item.submenu { return entries(in: sub, prefix: prefix + item.title + " › ") }
            guard !item.isHidden, !item.isSeparatorItem, !item.keyEquivalent.isEmpty else { return [] }
            // What macOS adds to Edit (dictation, emoji) comes several times over, some on the fn key.
            if let action = item.action, systemActions.contains(action) { return [] }
            return [Entry(title: prefix + item.title, keys: keys(item))]
        }
    }

    static let systemActions: Set<Selector> = [
        NSSelectorFromString("startDictation:"), #selector(NSApplication.orderFrontCharacterPalette(_:)),
    ]

    static func keys(_ item: NSMenuItem) -> String {
        let m = item.keyEquivalentModifierMask
        var s = ""
        if m.contains(.control) { s += "⌃" }
        if m.contains(.option) { s += "⌥" }
        var key = item.keyEquivalent
        // AppKit stores ⇧ as an uppercase letter.
        if m.contains(.shift) || (key.count == 1 && key != key.lowercased()) { s += "⇧" }
        if m.contains(.command) { s += "⌘" }
        switch key {
        case "\u{8}", "\u{7f}": key = "⌫"
        case "\t", "\u{19}": key = "⇥"
        case "\r": key = "↩"
        case "\u{1b}": key = "⎋"
        case " ": key = "Space"
        default: key = key.uppercased()
        }
        return s + key
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text("Keyboard Shortcuts").font(.headline)
                Spacer()
                Button("Done") { dismiss() }.keyboardShortcut(.cancelAction)
            }
            .padding(14)
            Divider()
            ScrollView {
                LazyVGrid(columns: [GridItem(.flexible(), alignment: .top), GridItem(.flexible(), alignment: .top)], alignment: .leading, spacing: 18) {
                    ForEach(groups) { g in
                        VStack(alignment: .leading, spacing: 6) {
                            Text(g.menu.uppercased()).font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                            ForEach(g.entries) { e in
                                HStack {
                                    Text(e.title).lineLimit(1)
                                    Spacer(minLength: 8)
                                    Text(e.keys).font(.body.monospaced()).foregroundStyle(.secondary)
                                }
                                .font(.callout)
                            }
                        }
                    }
                }
                .padding(16)
            }
        }
        .sheetSize(width: 640, height: 480)
    }
}
