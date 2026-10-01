import SwiftUI

/// A checkout of a repo (see crates/dino-core/src/worktree.rs).
struct Worktree: Codable, Equatable, Identifiable {
    var path: String
    var branch: String?
    /// dino made it for a session (see `ClosingWorktree`).
    var dino: Bool
    /// What's in it next to the main checkout; nil for the main checkout.
    var git: WorktreeGit?
    /// The subagent that made it, when its agent's hooks said so.
    var owner: WorktreeOwner?
    var id: String { path }
}

struct WorktreeGit: Codable, Equatable {
    /// The latest commit subject not on the main branch, else a readable branch name.
    var label: String
    var added: UInt32
    var removed: UInt32
    /// Uncommitted or new files.
    var dirty: Bool
    var ahead: UInt32
    /// "in_progress", "ready", "merged" or "empty".
    var state: String
}

struct WorktreeOwner: Codable, Equatable {
    var session: String
    var description: String?
    var agentType: String?
    var running: Bool

    enum CodingKeys: String, CodingKey {
        case session, description, running
        case agentType = "agent_type"
    }
}

struct ClosingWorktree: Equatable {
    var path: String
    var label: String
    /// Bring its changes into the checkout it came from first.
    var apply: Bool
}

/// A git repo with its worktrees, or a plain folder (no worktrees) where sessions run.
struct RepoInfo: Codable, Equatable, Identifiable {
    var path: String
    var name: String
    var worktrees: [Worktree]
    var id: String { path }
}

struct TreeResponse: Decodable {
    var repos: [RepoInfo]
}

extension DinoConnection {
    func tree(folders: [String]) throws -> [RepoInfo] {
        try JSONDecoder().decode(TreeResponse.self, from: send(["type": "tree", "folders": folders])).repos
    }
}

// MARK: - Filing sessions into repos and worktrees

/// One place sessions can live: a worktree, or a plain folder.
struct PlaceNode: Identifiable, Equatable {
    var path: String
    var label: String
    var sessions: [SessionInfo]
    /// A worktree dino made for a session, for the user to close.
    var dino = false
    var git: WorktreeGit?
    var owner: WorktreeOwner?
    var id: String { path }
    /// Its identity as a sidebar row: a plain row and one that opens to sessions aren't the same row.
    var rowID: String { sessions.isEmpty ? path : path + "#open" }

    /// Nothing in it would be lost by removing it: no uncommitted work, nobody in it.
    var cleanable: Bool {
        guard let git, !dino, sessions.isEmpty, owner?.running != true else { return false }
        return !git.dirty
    }
}

struct RepoNode: Identifiable, Equatable {
    var repo: RepoInfo
    /// The main checkout first, then other worktrees; fan-out worktrees are in `groups` instead.
    var places: [PlaceNode]
    var groups: [GroupInfo]
    /// Session id → the worktrees its subagents made, shown under that session.
    var subagents: [String: [PlaceNode]] = [:]
    /// Worktrees nobody here made and nobody works in.
    var others: [PlaceNode] = []
    /// Worktrees whose work landed on the main branch, ready to clean up.
    var merged: [PlaceNode] = []
    var id: String { repo.path }
    var isGit: Bool { !repo.worktrees.isEmpty }
    /// One checkout and no fan-outs: list its sessions right under the repo.
    var flat: Bool { places.count == 1 && groups.isEmpty && others.isEmpty && merged.isEmpty }
    var sessionCount: Int { places.reduce(0) { $0 + $1.sessions.count } }
}

