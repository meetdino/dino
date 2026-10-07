import AppKit
import SwiftUI

// MARK: - Grouping

/// How the sidebar lists sessions: under the repo and worktree they're in (Project), or under
/// what they're doing (State: Needs you, Working, Done, Idle). Kept in the defaults, as the
/// sidebar's filter is; the Filter menu and View → Group Sidebar By choose it.
enum SidebarGrouping: String, CaseIterable, Identifiable {
    case project, state
    var id: String { rawValue }
    static let key = "sidebar.groupBy"

    var label: String {
        switch self {
        case .project: "Project"
        case .state: "State"
        }
    }
}

/// The State grouping's sections, in order: the status filters, so their counts agree.
enum StateSection {
    static let order: [SessionFilter] = [.needsYou, .working, .done, .idle]

    /// The collapsed-rows key (and the row's tag) of a section.
    static func tag(_ f: SessionFilter) -> String { "state:\(f.rawValue)" }

    /// The section a session goes in.
    static func of(_ status: SessionStatus) -> SessionFilter {
        order.first { $0.passes(status) } ?? .idle
    }

    /// `sessions` split into the sections, each in the order given (pinned ones first).
    static func split(_ sessions: [SessionInfo], status: (SessionInfo) -> SessionStatus) -> [(SessionFilter, [SessionInfo])] {
        let sorted = sessions.filter { $0.pinned == true } + sessions.filter { $0.pinned != true }
        let by = Dictionary(grouping: sorted) { of(status($0)) }
        return order.map { ($0, by[$0] ?? []) }
    }
}

/// Where a session is, for its tag when the sidebar is grouped by state: the repo (with its node
/// for the repo's menu), its worktree when it has one of its own, an SSH host, or a folder.
struct SessionPlace: Equatable {
    var name: String
    var node: RepoNode?
    /// A worktree of the repo, not its main checkout.
    var worktree: PlaceNode?

    /// Each session of `tree` by id, filed as the project tree files it.
    static func index(_ tree: (repos: [RepoNode], unfiled: [SessionInfo])) -> [String: SessionPlace] {
        var out: [String: SessionPlace] = [:]
        for node in tree.repos {
            for place in node.places {
                for s in place.sessions {
                    out[s.id] = SessionPlace(name: node.repo.name, node: node, worktree: place.path == node.repo.path ? nil : place)
                }
            }
            for (_, forks) in node.forks {
                for f in forks {
                    let place = node.forkPlaces[f.id]
                    out[f.id] = SessionPlace(name: node.repo.name, node: node, worktree: place?.path == node.repo.path ? nil : place)
                }
            }
        }
        for s in tree.unfiled {
            let name = s.host ?? s.here.map { URL(fileURLWithPath: $0).lastPathComponent } ?? "—"
            out[s.id] = SessionPlace(name: name)
        }
        return out
    }
}

// MARK: - What a row says, in words

/// Everything a session's row knows, in words: the one tooltip that carries what the minimal row
/// leaves out (agent, model, sub-state, branch, PR, split, context and tokens, and the rest), and
/// the row's accessibility label, which says all of it too. Pure, so it can be tested.
struct RowFacts: Equatable {
    struct Input {
        var name: String
        var status: SessionStatus
        /// "Claude Code", as the launcher calls it.
        var agent: String
        /// "sonnet-5-5", or "tier → model".
        var model: String?
        var needs: String?
        var waitingOn: String?
        var serving: String?
        /// A shell's folder when it isn't where the row is filed.
        var shellPlace: String?
        var lastExit: Int?
        var branch: String?
        var pr: PrInfo?
        var auto: AutoPr?
        /// The other sessions in its split, named.
        var splitWith: String?
        var contextUsed: UInt64?
        var contextLimit: UInt64?
        var tokensIn: UInt64 = 0
        var tokensOut: UInt64 = 0
        var requests: UInt64 = 0
        var routes: [RouteUsage] = []
        var fallback: String?
        var error: String?
        var automation: String?
        var insteadOf: String?
        var forkedFrom: String?
        /// "Started by Fix CI's agent", "Its agent started Docs".
        var peers: [String] = []
        var pinned = false
        var using: String?
        /// The project it's in, when the row says it as a tag (grouped by state).
        var project: String?
    }

