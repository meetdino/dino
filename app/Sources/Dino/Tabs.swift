import SwiftUI

// Two layers, as agreed for dino: the tabs along the top are what's open (every shell, and the
// agents you opened), like Ghostty's; the sidebar is every agent, open or not. Closing an agent's
// tab only closes the view: it keeps running in the sidebar.

extension SessionInfo {
    /// A shell with no agent in it: it lives in the tabs only, not in the sidebar.
    var plainShell: Bool { agent_id == "shell" && inside == nil }
}

extension DinoModel {
    /// The sessions the sidebar lists: agents, including one started by hand in a shell.
    var sidebarSessions: [SessionInfo] { sessions.filter { !$0.plainShell } }

    /// Drop tabs whose session is gone and give every new shell one, wherever it was started
    /// (⌘T, `dino .`, an agent's tool). Called with the sessions dinod reports.
    func syncTabs(_ next: [SessionInfo]) {
        let live = Set(next.map(\.id))
        var t = tabs.filter { live.contains($0) }
        for s in next where s.agent_id == "shell" && !t.contains(s.id) && !knownTabless.contains(s.id) {
            t.append(s.id)
        }
        knownTabless = knownTabless.intersection(live)
        if t != tabs { tabs = t }
    }

    /// A tab for session `id`, right after the current one, unless it has one.
    func openTab(_ id: String) {
        guard !id.contains(":"), !tabs.contains(id) else { return }
        let at = selected.flatMap { tabs.firstIndex(of: $0) }.map { $0 + 1 } ?? tabs.endIndex
        tabs.insert(id, at: at)
        knownTabless.remove(id)
    }

    /// The tabs as shown: a split is one tab, under whichever of its two comes first.
    var shownTabs: [String] {
        var seen = Set<String>()
        return tabs.filter { id in
            guard !seen.contains(id) else { return false }
            if let split = splits.first(where: { $0.contains(id) }) { seen.formUnion([split.first, split.second]) }
            seen.insert(id)
            return true
        }
    }

    /// The tab showing `id`: its own, or its split's.
    func tab(of id: String?) -> String? {
        guard let id else { return nil }
        if let split = splits.first(where: { $0.contains(id) }) {
            return shownTabs.first { $0 == split.first || $0 == split.second }
        }
        return shownTabs.contains(id) ? id : nil
    }

    /// ⌘⇧] and ⌘⇧[, wrapping.
    func cycleTabs(by step: Int) {
        let shown = shownTabs
        guard !shown.isEmpty else { return }
        let at = tab(of: selected).flatMap { shown.firstIndex(of: $0) } ?? (step > 0 ? -1 : 0)
        select(shown[((at + step) % shown.count + shown.count) % shown.count])
    }

    /// Close tab `id` without asking: a shell ends, an agent only leaves the tabs. The next tab
    /// along takes over, as in Ghostty.
    func dropTab(_ id: String, ending: Bool) {
        let shown = shownTabs
        let at = shown.firstIndex(of: tab(of: id) ?? id) ?? 0
        let members = splits.first(where: { $0.contains(id) }).map { [$0.first, $0.second] } ?? [id]
        tabs.removeAll { members.contains($0) }
        if ending {
            for m in members { kill(m) }
        } else {
            // A shell left without a tab would come straight back on the next sync.
            knownTabless.formUnion(members)
        }
        let rest = shownTabs
        if rest.isEmpty {
            selected = nil
        } else if members.contains(selected ?? "") || selected == nil {
            select(rest[min(at, rest.count - 1)])
        }
    }
}

/// The tabs along the top of the terminal area.
struct TabStrip: View {
    @EnvironmentObject var model: DinoModel