enum SessionTree {
    /// Each session goes under the deepest worktree or folder containing its cwd; fan-out
    /// members go under their group. Sessions the tree doesn't cover yet come back as `unfiled`.
    /// Worktrees a session's subagents made go under that session; merged ones and ones nobody
    /// here made fold away.
    static func build(repos: [RepoInfo], sessions: [SessionInfo], groups: [GroupInfo]) -> (repos: [RepoNode], unfiled: [SessionInfo]) {
        // Pinned ones first wherever they land, the rest in dinod's order.
        let sessions = sessions.filter { $0.pinned == true } + sessions.filter { $0.pinned != true }
        let inGroup = Set(groups.flatMap { $0.members.map(\.session) })
        let groupWorktrees = Set(groups.flatMap { $0.members.map(\.worktree) })
        var nodes = repos.map { r in
            let places = r.worktrees.isEmpty
                ? [PlaceNode(path: r.path, label: r.name, sessions: [])]
                : r.worktrees.filter { !groupWorktrees.contains($0.path) }.map {
                    PlaceNode(
                        path: $0.path,
                        label: $0.git?.label ?? $0.branch ?? URL(fileURLWithPath: $0.path).lastPathComponent,
                        sessions: [], dino: $0.dino, git: $0.git, owner: $0.owner
                    )
                }
            return RepoNode(repo: r, places: places, groups: groups.filter { $0.repo == r.path })
        }
        var unfiled: [SessionInfo] = []
        // A session on an SSH host is in a folder there, never in one of these.
        for s in sessions where !inGroup.contains(s.id) {
            if s.host != nil {
                unfiled.append(s)
                continue
            }
            var best: (repo: Int, place: Int, depth: Int)?
            for (ri, node) in nodes.enumerated() {
                for (pi, place) in node.places.enumerated() where contains(place.path, s.cwd ?? "") {
                    if place.path.count > (best?.depth ?? -1) { best = (ri, pi, place.path.count) }
                }
            }
            if let best { nodes[best.repo].places[best.place].sessions.append(s) } else { unfiled.append(s) }
        }
        let ids = Set(sessions.map(\.id))
        for i in nodes.indices {
            var kept: [PlaceNode] = []
            for (pi, place) in nodes[i].places.enumerated() {
                // The main checkout, dino's own and ones with sessions in them stay where they are.
                if pi == 0 || place.dino || !place.sessions.isEmpty || place.git == nil {
                    kept.append(place)
                } else if let owner = place.owner, ids.contains(owner.session) {
                    nodes[i].subagents[owner.session, default: []].append(place)
                } else if place.git?.state == "merged", place.cleanable {
                    nodes[i].merged.append(place)
                } else if place.owner != nil {
                    // Its session is gone: back under the repo.
                    kept.append(place)
                } else {
                    nodes[i].others.append(place)
                }
            }
            nodes[i].places = kept
        }
        return (nodes, unfiled)
    }

    /// The tree with only the sessions `keep` passes: places, fan-outs and repos left empty go too.
    static func build(repos: [RepoInfo], sessions: [SessionInfo], groups: [GroupInfo], keep: (SessionInfo) -> Bool) -> (repos: [RepoNode], unfiled: [SessionInfo]) {
        let kept = sessions.filter(keep)
        let ids = Set(kept.map(\.id))
        var tree = build(repos: repos, sessions: kept, groups: groups)
        tree.repos = tree.repos.compactMap { node in
            var node = node
            node.places.removeAll { $0.sessions.isEmpty }
            node.groups = node.groups.filter { $0.members.contains { ids.contains($0.session) } }
            node.others = []
            node.merged = []
            return node.places.isEmpty && node.groups.isEmpty ? nil : node
        }
        return tree
    }

    static func contains(_ dir: String, _ path: String) -> Bool {
        path == dir || path.hasPrefix(dir.hasSuffix("/") ? dir : dir + "/")
    }
}

// MARK: - Filtering by status

/// The sidebar's status filter.
enum SessionFilter: String, CaseIterable, Identifiable {
    case all, needsYou, working, idle, archived
    var id: String { rawValue }

    var label: String {
        switch self {
        case .all: "All"
        case .needsYou: "Needs you"
        case .working: "Working"
        case .idle: "Idle"
        case .archived: "Archived"
        }
    }

    var help: String {
        switch self {
        case .all: "Every session"
        case .needsYou: "Waiting on you: asking for something, or finished and not looked at yet"
        case .working: "Thinking, running tools, or waiting on its subagents and background commands"
        case .idle: "Waiting for a prompt, or exited"
        case .archived: "Archived sessions: stopped and kept, to pick up again (⇧⌘A archives the current one)"
        }
    }

    /// "Needs you" includes finished-unseen: both wait on you.
    func passes(_ status: SessionStatus) -> Bool {
        switch self {
        case .all: true
        case .needsYou: status == .needsYou || status == .done
        case .working: status == .thinking || status == .working || status == .waiting
        case .idle: status == .idle || status == .ended || status == .exited
        // Live sessions are never archived; the archive is listed on its own.
        case .archived: false
        }
    }

    var color: Color {
        switch self {
        case .all: .secondary
        case .needsYou: SessionStatus.needsYou.color
        case .working: SessionStatus.working.color
        case .idle, .archived: .secondary
        }
    }
}

/// All · Needs you · Working · Idle, with counts; one click each.
struct FilterBar: View {
    @EnvironmentObject var model: DinoModel
    @Binding var filter: SessionFilter