    /// The tooltip's lines, most telling first.
    var tooltip: [String]
    /// The row for VoiceOver: its name, its status in words, and every line of the tooltip.
    var accessibility: String

    init(_ i: Input) {
        var lines: [String] = []
        lines.append([i.agent, i.model, Self.state(i)].compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: " · "))
        if let needs = i.needs { lines.append("Asks: \(needs)") }
        var place: [String] = []
        if let p = i.project { place.append("In \(p)") }
        if let b = i.branch { place.append("Branch \(b)") }
        if let pr = i.pr { place.append(Self.prText(pr, auto: i.auto)) }
        if !place.isEmpty { lines.append(place.joined(separator: " · ")) }
        if let s = i.splitWith { lines.append("In a split with \(s)") }
        var use: [String] = []
        if let used = i.contextUsed, let limit = i.contextLimit, limit > 0 {
            use.append("Context \(Int((Double(used) / Double(limit) * 100).rounded()))%")
        }
        if i.requests > 0 {
            use.append("↑\(tokens(i.tokensIn)) in · ↓\(tokens(i.tokensOut)) out")
            use.append("\(i.requests) request\(i.requests == 1 ? "" : "s")")
        }
        if !use.isEmpty { lines.append(use.joined(separator: " · ")) }
        if i.routes.count > 1 {
            lines += i.routes.map { "\($0.name): ↑\(tokens($0.input_tokens)) in · ↓\(tokens($0.output_tokens)) out" }
        }
        if let s = i.serving { lines.append("Serving :\(s.replacingOccurrences(of: ", ", with: " :"))") }
        if let here = i.shellPlace {
            lines.append(here + (i.lastExit.map { $0 != 0 ? " · exit \($0)" : "" } ?? ""))
        } else if let code = i.lastExit, code != 0 {
            lines.append("Last command: exit \(code)")
        }
        if let f = i.fallback { lines.append(f) }
        if let e = i.error { lines.append("Error: \(e)") }
        if let u = i.using { lines.append(u) }
        if let a = i.automation { lines.append("Started by the automation “\(a)”") }
        if let w = i.insteadOf { lines.append(w) }
        if let f = i.forkedFrom { lines.append("Forked from “\(f)”") }
        lines += i.peers
        if i.pinned { lines.append("Pinned") }
        tooltip = lines
        accessibility = ([i.name, i.status.label] + lines).joined(separator: ", ")
    }

    /// The tooltip, with how to rename at the end.
    var help: String { (tooltip + ["Double-click to rename"]).joined(separator: "\n") }

    /// The finer state behind the status word: "Thinking", "Waiting on 1 agent".
    static func state(_ i: Input) -> String {
        switch i.status {
        case .thinking: "Thinking"
        case .working: "Working"
        case .waiting: i.waitingOn.map { "Waiting on \($0)" } ?? "Waiting on subagents or commands"
        case .needsYou: "Needs you"
        case .done: "Done"
        case .idle: "Idle"
        case .ended: "Exited, Enter resumes it"
        case .exited: "Exited with an error"
        }
    }

    /// "PR #42, checks running", "PR #42, 1 failing · auto merges when checks pass".
    static func prText(_ pr: PrInfo, auto: AutoPr?) -> String {
        let look = PRLook(pr).label
        let automatic = pr.isOpen && auto?.any == true
        return "PR #\(pr.number), \(look.prefix(1).lowercased() + look.dropFirst())" + (automatic ? " · auto \(auto!.describe)" : "")
    }

    /// The row keeps one PR mark only: a red ✕ while checks fail.
    static func failing(_ pr: PrInfo?) -> Bool { pr.map { $0.isOpen && $0.checks.failed > 0 } ?? false }
}

// MARK: - Rows

/// A session's row: its name and at most one mark. Working is a green dot, Done a blue dot with
/// the name in bold (like unread mail), Idle nothing and the name muted; Needs you is the only one
/// with a word and shows what it asks under the name. The rest is in one tooltip, the row's
/// accessibility label, its menu and the toolbar. Grouped by state, the section says the status
/// and the row says its project instead.
struct SessionRow: View {
    @EnvironmentObject var model: DinoModel
    /// The session as it is now: how it's doing changes here, redrawing this row, not the sidebar.
    @ObservedObject private var live: LiveSession
    /// The worktree's branch, for a session alone in its worktree.
    var branch: String?
    /// The repo or worktree it's filed under: a shell's folder shows only when it's somewhere else.
    var root: String?
    /// Grouped by state: where it is, as a tag.
    var place: SessionPlace?
    @State private var hovering = false
    @State private var anchor = CostAnchorView()

