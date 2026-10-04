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
    /// "dino", "claude" or "codex" by where it is; nil when there's no telling.
    var madeBy: String?
    /// Programs working in it right now, by name.
    var users: [String]?
    /// Something works in it (a program, a subagent, a change in the last few minutes): Clean Up
    /// leaves it alone.
    var inUse: Bool?
    var id: String { path }

    enum CodingKeys: String, CodingKey {
        case path, branch, dino, git, owner, users
        case madeBy = "made_by"
        case inUse = "in_use"
    }
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
    /// Files with uncommitted changes, new ones included.
    var uncommitted: UInt32?
    /// Commits on no remote and not on the main branch.
    var unpushed: UInt32?
    /// When something last changed in it, in seconds since 1970.
    var changed: UInt64?
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
    /// The branch PRs go into; nil for a plain folder.
    var defaultBranch: String?
    var id: String { path }

    enum CodingKeys: String, CodingKey {
        case path, name, worktrees
        case defaultBranch = "default_branch"
    }

    /// The main checkout's branch when it isn't the default one ("detached" without one); nil on
    /// the default branch or for a plain folder.
    var offDefaultBranch: String? {
        guard let main = worktrees.first else { return nil }
        guard let branch = main.branch else { return "detached" }
        return defaultBranch.map { $0 == branch } == false ? branch : nil
    }
}

/// How a git repo and a plain folder look everywhere a folder is shown.
enum FolderLook {
    static func icon(repo: Bool) -> String { repo ? "shippingbox" : "folder" }
    static func help(repo: Bool, path: String) -> String {
        "\(repo ? "Git repository" : "Folder, not a git repository")\n\((path as NSString).abbreviatingWithTildeInPath)"
    }
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
    var madeBy: String?
    var users: [String] = []
    var inUse = false
    var id: String { path }
    /// Its identity as a sidebar row: a plain row and one that opens to sessions aren't the same row.
    var rowID: String { sessions.isEmpty ? path : sessions.count == 1 ? path + "#one" : path + "#open" }

    /// Nothing in it would be lost by removing it: no uncommitted work, nobody in it.
    var cleanable: Bool {
        guard let git, !dino, removable else { return false }
        return !git.dirty
    }

    /// Clean Up may remove it (losing what's uncommitted, if asked to): a worktree, not the main
    /// checkout, with no session, subagent or program in it and no change in the last few minutes.
    var removable: Bool { git != nil && sessions.isEmpty && !inUse && owner?.running != true }

    /// Its work landed and nothing would be lost: Clean Up Merged takes it without asking.
    var mergedAndClean: Bool { git?.state == "merged" && cleanable }
}

struct RepoNode: Identifiable, Equatable {
    var repo: RepoInfo
    /// The main checkout first, then other worktrees; fan-out worktrees are in `groups` instead.
    var places: [PlaceNode]
    var groups: [GroupInfo]
    /// Session id → the worktrees its subagents made, shown under that session.
    var subagents: [String: [PlaceNode]] = [:]
    /// Worktrees no dino session runs in: dino's left behind, other tools', made by hand.
    var others: [PlaceNode] = []
    var id: String { repo.path }
    var isGit: Bool { !repo.worktrees.isEmpty }
    /// Of `others`, the ones whose work landed with nothing to lose: ready to clean up.
    var merged: [PlaceNode] { others.filter(\.mergedAndClean) }
    /// Has something to show: agents, fan-outs, worktrees dino made or merged, or it's the folder
    /// new sessions start in. Other worktrees alone don't count.
    func worthShowing(here: String) -> Bool {
        sessionCount > 0 || !groups.isEmpty || !merged.isEmpty || !subagents.isEmpty || places.contains(where: \.dino)
            || SessionTree.contains(repo.path, here)
    }

    /// The collapsed-rows key of the row tagged `tag` here, if that row opens, and whether it
    /// starts open (see RepoRows).
    func opening(_ tag: String) -> (key: String, startsOpen: Bool)? {
        if tag == "repo:\(repo.path)" { return (repo.path, true) }
        if tag == "others:\(id)", !others.isEmpty { return ("open:\(tag)", false) }
        if tag.hasPrefix("group:"), groups.contains(where: { "group:\($0.id)" == tag }) { return (tag, true) }
        if tag.hasPrefix("dir:"), let p = worktreePlaces.first(where: { "dir:\($0.path)" == tag }),
           p.sessions.count > 1 || p.sessions.contains(where: { subagents[$0.id] != nil }) {
            return (p.path, true)
        }
        if subagents[tag] != nil { return ("subagents:\(tag)", true) }
        return nil
    }