    var body: some View {
        HStack(spacing: 4) {
            ForEach(SessionFilter.allCases.filter { $0 != .archived }) { f in
                let count = model.sessions.filter { f.passes(model.status(of: $0)) && model.sidebarShows($0) }.count
                let on = f == filter
                Button { filter = f } label: {
                    HStack(spacing: 3) {
                        Text(f.label).lineLimit(1)
                        Text("\(count)").monospacedDigit()
                            .foregroundStyle(on ? AnyShapeStyle(.primary) : count > 0 && f != .all ? AnyShapeStyle(f.color) : AnyShapeStyle(.tertiary))
                    }
                    .font(.caption.weight(on ? .semibold : .regular))
                    .padding(.horizontal, 6).padding(.vertical, 3)
                    .background(Capsule().fill(on ? Color.primary.opacity(0.1) : .clear))
                    .contentShape(Capsule())
                }
                .buttonStyle(.plain)
                .help(f.help)
            }
            Spacer(minLength: 0)
        }
    }
}

/// The way into the archive, beside the dino mark: the filters below keep their room.
struct ArchiveToggle: View {
    @EnvironmentObject var model: DinoModel
    @Binding var filter: SessionFilter

    var body: some View {
        let on = filter == .archived
        Button { filter = on ? .all : .archived } label: {
            HStack(spacing: 3) {
                Image(systemName: on ? "archivebox.fill" : "archivebox")
                if !model.archived.isEmpty {
                    Text("\(model.archived.count)").monospacedDigit()
                }
            }
            .font(.caption.weight(on ? .semibold : .regular))
            .foregroundStyle(on ? AnyShapeStyle(.primary) : AnyShapeStyle(.secondary))
            .padding(.horizontal, 6).padding(.vertical, 3)
            .background(Capsule().fill(on ? Color.primary.opacity(0.1) : .clear))
            .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .help(on ? "Back to every session" : SessionFilter.archived.help)
    }
}

// MARK: - Searching and narrowing

/// One project or host the sidebar can be narrowed to.
enum SidebarScope: Hashable {
    case repo(String)
    case host(String)
    case thisMac
}

extension DinoModel {
    /// A search or a scope hides some sessions.
    var sidebarNarrowed: Bool { !sidebarQuery.trimmingCharacters(in: .whitespaces).isEmpty || sidebarScope != nil }

    /// In the sidebar's scope, and its name, title, branch, folder or agent has the search in it.
    func sidebarShows(_ s: SessionInfo) -> Bool {
        let cwd = s.cwd ?? ""
        switch sidebarScope {
        case nil: break
        case .thisMac: if s.host != nil { return false }
        case .host(let h): if s.host != h { return false }
        case .repo(let path):
            guard s.host == nil, let r = repos.first(where: { $0.path == path }) else { return false }
            if !SessionTree.contains(r.path, cwd), !r.worktrees.contains(where: { SessionTree.contains($0.path, cwd) }) { return false }
        }
        let q = sidebarQuery.trimmingCharacters(in: .whitespaces)
        guard !q.isEmpty else { return true }
        let branch = s.host == nil
            ? repos.flatMap(\.worktrees).filter { SessionTree.contains($0.path, cwd) }.max { $0.path.count < $1.path.count }?.branch
            : nil
        return [s.display, s.name, s.title ?? "", branch ?? "", cwd, s.agent_id, launchers.first { $0.agent_id == s.agent_id }?.label ?? "", s.host ?? "", s.inside?.title ?? ""]
            .contains { $0.localizedCaseInsensitiveContains(q) }
    }

    /// Esc in the search: show everything again and go back to typing in the session.
    func endFinding() {
        sidebarQuery = ""
        findingSessions = false
        if let id = selected { terminals[id]?.requestFocus() }
    }
}

/// ⇧⌘F: narrows the sidebar as you type; Return opens the first match, Esc clears.
struct SessionSearchField: View {
    @EnvironmentObject var model: DinoModel
    @FocusState private var focused: Bool