    init(session: SessionInfo, index _: Int = 0, branch: String? = nil, root: String? = nil, place: SessionPlace? = nil) {
        _live = ObservedObject(wrappedValue: LiveSessions.of(session))
        self.branch = branch
        self.root = root
        self.place = place
    }

    private var session: SessionInfo { live.info }

    var body: some View {
        let status = model.status(of: session)
        let pr = model.pr(of: session)
        let facts = facts(status, pr: pr)
        let renaming = model.renaming == Renaming(id: session.id, place: .sidebar)
        VStack(alignment: .leading, spacing: 2) {
            if status == .needsYou, place == nil, !hovering {
                // The pill while the whole name fits beside it; else the "!" and the question
                // under the name say it, and the name gets the room.
                ViewThatFits(in: .horizontal) {
                    firstLine(status, pr: pr, pill: true)
                    firstLine(status, pr: pr, pill: false)
                }
            } else {
                firstLine(status, pr: pr, pill: false)
            }
            if let needs = session.needs {
                Text(needs).font(.caption).foregroundStyle(SessionStatus.needsYou.color)
                    .lineLimit(2).truncationMode(.tail)
                    .padding(.leading, place == nil ? 20 : 0)
            } else if place != nil, let error = session.error {
                // Grouped by state there's no dot to turn red: the message says it.
                Text(error).font(.caption).foregroundStyle(SessionStatus.exited.color)
                    .lineLimit(1).truncationMode(.tail)
            }
        }
        .padding(.vertical, 3)
        .contentShape(Rectangle())
        .help(facts.help)
        .modifier(RowAccessibility(on: !renaming, label: facts.accessibility, archive: model.canArchive(session.id) ? { model.archive(session.id) } : nil, rename: {
            model.renaming = Renaming(id: session.id, place: .sidebar)
        }))
        .background(CostAnchor(holder: anchor))
        .onHover {
            hovering = $0
            // What it costs the Mac, in a card beside the row; only for what runs on this Mac.
            if !$0 || (session.host == nil && !session.exited) { CostCard.shared.hover(session.id, anchor: anchor.view, on: $0) }
        }
        .onDisappear { CostCard.shared.hover(session.id, anchor: nil, on: false) }
    }

    private func firstLine(_ status: SessionStatus, pr: PrInfo?, pill: Bool) -> some View {
        HStack(spacing: 6) {
            if place == nil {
                MinimalMark(status: status, error: session.error != nil)
                    .overlay { RowProgress(signal: PaneSignals.of(session.id)) }
            } else {
                RowProgress(signal: PaneSignals.of(session.id))
            }
            SessionName(
                session: session, place: .sidebar,
                font: .body.weight(status == .needsYou || status == .done ? .semibold : place == nil ? .regular : .medium),
                color: status == .idle || status == .ended ? .secondary : .primary,
                tooltip: false
            )
            .layoutPriority(1)
            // Computer use stays on the row while it lasts: the Mac is being driven.
            if let reach = session.reach { UsingMark(session: session, reach: reach) }
            Spacer(minLength: 4)
            if hovering {
                if session.pinned == true {
                    Image(systemName: "pin.fill")
                        .font(.caption2).foregroundStyle(.tertiary)
                        .rotationEffect(.degrees(45))
                        .help("Pinned: stays at the top of its group and is never archived automatically")
                        .accessibilityHidden(true)
                }
                if model.canArchive(session.id) {
                    // Where Claude desktop has it: on the row, under the pointer.
                    Button { model.archive(session.id) } label: { Image(systemName: "archivebox") }
                        .buttonStyle(.plain)
                        .foregroundStyle(.secondary)
                        .help("Archive (⇧⌘A): stop this session and move it to Archived, where you can resume it later")
                        .accessibilityLabel("Archive")
                }
            }
            if !hovering || !model.canArchive(session.id) {
                if pill { NeedsYouPill() }
                trailing(status, pr: pr)
            }
        }
    }