    /// The main checkout (or the folder): its sessions go right under the repo's row. Not there
    /// when a filter left it with none.
    var main: PlaceNode? { places.first { $0.path == repo.path } }
    /// The worktrees with sessions in them, each a row of its own.
    var worktreePlaces: [PlaceNode] { places.filter { $0.path != repo.path } }
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
                        sessions: [], dino: $0.dino, git: $0.git, owner: $0.owner,
                        madeBy: $0.madeBy, users: $0.users ?? [], inUse: $0.inUse ?? false
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
                for (pi, place) in node.places.enumerated() where contains(place.path, s.here ?? "") {
                    if place.path.count > (best?.depth ?? -1) { best = (ri, pi, place.path.count) }
                }
            }
            if let best { nodes[best.repo].places[best.place].sessions.append(s) } else { unfiled.append(s) }
        }
        let ids = Set(sessions.map(\.id))
        for i in nodes.indices {
            var kept: [PlaceNode] = []
            for (pi, place) in nodes[i].places.enumerated() {
                // The main checkout and worktrees with sessions in them stay in the list. A worktree
                // with no session (dino's own left after its session ended too) isn't somewhere you're
                // working in dino: it folds away under Other worktrees, which says what each is and
                // where it can be opened again or cleaned up.
                if pi == 0 || !place.sessions.isEmpty {
                    kept.append(place)
                } else if let owner = place.owner, ids.contains(owner.session) {
                    nodes[i].subagents[owner.session, default: []].append(place)
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
    case all, working, needsYou, done, idle, archived
    var id: String { rawValue }

    var label: String {
        switch self {
        case .all: "All"
        case .needsYou: "Needs you"
        case .working: "Working"
        case .done: "Done"
        case .idle: "Idle"
        case .archived: "Archived"
        }
    }

    /// The row's own status mark, for the filter when there's no room for its word.
    var symbol: String {
        switch self {
        case .all: "square.stack"
        case .needsYou: "exclamationmark.circle.fill"
        case .working: "circle.fill"
        case .done: "checkmark.circle.fill"
        case .idle: "circle"
        case .archived: "archivebox"
        }
    }

    var help: String {
        switch self {
        case .all: "Every session"
        case .needsYou: "Asking for something: a permission, an answer"
        case .done: "Finished, and you haven't looked yet"
        case .working: "Thinking, running tools, or waiting on its subagents and background commands"
        case .idle: "Waiting for a prompt, or exited"
        case .archived: "Archived sessions: stopped and kept, to pick up again (⇧⌘A archives the current one)"
        }
    }

    func passes(_ status: SessionStatus) -> Bool {
        switch self {
        case .all: true
        case .needsYou: status == .needsYou
        case .done: status == .done
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
        case .done: SessionStatus.done.color
        case .idle, .archived: .secondary
        }
    }
}

/// All · Working · Needs you · Done · Idle, with counts; one click each. Words when they fit,
/// else each status's mark with its count and the chosen one's word.
struct FilterBar: View {
    @EnvironmentObject var model: DinoModel
    @Binding var filter: SessionFilter

    var body: some View {
        let filters = SessionFilter.allCases.filter { $0 != .archived }
        let counts = Dictionary(uniqueKeysWithValues: filters.map { f in
            (f, model.sidebarSessions.filter { f.passes(model.status(of: $0)) && model.sidebarShows($0) }.count)
        })
        ViewThatFits(in: .horizontal) {
            bar(filters, counts, words: true)
            bar(filters, counts, words: false)
        }
    }

    private func bar(_ filters: [SessionFilter], _ counts: [SessionFilter: Int], words: Bool) -> some View {
        HStack(spacing: 2) {
            ForEach(filters) { f in
                let count = counts[f] ?? 0
                let on = f == filter
                Button { filter = f } label: {
                    HStack(spacing: 3) {
                        if words || on || f == .all {
                            Text(f.label).lineLimit(1).fixedSize()
                        } else {
                            Image(systemName: f.symbol).imageScale(.small)
                                .foregroundStyle(count > 0 ? AnyShapeStyle(f.color) : AnyShapeStyle(.tertiary))
                        }
                        Text("\(count)").monospacedDigit()
                            .foregroundStyle(on ? AnyShapeStyle(.primary) : count > 0 && f != .all ? AnyShapeStyle(f.color) : AnyShapeStyle(.tertiary))
                    }
                    .font(.caption.weight(on ? .semibold : .regular))
                    .padding(.horizontal, 6).padding(.vertical, 3)
                    .background(Capsule().fill(on ? Color.primary.opacity(0.1) : .clear))
                    .contentShape(Capsule())
                }
                .buttonStyle(.plain)
                .help("\(f.label): \(f.help)")
                .accessibilityLabel("\(f.label), \(count)")
                .accessibilityAddTraits(on ? .isSelected : [])
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
                Text("Archived")
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
        .accessibilityLabel(on ? "Back to sessions" : "Archived, \(model.archived.count)")
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
        let cwd = s.here ?? ""
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
        .accessibilityLabel(model.sidebarScopeName.map { "Showing \($0)" } ?? "Show one project or host")
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

/// A sidebar row that opens to the rows under it, the way a DisclosureGroup does, but with every
/// row a plain row of the list. With DisclosureGroups the list (an outline view) expanded an open
/// group only once it had made the group's own row, from inside making it: the rows that went in
/// shifted the ones below that were already laid out, and the table lost one of those row views.
/// It stayed where it was, drawn over whatever row came there: the scheduled task's row over a
/// session's when a repo came into the list above the Scheduled section, as at launch. Here a
/// group's rows go in with the group, in one update.
struct OpeningRows<Label: View, Content: View>: View {
    @Binding var open: Bool
    /// The opening row's tag, when it can be selected; the rows under it carry their own.
    let tag: String?
    @ViewBuilder var label: Label
    @ViewBuilder var content: Content

    init(open: Binding<Bool>, tag: String?, @ViewBuilder label: () -> Label, @ViewBuilder content: () -> Content) {
        _open = open
        self.tag = tag
        self.label = label()
        self.content = content()
    }

    var body: some View {
        if let tag {
            header.tag(tag)
        } else {
            header
        }
        if open {
            // Under the opening row's icon, a step further in for each level.
            content.padding(.leading, OpeningRows.chevron + 4)
        }
    }

    private var header: some View {
        HStack(spacing: 4) {
            Button { open.toggle() } label: {
                Image(systemName: "chevron.right")
                    .font(.caption2.weight(.semibold))
                    .foregroundStyle(.tertiary)
                    .rotationEffect(.degrees(open ? 90 : 0))
                    .frame(width: OpeningRows.chevron, height: 16)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help(open ? "Collapse" : "Expand")
            .accessibilityLabel(open ? "Collapse" : "Expand")
            label
        }
        .accessibilityAddTraits(.isHeader)
    }
}

extension OpeningRows {
    static var chevron: CGFloat { 10 }
}

/// Rows for one repo or folder: a row that opens, which you can select to start work there.
struct RepoRows: View {
    @EnvironmentObject var model: DinoModel
    let node: RepoNode
    var filter = SessionFilter.all
    @Binding var collapsed: Set<String>

    var body: some View {
        OpeningRows(open: expanded(node.id), tag: "repo:\(node.repo.path)") {
            repoLabel
        } content: {
            // The main checkout isn't a row of its own: its sessions are the repo's. Its branch is
            // on the repo's row when it isn't the default one.
            if let main = node.main {
                sessionRows(main.sessions, root: main.path)
            }
            // A worktree that gains or loses sessions changes kind (its one session's row, or one
            // that opens): a new identity then, so the list replaces the row instead of morphing it.
            ForEach(node.worktreePlaces, id: \.rowID) { place in
                if let s = sole(place) {
                    // No header over one session: its branch goes on the row.
                    SessionRow(session: s, index: 0, branch: branchName(place), root: place.path)
                        .tag(s.id)
                        .contextMenu {
                            SessionMenu(session: s)
                            Divider()
                            placeMenu(place)
                        }
                } else {
                    // The menu on the label only, as for the repo's row.
                    OpeningRows(open: expanded(place.id), tag: "dir:\(place.path)") {
                        WorktreeRow(place: place, title: headerTitle(place))
                            .contextMenu { placeMenu(place) }
                    } content: {
                        sessionRows(place.sessions, root: place.path)
                    }
                }
            }
            ForEach(node.groups) { g in
                OpeningRows(open: expanded("group:\(g.id)"), tag: "group:\(g.id)") {
                    GroupRow(group: g)
                } content: {
                    ForEach(g.members) { m in
                        if let s = model.sessions.first(where: { $0.id == m.session }), filter.passes(model.status(of: s)) {
                            SessionRow(session: s, index: 0, stat: m.stat).tag(s.id)
                                .contextMenu { SessionMenu(session: s) }
                        }
                    }
                }
            }
            if !node.others.isEmpty {
                OpeningRows(open: opened("others:\(node.id)"), tag: "others:\(node.id)") {
                    OtherWorktreesHeader(others: node.others)
                        .contextMenu { cleanUpItems(node.others) }
                } content: {
                    ForEach(node.others) { place in
                        OtherWorktreeRow(place: place)
                            .tag("dir:\(place.path)")
                            .contextMenu { otherMenu(place) }
                    }
                }
            }
        }
    }

    /// The repo's own row: a box for a git repo, a folder for a folder that isn't one, and the
    /// main checkout's branch when it isn't the default one.
    private var repoLabel: some View {
        PlaceRow(icon: FolderLook.icon(repo: node.isGit), title: node.repo.name, detail: nil, branch: node.repo.offDefaultBranch)
            .help(FolderLook.help(repo: node.isGit, path: node.repo.path)
                + (node.repo.offDefaultBranch.map { "\nOn \($0), not \(node.repo.defaultBranch ?? "its default branch")" } ?? ""))
            // On the label only: on the rows under it too, right-clicking a session showed its
            // folder's menu instead of its own.
            .contextMenu {
                Button("Copy Path") {
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(node.repo.path, forType: .string)
                }
                Button("Show in Finder") { NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: node.repo.path) }
                if !node.others.isEmpty {
                    Divider()
                    cleanUpItems(node.others)
                }
            }
    }

    /// Clean Up Merged takes the ones whose work landed with nothing to lose, at once; Clean Up…
    /// every one nothing works in, after saying what would be lost.
    @ViewBuilder
    private func cleanUpItems(_ others: [PlaceNode]) -> some View {
        let merged = others.filter(\.mergedAndClean)
        let removable = others.filter(\.removable)
        Button(merged.isEmpty ? "Clean Up Merged" : "Clean Up \(merged.count) Merged") { model.cleanWorktrees(merged.map(\.path)) }
            .disabled(merged.isEmpty)
        Button(removable.isEmpty ? "Clean Up…" : "Clean Up \(removable.count)…") { model.cleaningUp = CleanUpPlan(places: removable) }
            .disabled(removable.isEmpty)
    }

    @ViewBuilder
    private func otherMenu(_ place: PlaceNode) -> some View {
        Button("Open in New Terminal") { model.newShell(in: place.path) }
        if place.dino {
            WorktreeClosingItems(place: place)
        } else if place.removable {
            Button(place.cleanable ? "Clean Up" : "Clean Up…") {
                if place.cleanable { model.cleanWorktrees([place.path]) } else { model.cleaningUp = CleanUpPlan(places: [place]) }
            }
            Divider()
        } else {
            Button(OtherWorktreeRow.usage(place) ?? "In use") {}.disabled(true)
            Divider()
        }
        Button("Copy Path") {
            NSPasteboard.general.clearContents()
            NSPasteboard.general.setString(place.path, forType: .string)
        }
        Button("Show in Finder") { NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: place.path) }
    }

    /// Its only session, when nothing else hangs under it (subagents' worktrees keep the header).
    private func sole(_ place: PlaceNode) -> SessionInfo? {
        guard place.sessions.count == 1, let s = place.sessions.first, node.subagents[s.id] == nil else { return nil }
        return s
    }

    /// The branch itself for the chip, as `git` would print it, not a commit subject.
    private func branchName(_ place: PlaceNode) -> String {
        node.repo.worktrees.first { $0.path == place.path }?.branch.map(Self.readable) ?? place.label
    }

    /// A worktree dino named for a session ("claude-ab12") reads as its first session's name.
    private func headerTitle(_ place: PlaceNode) -> String? {
        guard place.owner?.description == nil, Self.autoNamed(place.label), let first = place.sessions.first else { return nil }
        return first.display
    }

    static func readable(_ branch: String) -> String {
        branch.hasPrefix("dino/") ? String(branch.dropFirst(5)) : branch
    }

    /// "claude-ab12", "Subagent a367461": made up by dino or an agent, not by a person.
    static func autoNamed(_ label: String) -> Bool {
        if label.hasPrefix("Subagent ") { return true }
        guard let dash = label.lastIndex(of: "-") else { return false }
        let tail = label[label.index(after: dash)...]
        return tail.count >= 4 && tail.allSatisfy(\.isHexDigit) && tail.contains(where: \.isNumber)
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
    private func sessionRows(_ sessions: [SessionInfo], root: String) -> some View {
        // With or without subagents' worktrees under it, as for folders.
        ForEach(sessions.map { (key: node.subagents[$0.id] == nil ? $0.id : "\($0.id)#sub", session: $0) }, id: \.key) { row in
            let s = row.session
            if let children = node.subagents[s.id] {
                OpeningRows(open: expanded("subagents:\(s.id)"), tag: s.id) {
                    SessionRow(session: s, index: 0, root: root)
                        .contextMenu { SessionMenu(session: s) }
                } content: {
                    worktreeRows(children)
                }
            } else {
                SessionRow(session: s, index: 0, root: root)
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

    /// Where it is now: a shell follows its `cd`s (and so does an agent typed into it); everything
    /// else stays in the folder it started in.
    var here: String? { agent_id == "shell" && host == nil ? shell_cwd ?? cwd : cwd }
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
        OpeningRows(open: Binding(
            get: { !collapsed.contains("host:\(host)") },
            set: { open in if open { collapsed.remove("host:\(host)") } else { collapsed.insert("host:\(host)") } }
        ), tag: nil) {
            PlaceRow(icon: "server.rack", title: host, detail: "SSH")
                .help("Sessions running on \(host) over SSH")
        } content: {
            ForEach(sessions) { s in
                SessionRow(session: s, index: 0)
                    .tag(s.id)
                    .contextMenu { SessionMenu(session: s) }
            }
        }
    }
}

struct PlaceRow: View {
    let icon: String
    let title: String
    let detail: String?
    /// A branch to show beside the title, as session rows show theirs.
    var branch: String?

    var body: some View {
        HStack(spacing: 6) {
            // Not a Label: sidebar rows tint Label icons with the accent color.
            Image(systemName: icon).foregroundStyle(.secondary).frame(width: 16).accessibilityHidden(true)
            Text(title).fontWeight(.medium).lineLimit(1)
            if let detail {
                Text(detail).font(.caption).foregroundStyle(.secondary).lineLimit(1)
            }
            if let branch {
                BranchChip(branch: branch)
            }
        }
    }
}

/// A branch name, small and monospaced, as git prints it.
struct BranchChip: View {
    let branch: String

    var body: some View {
        Text(branch)
            .font(.caption.monospaced())
            .foregroundStyle(.secondary)
            .lineLimit(1)
            .truncationMode(.middle)
            .padding(.horizontal, 4).padding(.vertical, 1)
            .background(.quaternary.opacity(0.6), in: RoundedRectangle(cornerRadius: 3))
    }
}

/// "Other worktrees  5 · 2 in use": worktrees of the repo no dino session runs in.
struct OtherWorktreesHeader: View {
    let others: [PlaceNode]

    var body: some View {
        let busy = others.filter { !$0.removable }.count
        PlaceRow(icon: "square.stack.3d.up", title: "Other worktrees", detail: busy > 0 ? "\(others.count) · \(busy) in use" : "\(others.count)")
            .help("""
                Worktrees of this repo with no dino session in them: left by a dino session, made by \
                Claude Code or Codex, or by hand. In use: a program works in it, or it changed in the \
                last few minutes. Clean Up leaves those alone and never touches the main checkout.
                """)
    }
}

/// A worktree no dino session runs in: what it is, who made it, whether something works in it now
/// and what removing it would lose.
/// "Fix the parser                              In use"
/// "Claude Code · in use by cargo · 3 uncommitted"
struct OtherWorktreeRow: View {
    let place: PlaceNode

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 6) {
                Image(systemName: "arrow.triangle.branch").foregroundStyle(.secondary).frame(width: 16)
                    .accessibilityHidden(true)
                Text(place.owner?.description ?? place.label).lineLimit(1).truncationMode(.tail).layoutPriority(1)
                Spacer(minLength: 4)
                if !place.removable {
                    Text("In use").font(.caption).foregroundStyle(SessionStatus.working.color).fixedSize()
                } else if place.git?.state == "merged" {
                    Image(systemName: "arrow.triangle.merge").font(.caption).foregroundStyle(.secondary)
                        .accessibilityLabel("Merged")
                }
            }
            Text(Self.facts(place).joined(separator: " · "))
                .font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.tail)
                .padding(.leading, 22)
        }
        .padding(.vertical, 2)
        .help(([place.label, (place.path as NSString).abbreviatingWithTildeInPath] + Self.facts(place, long: true)).joined(separator: "\n"))
        .accessibilityElement(children: .combine)
    }

    /// Who made it: dino, Claude Code or Codex by where it is, else "other" (by hand, another
    /// tool); `long` says it as a sentence, for the tooltip.
    static func maker(_ place: PlaceNode, long: Bool = false) -> String {
        let who: String? = switch place.madeBy {
        case "dino": "dino"
        case "claude": "Claude Code"
        case "codex": "Codex"
        default: place.dino ? "dino" : nil
        }
        guard long else { return who ?? "other" }
        return who.map { "Made by \($0)" } ?? "Made outside dino, by hand or by another tool"
    }

    /// What's working in it, if anything: "in use by cargo, zsh", "a subagent is working in it",
    /// "changed 3 min ago".
    static func usage(_ place: PlaceNode) -> String? {
        if !place.users.isEmpty { return "in use by \(place.users.joined(separator: ", "))" }
        if place.owner?.running == true { return "a subagent is working in it" }
        if place.inUse, let changed = place.git?.changed { return "changed \(ago(changed))" }
        return place.inUse ? "in use" : nil
    }

    /// Maker, use, what removing it would lose, and state; short for the row (most telling
    /// first, as the row cuts the rest off) or in full for its tooltip.
    static func facts(_ place: PlaceNode, long: Bool = false) -> [String] {
        var out = [maker(place, long: long)]
        let use = usage(place)
        if let use { out.append(long ? use.prefix(1).uppercased() + use.dropFirst() : use) }
        guard let git = place.git else { return out }
        let uncommitted = git.uncommitted ?? (git.dirty ? 1 : 0)
        let unpushed = git.unpushed ?? 0
        if uncommitted > 0 {
            out.append(git.uncommitted == nil ? "uncommitted changes" : "\(uncommitted) uncommitted")
        }
        if unpushed > 0 { out.append("\(unpushed) unpushed") }
        switch git.state {
        case "merged": out.append("merged")
        case "empty" where uncommitted == 0: out.append("no changes")
        case "ready" where unpushed == 0: out.append("pushed")
        default: break
        }
        if use == nil, let changed = git.changed { out.append("last changed \(ago(changed))") }
        return out
    }

    private static let relative: RelativeDateTimeFormatter = {
        let f = RelativeDateTimeFormatter()
        f.unitsStyle = .short
        return f
    }()

    private static func ago(_ secs: UInt64) -> String {
        relative.localizedString(for: Date(timeIntervalSince1970: TimeInterval(secs)), relativeTo: Date())
    }
}

/// Asks before Clean Up… removes worktrees, saying what would be lost.
struct CleanUpAlert: ViewModifier {
    @ObservedObject var model: DinoModel

    func body(content: Content) -> some View {
        content.alert(
            model.cleaningUp?.title ?? "",
            isPresented: Binding(get: { model.cleaningUp != nil }, set: { if !$0, model.cleaningUp != nil { model.cleaningUp = nil } }),
            presenting: model.cleaningUp
        ) { plan in
            Button(plan.losing.isEmpty ? "Clean Up" : "Clean Up and Lose Changes", role: plan.losing.isEmpty ? nil : .destructive) {
                model.cleanWorktrees(plan.places.map(\.path), force: !plan.losing.isEmpty)
            }
            Button("Cancel", role: .cancel) {}
        } message: { plan in
            Text(plan.message)
        }
    }
}

/// Clean Up…: the worktrees it would remove, and what that loses.
struct CleanUpPlan: Equatable {
    var places: [PlaceNode]

    /// Uncommitted changes that go with them.
    var losing: [PlaceNode] { places.filter { ($0.git?.uncommitted ?? 0) > 0 || $0.git?.dirty == true } }
    /// Commits on no remote: they stay on their branches, which aren't deleted.
    var unpushed: [PlaceNode] { places.filter { ($0.git?.unpushed ?? 0) > 0 } }

    var title: String { places.count == 1 ? "Clean up “\(places[0].label)”?" : "Clean up \(places.count) worktrees?" }

    var message: String {
        var lines = [places.count == 1 ? "Its folder is removed." : "Their folders are removed. None is in use."]
        if !losing.isEmpty {
            lines.append("Uncommitted changes are lost in:\n" + losing.map { "• \($0.label) (\($0.git?.uncommitted.map { "\($0) file\($0 == 1 ? "" : "s")" } ?? "changes"))" }.joined(separator: "\n"))
        }
        if !unpushed.isEmpty {
            lines.append("Unpushed commits stay on their branches, which are kept:\n" + unpushed.map { "• \($0.label) (\($0.git?.unpushed ?? 0) commit\($0.git?.unpushed == 1 ? "" : "s"))" }.joined(separator: "\n"))
        }
        lines.append("A branch goes too only when it's merged. The main checkout is never touched.")
        return lines.joined(separator: "\n\n")
    }
}

/// A worktree at a glance: what it's for, how much changed, and where it stands.
/// "● Review code button   +312 −20   running"
struct WorktreeRow: View {
    let place: PlaceNode
    /// In place of an auto-made branch name.
    var title: String?

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: "arrow.triangle.branch").foregroundStyle(.secondary).frame(width: 16)
                .accessibilityHidden(true)
            Text(title ?? place.owner?.description ?? place.label).lineLimit(1).truncationMode(.tail).layoutPriority(1)
            Spacer(minLength: 4)
            if let git = place.git {
                if git.added + git.removed > 0 {
                    HStack(spacing: 3) {
                        if git.added > 0 { Text("+\(git.added)").foregroundStyle(Color(nsColor: .systemGreen)) }
                        if git.removed > 0 { Text("−\(git.removed)").foregroundStyle(Color(nsColor: .systemRed)) }
                    }
                    .font(.caption.monospacedDigit())
                    .fixedSize()
                }
                if git.dirty {
                    Circle().fill(Color(nsColor: .systemOrange)).frame(width: 6, height: 6).help("In progress: uncommitted changes")
                        .accessibilityLabel("Uncommitted changes")
                }
            }
            if place.owner?.running == true {
                ProgressView().controlSize(.mini).help("Running").accessibilityLabel("Running")
            } else if let (icon, tip) = statusIcon {
                Image(systemName: icon).font(.caption).foregroundStyle(.secondary).help(tip)
                    .accessibilityLabel(tip)
            }
        }
        .help(help)
    }

    /// Icons, not words: the label needs the room in a narrow sidebar. The dirty dot already says "in progress".
    private var statusIcon: (String, String)? {
        switch place.git?.state {
        case "ready": return ("checkmark.circle", "Ready: committed, not on the main branch yet")
        case "merged": return ("arrow.triangle.merge", "Merged into the main branch")
        case "empty": return place.owner == nil ? ("circle.dashed", "No changes yet: nothing committed or edited in this worktree") : ("checkmark", "Done: nothing changed")
        default: return nil
        }
    }

    private var help: String {
        var lines = [place.path]
        if let label = place.git?.label, label != (title ?? place.owner?.description ?? place.label) { lines.insert(label, at: 0) }
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