    var body: some View {
        HStack(spacing: 4) {
            Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
            TextField("Find sessions", text: $model.sidebarQuery)
                .textFieldStyle(.plain)
                .focused($focused)
                .onSubmit {
                    // Open the first match and put the search away, like Spotlight.
                    guard let first = model.sidebarOrder.first(where: { id in model.sessions.first { $0.id == id }.map(model.sidebarShows) ?? false })
                    else { return }
                    model.sidebarQuery = ""
                    model.findingSessions = false
                    model.select(first)
                }
                .onExitCommand { model.endFinding() }
            if !model.sidebarQuery.isEmpty {
                Button { model.sidebarQuery = "" } label: { Image(systemName: "xmark.circle.fill") }
                    .buttonStyle(.plain)
                    .foregroundStyle(.tertiary)
                    .help("Clear")
            }
        }
        .font(.callout)
        .padding(.horizontal, 6).padding(.vertical, 4)
        .background(RoundedRectangle(cornerRadius: 6).fill(Color.primary.opacity(0.06)))
        .onAppear { focused = model.findingSessions }
        .onChange(of: model.findingSessions) { _, on in if on { focused = true } }
        .onChange(of: focused) { _, f in
            // Clicking away from an empty search puts it away.
            if !f, model.sidebarQuery.isEmpty { model.findingSessions = false }
        }
    }
}

/// Narrow the sidebar to one project, this Mac, or one SSH host.
struct ScopeMenu: View {
    @EnvironmentObject var model: DinoModel

    var body: some View {
        let hosts = Set(model.sessions.compactMap(\.host)).sorted()
        let on = model.sidebarScope != nil
        Menu {
            Picker("Show", selection: $model.sidebarScope) {
                Text("Everywhere").tag(SidebarScope?.none)
                if !hosts.isEmpty {
                    Text("This Mac").tag(SidebarScope?.some(.thisMac))
                }
            }
            .pickerStyle(.inline)
            if !model.repos.isEmpty {
                Picker("Project", selection: $model.sidebarScope) {
                    ForEach(model.repos) { r in
                        Text(r.name).tag(SidebarScope?.some(.repo(r.path)))
                    }
                }
                .pickerStyle(.inline)
            }
            if !hosts.isEmpty {
                Picker("Host", selection: $model.sidebarScope) {
                    ForEach(hosts, id: \.self) { h in
                        Text(h).tag(SidebarScope?.some(.host(h)))
                    }
                }
                .pickerStyle(.inline)
            }
        } label: {
            Image(systemName: on ? "line.3.horizontal.decrease.circle.fill" : "line.3.horizontal.decrease.circle")
                .foregroundStyle(on ? AnyShapeStyle(.primary) : AnyShapeStyle(.secondary))
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .help(model.sidebarScopeName.map { "Showing \($0)" } ?? "Show one project or host")
    }
}

/// What the sidebar is narrowed to, with a way back to everything.
struct ScopeChip: View {
    @EnvironmentObject var model: DinoModel

    var body: some View {
        if let name = model.sidebarScopeName {
            Button { model.sidebarScope = nil } label: {
                HStack(spacing: 4) {
                    Image(systemName: "line.3.horizontal.decrease")
                    Text(name).lineLimit(1)
                    Image(systemName: "xmark").foregroundStyle(.secondary)
                }
                .font(.caption)
                .padding(.horizontal, 7).padding(.vertical, 3)
                .background(Capsule().fill(Color.primary.opacity(0.1)))
                .contentShape(Capsule())
            }
            .buttonStyle(.plain)
            .help("Show every session again")
        }
    }
}

extension DinoModel {
    var sidebarScopeName: String? {
        switch sidebarScope {
        case nil: nil
        case .thisMac: "This Mac"
        case .host(let h): h
        case .repo(let p): repos.first { $0.path == p }?.name ?? URL(fileURLWithPath: p).lastPathComponent
        }
    }
}

// MARK: - Sidebar rows

/// Rows for one repo or folder: a disclosure row you can select to start work there.
struct RepoRows: View {
    @EnvironmentObject var model: DinoModel
    let node: RepoNode
    var filter = SessionFilter.all
    @Binding var collapsed: Set<String>