    /// What's at the end of the row when the pointer isn't on it, after the Needs you pill: by
    /// project, a red ✕ while its PR's checks fail; by state, the PR's icon and the project's tag.
    @ViewBuilder
    private func trailing(_ status: SessionStatus, pr: PrInfo?) -> some View {
        if let place {
            if let pr {
                let look = PRLook(pr)
                Image(systemName: look.icon).font(.caption).foregroundStyle(look.color)
                    .help(RowFacts.prText(pr, auto: session.auto))
                    .accessibilityHidden(true)
            }
            ProjectTag(place: place)
        } else if RowFacts.failing(pr), let pr {
            Image(systemName: "xmark.circle.fill").font(.caption).foregroundStyle(SessionStatus.exited.color)
                .help(RowFacts.prText(pr, auto: session.auto))
                .accessibilityHidden(true)
        }
    }

    private func facts(_ status: SessionStatus, pr: PrInfo?) -> RowFacts {
        model.rowFacts(session, status: status, pr: pr, branch: branch, root: root, place: place)
    }
}

extension DinoModel {
    /// What session `s`'s row says in its tooltip and to VoiceOver (see `RowFacts`).
    func rowFacts(_ s: SessionInfo, status: SessionStatus, pr: PrInfo?, branch: String?, root: String?, place: SessionPlace?) -> RowFacts {
        let split = split(of: s.id).map { split in
            ListFormatter.localizedString(byJoining: split.panes.filter { $0 != s.id }.map { id in
                sessions.first { $0.id == id }?.display ?? "another session"
            })
        }
        let insteadOf = s.instead_of.map { why in
            let asked = launchers.first { $0.agent_id == why.agent_id }?.label ?? why.agent_id
            return "Started instead of \(asked): \(why.name) was at its limit\(why.resets_at.map { " until \(Clock.short($0))" } ?? "")"
        }
        let parent = s.forked_from.map { from in sessions.first { $0.id == from.session }?.display ?? from.name }
        let fallback = s.fallback.flatMap { f in saysInFooter(f) ? nil : f.rowLabel }
        var input = RowFacts.Input(
            name: tabName(s),
            status: status,
            agent: s.plainShell ? "Shell" : launchers.first { $0.agent_id == s.agent }?.label ?? s.agentWord,
            model: Self.modelText(s),
            needs: s.needs,
            waitingOn: s.waitingOn,
            serving: status == .idle || status == .done ? s.serving : nil,
            shellPlace: Self.shellPlace(s, root: root),
            lastExit: s.inside == nil ? s.last_exit : nil,
            branch: branch,
            pr: pr,
            auto: s.auto,
            splitWith: split
        )
        if let ctx = s.contextUse {
            input.contextUsed = ctx.used
            input.contextLimit = ctx.limit
        }
        input.tokensIn = s.input_tokens
        input.tokensOut = s.output_tokens
        input.requests = s.requests
        input.routes = s.usage_by_route ?? []
        input.fallback = fallback
        input.error = s.error
        input.automation = s.scheduled
        input.insteadOf = insteadOf
        input.forkedFrom = parent
        input.peers = PeerLinks.of(s, in: self).map(\.help)
        input.pinned = s.pinned == true
        input.using = UsingDisplay.shown ? s.usingSentence : nil
        input.project = place?.name
        return RowFacts(input)
    }

    /// A shell's folder when it isn't the place it's filed under: relative inside it, else in full.
    static func shellPlace(_ s: SessionInfo, root: String?) -> String? {
        guard s.inside == nil, let here = s.shell_cwd else { return nil }
        let base = root ?? s.cwd
        if let base, here == base { return nil }
        if let base, SessionTree.contains(base, here) { return String(here.dropFirst(base.count + 1)) }
        return NSString(string: here).abbreviatingWithTildeInPath
    }

    /// The model it's on: as its agent says (routing off too), else as its last call asked.
    static func modelText(_ s: SessionInfo) -> String? {
        guard s.agent_model != nil || s.requests > 0, let now = s.modelNow else { return nil }
        let m = shortModel(now)
        return s.tier.map { "\($0) → \(m)" } ?? m
    }
}

/// The row as one element for VoiceOver, saying everything the tooltip says, with Archive and
/// Rename as its actions; while it's being renamed, its text field is reachable instead.
private struct RowAccessibility: ViewModifier {
    let on: Bool
    let label: String
    let archive: (() -> Void)?
    let rename: () -> Void

