import SwiftUI

/// A checkout of a repo (see crates/dino-core/src/worktree.rs).
struct Worktree: Codable, Equatable, Identifiable {
    var path: String
    var branch: String?
    /// dino made it for a session (see `ClosingWorktree`).
    var dino: Bool
    var id: String { path }
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
    var id: String { path }
}

struct RepoNode: Identifiable, Equatable {
    var repo: RepoInfo
    /// The main checkout first, then other worktrees; fan-out worktrees are in `groups` instead.
    var places: [PlaceNode]
    var groups: [GroupInfo]
    var id: String { repo.path }
    var isGit: Bool { !repo.worktrees.isEmpty }
    /// One checkout and no fan-outs: list its sessions right under the repo.
    var flat: Bool { places.count == 1 && groups.isEmpty }
    var sessionCount: Int { places.reduce(0) { $0 + $1.sessions.count } }
    /// Changes when rows switch between plain rows and disclosure groups. The sidebar keys
    /// rows on it: macOS List leaves stale rows behind when that switch is diffed in place.
    var shape: String {
        ([repo.path, flat ? "flat" : "tree"] + places.map { "\($0.path)=\($0.sessions.isEmpty)" } + groups.map(\.id))
            .joined(separator: "|")
    }
}

enum SessionTree {
    /// Each session goes under the deepest worktree or folder containing its cwd; fan-out
    /// members go under their group. Sessions the tree doesn't cover yet come back as `unfiled`.
    static func build(repos: [RepoInfo], sessions: [SessionInfo], groups: [GroupInfo]) -> (repos: [RepoNode], unfiled: [SessionInfo]) {
        let inGroup = Set(groups.flatMap { $0.members.map(\.session) })
        let groupWorktrees = Set(groups.flatMap { $0.members.map(\.worktree) })
        var nodes = repos.map { r in
            let places = r.worktrees.isEmpty
                ? [PlaceNode(path: r.path, label: r.name, sessions: [])]
                : r.worktrees.filter { !groupWorktrees.contains($0.path) }.map {
                    PlaceNode(path: $0.path, label: $0.branch ?? URL(fileURLWithPath: $0.path).lastPathComponent, sessions: [], dino: $0.dino)
                }
            return RepoNode(repo: r, places: places, groups: groups.filter { $0.repo == r.path })
        }
        var unfiled: [SessionInfo] = []
        for s in sessions where !inGroup.contains(s.id) {
            var best: (repo: Int, place: Int, depth: Int)?
            for (ri, node) in nodes.enumerated() {
                for (pi, place) in node.places.enumerated() where contains(place.path, s.cwd ?? "") {
                    if place.path.count > (best?.depth ?? -1) { best = (ri, pi, place.path.count) }
                }
            }
            if let best { nodes[best.repo].places[best.place].sessions.append(s) } else { unfiled.append(s) }
        }
        return (nodes, unfiled)
    }

    static func contains(_ dir: String, _ path: String) -> Bool {
        path == dir || path.hasPrefix(dir.hasSuffix("/") ? dir : dir + "/")
    }
}

// MARK: - Sidebar rows

/// Rows for one repo or folder: a disclosure row you can select to start work there.
struct RepoRows: View {
    @EnvironmentObject var model: DinoModel
    let node: RepoNode
    @Binding var collapsed: Set<String>

    var body: some View {
        DisclosureGroup(isExpanded: expanded(node.id)) {
            if node.flat {
                sessionRows(node.places[0].sessions)
            } else {
                ForEach(node.places) { place in
                    Group {
                        if place.sessions.isEmpty {
                            PlaceRow(icon: "arrow.triangle.branch", title: place.label, detail: nil)
                        } else {
                            DisclosureGroup(isExpanded: expanded(place.id)) {
                                sessionRows(place.sessions)
                            } label: {
                                PlaceRow(icon: "arrow.triangle.branch", title: place.label, detail: nil)
                            }
                        }
                    }
                    .tag("dir:\(place.path)")
                    .contextMenu {
                        if place.dino {
                            Button("Apply Changes and Close Worktree…") {
                                model.closingWorktree = ClosingWorktree(path: place.path, label: place.label, apply: true)
                            }
                            Button("Discard Worktree…", role: .destructive) {
                                model.closingWorktree = ClosingWorktree(path: place.path, label: place.label, apply: false)
                            }
                            Divider()
                        }
                        Button("Show in Finder") { NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: place.path) }
                    }
                }
                ForEach(node.groups) { g in
                    DisclosureGroup(isExpanded: expanded("group:\(g.id)")) {
                        ForEach(g.members) { m in
                            if let s = model.sessions.first(where: { $0.id == m.session }) {
                                SessionRow(session: s, index: 0, stat: m.stat).tag(s.id)
                            }
                        }
                    } label: {
                        GroupRow(group: g)
                    }
                    .tag("group:\(g.id)")
                }
            }
        } label: {
            PlaceRow(
                icon: node.isGit ? "shippingbox" : "folder",
                title: node.repo.name,
                detail: node.flat && node.isGit ? node.places[0].label : nil
            )
        }
        .tag("dir:\(node.repo.path)")
        .contextMenu {
            Button("Copy Path") {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(node.repo.path, forType: .string)
            }
            Button("Show in Finder") { NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: node.repo.path) }
        }
    }

    private func sessionRows(_ sessions: [SessionInfo]) -> some View {
        ForEach(sessions) { s in
            SessionRow(session: s, index: 0)
                .tag(s.id)
                .contextMenu {
                    Button("Kill Session", role: .destructive) { model.kill(s.id) }
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