    var body: some View {
        DisclosureGroup(isExpanded: expanded(node.id)) {
            if node.flat {
                sessionRows(node.places[0].sessions)
            } else {
                // A folder that gains or loses sessions changes kind (a row, or one that opens):
                // a new identity then, so the list replaces the row instead of morphing it.
                ForEach(Array(node.places.enumerated()), id: \.element.rowID) { i, place in
                    Group {
                        if place.sessions.isEmpty {
                            placeRow(place, main: i == 0)
                        } else {
                            DisclosureGroup(isExpanded: expanded(place.id)) {
                                sessionRows(place.sessions)
                            } label: {
                                placeRow(place, main: i == 0)
                            }
                        }
                    }
                    .tag("dir:\(place.path)")
                    .contextMenu { placeMenu(place) }
                }
                ForEach(node.groups) { g in
                    DisclosureGroup(isExpanded: expanded("group:\(g.id)")) {
                        ForEach(g.members) { m in
                            if let s = model.sessions.first(where: { $0.id == m.session }), filter.passes(model.status(of: s)) {
                                SessionRow(session: s, index: 0, stat: m.stat).tag(s.id)
                                    .contextMenu { SessionMenu(session: s) }
                            }
                        }
                    } label: {
                        GroupRow(group: g)
                    }
                    .tag("group:\(g.id)")
                }
                if !node.merged.isEmpty {
                    DisclosureGroup(isExpanded: opened("merged:\(node.id)")) {
                        worktreeRows(node.merged)
                    } label: {
                        PlaceRow(icon: "checkmark.circle", title: "Merged", detail: "\(node.merged.count)")
                    }
                    .tag("merged:\(node.id)")
                    .contextMenu {
                        Button("Clean Up \(node.merged.count == 1 ? "Worktree" : "All \(node.merged.count) Worktrees")") {
                            model.cleanWorktrees(node.merged.map(\.path))
                        }
                    }
                }
                if !node.others.isEmpty {
                    DisclosureGroup(isExpanded: opened("others:\(node.id)")) {
                        worktreeRows(node.others)
                    } label: {
                        PlaceRow(icon: "square.stack.3d.up", title: "Other worktrees", detail: "\(node.others.count)")
                    }
                    .tag("others:\(node.id)")
                }
            }
        } label: {
            PlaceRow(
                icon: node.isGit ? "shippingbox" : "folder",
                title: node.repo.name,
                detail: node.flat && node.isGit ? node.places[0].label : nil
            )
        }
        // Not "dir:": its main checkout's row has that tag, and two rows with one tag confuse the list.
        .tag("repo:\(node.repo.path)")
        .contextMenu {
            Button("Copy Path") {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(node.repo.path, forType: .string)
            }
            Button("Show in Finder") { NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: node.repo.path) }
        }
    }

    @ViewBuilder
    private func placeRow(_ place: PlaceNode, main: Bool) -> some View {
        if main || place.git == nil {
            PlaceRow(icon: "arrow.triangle.branch", title: place.label, detail: nil)
        } else {
            WorktreeRow(place: place)
        }
    }

    @ViewBuilder
    private func placeMenu(_ place: PlaceNode) -> some View {
        WorktreeClosingItems(place: place)
        Button("Show in Finder") { NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: place.path) }
    }

    private func worktreeRows(_ places: [PlaceNode]) -> some View {
        ForEach(places) { place in
            WorktreeRow(place: place)
                .tag("dir:\(place.path)")
                .contextMenu { placeMenu(place) }
        }
    }

    /// A session's rows, each with the worktrees its subagents made under it.
    private func sessionRows(_ sessions: [SessionInfo]) -> some View {
        // With or without subagents' worktrees under it, as for folders.
        ForEach(sessions.map { (key: node.subagents[$0.id] == nil ? $0.id : "\($0.id)#sub", session: $0) }, id: \.key) { row in
            let s = row.session
            if let children = node.subagents[s.id] {
                DisclosureGroup(isExpanded: expanded("subagents:\(s.id)")) {
                    worktreeRows(children)
                } label: {
                    SessionRow(session: s, index: 0)
                }
                .tag(s.id)
                .contextMenu { SessionMenu(session: s) }
            } else {
                SessionRow(session: s, index: 0)
                    .tag(s.id)
                    .contextMenu { SessionMenu(session: s) }
            }
        }
    }

    /// Nodes start open; the sidebar remembers the ones you close.
    private func expanded(_ id: String) -> Binding<Bool> {
        Binding(
            get: { !collapsed.contains(id) },
            set: { open in if open { collapsed.remove(id) } else { collapsed.insert(id) } }
        )
    }

    /// Nodes that start closed; the sidebar remembers the ones you open.
    private func opened(_ id: String) -> Binding<Bool> {
        let key = "open:\(id)"
        return Binding(
            get: { collapsed.contains(key) },
            set: { open in if open { collapsed.insert(key) } else { collapsed.remove(key) } }
        )
    }
}

extension SessionInfo {
    /// Why features that need the session's folder on this Mac are off, for a session on an SSH host.
    var remoteReason: String? { host.map { "Not available for sessions on \($0): the folder is there, not on this Mac" } }
}

/// The SSH host a session runs on, beside its name in the toolbar.
struct HostChip: View {
    let host: String