    func body(content: Content) -> some View {
        if on, let archive {
            content
                .accessibilityElement(children: .combine)
                .accessibilityLabel(label)
                .accessibilityAction(named: "Rename", rename)
                .accessibilityAction(named: "Archive", archive)
        } else if on {
            content
                .accessibilityElement(children: .combine)
                .accessibilityLabel(label)
                .accessibilityAction(named: "Rename", rename)
        } else {
            content
        }
    }
}

/// A session's status as the minimal row shows it: Needs you an orange "!", Working a green dot
/// (a ring while it waits on subagents or commands), Done a blue dot, an error a red dot, Idle
/// nothing. VoiceOver hears the word from the row's label.
struct MinimalMark: View {
    let status: SessionStatus
    /// Its agent's last call failed: a dead agent never looks merely idle.
    var error = false

    var body: some View {
        ZStack {
            switch status {
            case .needsYou:
                Image(systemName: "exclamationmark.circle.fill").foregroundStyle(status.color)
            case .thinking, .working:
                Pulse(color: NSColor(status.color), ring: false, period: 0.7).frame(width: 7, height: 7)
            case .waiting:
                Pulse(color: NSColor(status.color), ring: true, period: 1.4).frame(width: 8, height: 8)
            case .done:
                Circle().fill(error ? SessionStatus.exited.color : status.color).frame(width: 7, height: 7)
            case .exited:
                Circle().fill(status.color).frame(width: 7, height: 7)
            case .idle, .ended:
                if error { Circle().fill(SessionStatus.exited.color).frame(width: 7, height: 7) }
            }
        }
        .frame(width: 14, height: 14)
        .accessibilityHidden(true)
    }
}

/// "Needs you", the one word a minimal row carries.
struct NeedsYouPill: View {
    var body: some View {
        Text("Needs you")
            .font(.caption.weight(.medium))
            .foregroundStyle(SessionStatus.needsYou.color)
            .lineLimit(1)
            .fixedSize()
            .padding(.horizontal, 6).padding(.vertical, 1)
            .background(Capsule().fill(SessionStatus.needsYou.color.opacity(0.15)))
            .accessibilityHidden(true)
    }
}

/// A session's project, small, at the end of its row when grouped by state. The row's menu has the
/// repo's own menu under the repo's name.
struct ProjectTag: View {
    @EnvironmentObject var model: DinoModel
    let place: SessionPlace

    var body: some View {
        Text(place.name)
            .font(.caption)
            .foregroundStyle(.secondary)
            .lineLimit(1)
            .truncationMode(.middle)
            .frame(maxWidth: 110, alignment: .trailing)
            .fixedSize(horizontal: true, vertical: false)
            .padding(.horizontal, 5).padding(.vertical, 1)
            .background(RoundedRectangle(cornerRadius: 4).fill(.quaternary.opacity(0.6)))
            .help(place.node.map { FolderLook.help(repo: $0.isGit, path: $0.repo.path) } ?? place.name)
            .accessibilityHidden(true)
    }
}

/// "● Working  3  ⌄": a section of the sidebar grouped by state. A click opens or closes it, as a
/// repo's row does; → and ← too.
struct StateHeading: View {
    let section: SessionFilter
    let count: Int
    @Binding var open: Bool