    var body: some View {
        let current = model.tab(of: model.selected)
        HStack(spacing: 0) {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 1) {
                    ForEach(model.shownTabs, id: \.self) { id in
                        if let s = model.sessions.first(where: { $0.id == id }) {
                            TabItem(session: s, selected: id == current)
                        }
                    }
                }
            }
            Button { model.newShell() } label: {
                Image(systemName: "plus").frame(width: 28, height: 26)
            }
            .buttonStyle(.plain)
            .foregroundStyle(.secondary)
            .help("New tab: a shell in \(shortPath(model.folder.path)) (⌘T)")
        }
        .frame(height: 28)
        .background(.bar)
        .overlay(alignment: .bottom) { Divider() }
    }
}

private struct TabItem: View {
    @EnvironmentObject var model: DinoModel
    let session: SessionInfo
    let selected: Bool
    @State private var hovering = false

    var body: some View {
        let split = model.splits.first { $0.contains(session.id) }
        let partner = split.flatMap { s in model.sessions.first { $0.id == s.other(session.id) } }
        let status = model.status(of: session)
        HStack(spacing: 6) {
            if !session.plainShell {
                Circle().fill(status.color).frame(width: 6, height: 6)
            }
            Text(partner.map { "\(model.tabName(session)) | \(model.tabName($0))" } ?? model.tabName(session))
                .lineLimit(1)
                .truncationMode(.middle)
            let unseen = model.tmuxUnseen(session)
            if !unseen.isEmpty {
                Text("\(unseen.count)")
                    .font(.caption2.monospacedDigit().weight(.semibold))
                    .padding(.horizontal, 5)
                    .background(Capsule().fill(SessionStatus.needsYou.color.opacity(0.25)))
                    .help(unseen.map(\.text).joined(separator: "\n"))
            }
            Button { model.closeTab(session) } label: {
                Image(systemName: "xmark").font(.system(size: 9, weight: .semibold))
            }
            .buttonStyle(.plain)
            .opacity(hovering || selected ? 1 : 0)
            .help(session.tmux != nil ? "Detach tmux and close this tab; tmux keeps everything (⌘W)"
                : session.plainShell ? "Close this tab (⌘W)" : "Close this tab; \(session.display) keeps running in the sidebar (⌘W)")
        }
        .font(.callout)
        .foregroundStyle(selected ? .primary : .secondary)
        .padding(.horizontal, 10)
        .frame(minWidth: 90, maxWidth: 220, minHeight: 27)
        .background(selected ? Color(nsColor: .textBackgroundColor) : .clear)
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
        .onTapGesture { model.select(session.id) }
        .help(session.here.map(shortPath) ?? session.display)
    }
}

extension DinoModel {
    /// Bells and notifications from the tmux in shell `s` not seen yet: seen once its tab is looked at.
    func tmuxUnseen(_ s: SessionInfo) -> [TmuxAlert] {
        let seen = tmuxSeen[s.id] ?? 0
        return (s.tmux?.alerts ?? []).filter { $0.seq > seen }
    }

    /// New bells and notifications from a tmux: one notification for the tab, saying how many and
    /// where each came from, replacing the last one instead of piling up. Called with the sessions
    /// dinod reports, before they replace `sessions`. Returns the tabs that now need you.
    func noteTmux(_ next: [SessionInfo], looking: (SessionInfo) -> Bool) -> [String] {
        var needs: [String] = []
        for s in next {
            guard let t = s.tmux, let newest = t.alerts?.last?.seq else { continue }
            if looking(s) {
                if tmuxSeen[s.id] != newest { tmuxSeen[s.id] = newest }
                continue
            }
            let before = sessions.first { $0.id == s.id }?.tmux?.alerts?.last?.seq ?? 0
            guard newest > before else { continue }
            let unseen = tmuxUnseen(s)
            guard !unseen.isEmpty else { continue }
            let tmuxSession = t.target.split(separator: ":").first.map(String.init) ?? t.label
            let title = unseen.count == 1 ? "tmux \(tmuxSession)" : "\(unseen.count) notifications in tmux \(tmuxSession)"
            needs.append(s.id)
            Notifier.post(session: s, title: title, body: unseen.map(\.text).joined(separator: "\n"))
        }
        return needs
    }
}