    var body: some View {
        Label(host, systemImage: "server.rack")
            .labelStyle(.titleAndIcon)
            .font(.caption.weight(.medium))
            .foregroundStyle(.secondary)
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(.quaternary, in: Capsule())
            .help("Running on \(host) over SSH")
    }
}

/// The sessions on one SSH host, under the host's name.
struct HostRows: View {
    let host: String
    let sessions: [SessionInfo]
    @Binding var collapsed: Set<String>

    var body: some View {
        DisclosureGroup(isExpanded: Binding(
            get: { !collapsed.contains("host:\(host)") },
            set: { open in if open { collapsed.remove("host:\(host)") } else { collapsed.insert("host:\(host)") } }
        )) {
            ForEach(sessions) { s in
                SessionRow(session: s, index: 0)
                    .tag(s.id)
                    .contextMenu { SessionMenu(session: s) }
            }
        } label: {
            PlaceRow(icon: "server.rack", title: host, detail: "SSH")
                .help("Sessions running on \(host) over SSH")
        }
    }
}

struct PlaceRow: View {
    let icon: String
    let title: String
    let detail: String?

    var body: some View {
        HStack(spacing: 6) {
            // Not a Label: sidebar rows tint Label icons with the accent color.
            Image(systemName: icon).foregroundStyle(.secondary).frame(width: 16)
            Text(title).fontWeight(.medium).lineLimit(1)
            if let detail {
                Text(detail).font(.caption).foregroundStyle(.secondary).lineLimit(1)
            }
        }
    }
}

/// A worktree at a glance: what it's for, how much changed, and where it stands.
/// "● Review code button   +312 −20   running"
struct WorktreeRow: View {
    let place: PlaceNode

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: "arrow.triangle.branch").foregroundStyle(.secondary).frame(width: 16)
            Text(place.owner?.description ?? place.label).lineLimit(1).truncationMode(.tail).layoutPriority(1)
            Spacer(minLength: 4)
            if let git = place.git {
                if git.added + git.removed > 0 {
                    HStack(spacing: 3) {
                        if git.added > 0 { Text("+\(git.added)").foregroundStyle(.green) }
                        if git.removed > 0 { Text("−\(git.removed)").foregroundStyle(.red) }
                    }
                    .font(.caption.monospacedDigit())
                    .fixedSize()
                }
                if git.dirty {
                    Circle().fill(Color.orange).frame(width: 6, height: 6).help("In progress: uncommitted changes")
                }
            }
            if place.owner?.running == true {
                ProgressView().controlSize(.mini).help("Running")
            } else if let (icon, tip) = statusIcon {
                Image(systemName: icon).font(.caption).foregroundStyle(.secondary).help(tip)
            }
        }
        .help(help)
    }

    /// Icons, not words: the label needs the room in a narrow sidebar. The dirty dot already says "in progress".
    private var statusIcon: (String, String)? {
        switch place.git?.state {
        case "ready": return ("checkmark.circle", "Ready: committed, not on the main branch yet")
        case "merged": return ("arrow.triangle.merge", "Merged into the main branch")
        case "empty": return place.owner == nil ? ("circle.dashed", "Empty: nothing changed") : ("checkmark", "Done: nothing changed")
        default: return nil
        }
    }

    private var help: String {
        var lines = [place.path]
        if let d = place.owner?.description, let label = place.git?.label, d != label { lines.insert(label, at: 0) }
        if let t = place.owner?.agentType { lines.append("Made by a \(t) subagent") }
        if let ahead = place.git?.ahead, ahead > 0 { lines.append("\(ahead) commit\(ahead == 1 ? "" : "s") not on the main branch") }
        return lines.joined(separator: "\n")
    }
}

/// Close a worktree dino made, or clean up one nothing would be lost from.
struct WorktreeClosingItems: View {
    @EnvironmentObject var model: DinoModel
    let place: PlaceNode

    var body: some View {
        if place.dino {
            Button("Apply Changes and Close Worktree…") {
                model.closingWorktree = ClosingWorktree(path: place.path, label: place.label, apply: true)
            }
            Button("Discard Worktree…", role: .destructive) {
                model.closingWorktree = ClosingWorktree(path: place.path, label: place.label, apply: false)
            }
            Divider()
        } else if place.cleanable {
            Button("Clean Up Worktree") { model.cleanWorktrees([place.path]) }
                .help("Removes the worktree, and its branch if it's merged. Never forced.")
            Divider()
        }
    }
}