    var body: some View {
        HStack(spacing: 6) {
            StatusDot(status: Self.status(section)).allowsHitTesting(false)
            Text(section.label).font(.subheadline.weight(.semibold))
                .foregroundStyle(section == .needsYou && count > 0 ? AnyShapeStyle(SessionStatus.needsYou.color) : AnyShapeStyle(.secondary))
            Text("\(count)").font(.caption.monospacedDigit()).foregroundStyle(.tertiary)
            Spacer()
            Button { open.toggle() } label: {
                Image(systemName: "chevron.right")
                    .font(.caption2.weight(.semibold))
                    .foregroundStyle(.tertiary)
                    .rotationEffect(.degrees(open ? 90 : 0))
                    .frame(width: 16, height: 16)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help(open ? "Collapse" : "Expand")
        }
        .padding(.top, 6)
        .help(section.help)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(section.label), \(count == 1 ? "1 session" : "\(count) sessions")")
        .accessibilityHint(open ? "Collapse" : "Expand")
        .accessibilityAddTraits(.isHeader)
        .accessibilityAction(named: open ? "Collapse" : "Expand") { open.toggle() }
    }

    /// The status the heading's mark shows.
    static func status(_ f: SessionFilter) -> SessionStatus {
        switch f {
        case .needsYou: .needsYou
        case .working: .working
        case .done: .done
        default: .idle
        }
    }
}

/// The sidebar grouped by state: a section for each status, its sessions under it with their
/// project as a tag.
struct StateSections: View {
    @EnvironmentObject var model: DinoModel
    let sessions: [SessionInfo]
    /// The one section the status filter leaves, if it leaves one.
    var only: SessionFilter?
    let tree: (repos: [RepoNode], unfiled: [SessionInfo])
    @Binding var collapsed: Set<String>

    var body: some View {
        let places = SessionPlace.index(tree)
        ForEach(StateSection.split(sessions, status: model.status(of:)).filter { only == nil || $0.0 == only }, id: \.0) { section, list in
            let key = StateSection.tag(section)
            let open = Binding(
                get: { !collapsed.contains(key) },
                set: { o in if o { collapsed.remove(key) } else { collapsed.insert(key) } }
            )
            StateHeading(section: section, count: list.count, open: open).tag(key)
            if open.wrappedValue {
                ForEach(list) { s in
                    let place = places[s.id] ?? SessionPlace(name: s.host ?? "—")
                    SessionRow(session: s, branch: place.worktree.map { _ in model.branch(of: s) } ?? nil, root: place.worktree?.path ?? place.node?.repo.path, place: place)
                        .tag(s.id)
                        .contextMenu {
                            SessionMenu(session: s)
                            if let w = place.worktree {
                                Divider()
                                PlaceMenuItems(place: w)
                            }
                            if let node = place.node {
                                Divider()
                                Menu(node.repo.name) { RepoMenuItems(node: node) }
                            }
                        }
                }
            }
        }
    }
}

// MARK: - Shared menus

/// A repo's own menu: on its row, and on a session's project tag.
struct RepoMenuItems: View {
    @EnvironmentObject var model: DinoModel
    let node: RepoNode

    var body: some View {
        Button("Copy Path") {
            NSPasteboard.general.clearContents()
            NSPasteboard.general.setString(node.repo.path, forType: .string)
        }
        Button("Show in Finder") { NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: node.repo.path) }
        if !node.others.isEmpty {
            Divider()
            CleanUpItems(others: node.others)
        }
    }
}

/// Clean Up Merged takes the ones whose work landed with nothing to lose, at once; Clean Up…
/// every one nothing works in, after saying what would be lost.
struct CleanUpItems: View {
    @EnvironmentObject var model: DinoModel
    let others: [PlaceNode]

    var body: some View {
        let merged = others.filter(\.mergedAndClean)
        let removable = others.filter(\.removable)
        Button(merged.isEmpty ? "Clean Up Merged" : "Clean Up \(merged.count) Merged") { model.cleanWorktrees(merged.map(\.path)) }
            .disabled(merged.isEmpty)
        Button(removable.isEmpty ? "Clean Up…" : "Clean Up \(removable.count)…") { model.cleaningUp = CleanUpPlan(places: removable) }
            .disabled(removable.isEmpty)
    }
}

/// A worktree's menu: close it (dino's) or clean it up, and Show in Finder.
struct PlaceMenuItems: View {
    let place: PlaceNode

    var body: some View {
        WorktreeClosingItems(place: place)
        Button("Show in Finder") { NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: place.path) }
    }
}

extension DinoModel {
    /// The branch of the worktree session `s` is in, as `git` prints it.
    func branch(of s: SessionInfo) -> String? {
        guard s.host == nil, let cwd = s.here else { return nil }
        return repos.flatMap(\.worktrees).filter { SessionTree.contains($0.path, cwd) }.max { $0.path.count < $1.path.count }?
            .branch.map(RepoRows.readable)
    }
}

extension UsingDisplay {
    /// The setting isn't off: rows mark an agent using the Mac.
    static var shown: Bool { UserDefaults.standard.string(forKey: key) != UsingDisplay.off.rawValue }
}
