import SwiftUI

// Two layers, as agreed for dino: the tabs along the top are what's open (every shell, and the
// agents you opened), like Ghostty's; the sidebar is every agent, open or not. Closing a tab closes
// what's in it, as in a terminal: a shell ends, an agent is archived (Archived has it to resume).

extension SessionInfo {
    /// A shell with no agent in it: it lives in the tabs only, not in the sidebar.
    var plainShell: Bool { agent_id == "shell" && inside == nil }

    /// It says how it's doing itself: hooks or its agent's own signal (working, done, needs you),
    /// or, typed into a shell, the agent's own status. A bell from it isn't a question then.
    var reportsStatus: Bool { activity != nil || needs != nil || inside?.status != nil }
}

extension DinoModel {
    /// The sessions the sidebar lists: agents, including one started by hand in a shell.
    var sidebarSessions: [SessionInfo] { sessions.filter { !$0.plainShell } }

    /// Drop tabs whose session is gone and give every new shell one, wherever it was started
    /// (⌘T, `dino .`, an agent's tool). Called with the sessions dinod reports.
    func syncTabs(_ next: [SessionInfo]) {
        let live = Set(next.map(\.id))
        var t = tabs.filter { live.contains($0) }
        for s in next where s.agent_id == "shell" && !t.contains(s.id) {
            t.append(s.id)
        }
        if t != tabs { tabs = t }
    }

    /// A tab for session `id`, right after the current one, unless it has one.
    func openTab(_ id: String) {
        guard !id.contains(":"), !tabs.contains(id) else { return }
        let at = selected.flatMap { tabs.firstIndex(of: $0) }.map { $0 + 1 } ?? tabs.endIndex
        tabs.insert(id, at: at)
    }

    /// The tabs as shown: a split is one tab, under whichever of its two comes first.
    var shownTabs: [String] {
        var seen = Set<String>()
        return tabs.filter { id in
            guard !seen.contains(id) else { return false }
            if let split = split(of: id) { seen.formUnion(split.panes) }
            seen.insert(id)
            return true
        }
    }

    /// The tab showing `id`: its own, or its split's.
    func tab(of id: String?) -> String? {
        guard let id else { return nil }
        if let split = split(of: id) {
            return shownTabs.first { split.contains($0) }
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

    /// Close tab `id` without asking: its shells end (⌘Z brings them back for a while) and, with
    /// `archiving`, its agents are archived; otherwise they only leave the tabs. The next tab along
    /// takes over, as in Ghostty.
    func dropTab(_ id: String, archiving: Bool = false) {
        let shown = shownTabs
        let at = shown.firstIndex(of: tab(of: id) ?? id) ?? 0
        let members = split(of: id)?.panes ?? [id]
        let before = layoutBefore()
        tabs.removeAll { members.contains($0) }
        let inTab = members.compactMap { m in sessions.first { $0.id == m } }
        let shells = inTab.filter { $0.agent_id == "shell" }.map(\.id)
        let agents = archiving ? inTab.filter { $0.agent_id != "shell" }.map(\.id) : []
        // ⌘Z puts it back: a shell is kept by dinod until the time to undo is up. An archived
        // agent comes back from Archived instead.
        if agents.count < inTab.count {
            closed(since: before, members: members, shells: shells, name: "Close Tab") { [weak self] in
                self?.dropTab(id, archiving: archiving)
            }
        }
        for m in shells { endShell(m) }
        let rest = shownTabs
        if rest.isEmpty {
            selected = nil
        } else if members.contains(selected ?? "") || selected == nil {
            select(rest[min(at, rest.count - 1)])
        }
        for a in agents { archiveNow(a) }
    }
}

/// The tabs along the top of the terminal area.
struct TabStrip: View {
    @EnvironmentObject var model: DinoModel
    @Environment(\.colorScheme) private var colorScheme

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
                Image(systemName: "plus").font(.system(size: 11)).frame(width: 24, height: 22)
            }
            .buttonStyle(.plain)
            .foregroundStyle(.secondary)
            .help("New tab: a shell in \(shortPath(model.folder.path)) (⌘T)")
        }
        .frame(height: 22)
        .background {
            // In light mode the bar is near white, as the selected tab is: a shade darker, so the
            // tab you're on stands out, as in Safari and Xcode. Dark mode's contrast is already there.
            ZStack {
                Rectangle().fill(.bar)
                if colorScheme == .light { Color.black.opacity(0.06) }
            }
        }
        .overlay(alignment: .bottom) { Divider() }
    }
}

private struct TabItem: View {
    @EnvironmentObject var model: DinoModel
    let session: SessionInfo
    let selected: Bool
    @State private var hovering = false

    /// The agent this tab is: its own, or one typed into the shell.
    private var tabAgent: String? {
        if let inside = session.inside { return inside.agent }
        return session.agent_id == "shell" ? nil : session.agent_id
    }

    var body: some View {
        // A split's tab names each of its panes.
        let panes = model.split(of: session.id).map { t in t.panes.compactMap { id in model.sessions.first { $0.id == id } } } ?? []
        let status = model.status(of: session)
        HStack(spacing: 5) {
            if !session.plainShell {
                Circle().fill(status.color).frame(width: 5, height: 5)
            }
            BellTitleMark(signal: PaneSignals.of(session.id))
            Text(panes.count > 1 ? panes.map { model.tabName($0) }.joined(separator: " | ") : model.tabName(session))
                .lineLimit(1)
                .truncationMode(.middle)
            let unseen = model.tmuxUnseen(session)
            if !unseen.isEmpty {
                Text("\(unseen.count)")
                    .font(.caption2.monospacedDigit().weight(.semibold))
                    .padding(.horizontal, 4)
                    .background(Capsule().fill(SessionStatus.needsYou.color.opacity(0.25)))
                    .help(unseen.map(\.text).joined(separator: "\n"))
            }
            Button { model.closeTab(session) } label: {
                Image(systemName: "xmark").font(.system(size: 8, weight: .semibold))
                    .frame(width: 16, height: 16).contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .opacity(hovering || selected ? 1 : 0)
            .help(session.tmux != nil ? "Close this tab and detach from tmux (⌘W). The session keeps running in tmux."
                : session.agent_id == "shell" ? "Close this tab (⌘W)" : "Close this tab (⌘W). \(session.display) stops and is archived, to resume from Archived.")
        }
        .font(.subheadline)
        .foregroundStyle(selected ? .primary : .secondary)
        .padding(.leading, 9).padding(.trailing, 4)
        .frame(minWidth: 80, maxWidth: 200, minHeight: 22)
        .background(selected ? Color(nsColor: .textBackgroundColor) : .clear)
        // An agent's tab carries its colour along the top, as its chip in the sidebar; shells don't.
        .overlay(alignment: .top) {
            if let agent = tabAgent {
                Rectangle().fill(AgentBadge.color(agent).opacity(selected ? 0.8 : 0.4)).frame(height: 1.5)
            }
        }
        // A program's progress along the bottom, over the tab: nothing moves when it comes or goes.
        .overlay(alignment: .bottom) { TabProgress(signal: PaneSignals.of(session.id)) }
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
        .onTapGesture { model.select(session.id) }
        .help([tabAgent.map { model.launcherLabel($0) }, session.here.map(shortPath) ?? session.display].compactMap { $0 }.joined(separator: " · "))
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

/// An agent in a tmux pane nobody is attached to: what the pane shows, read-only and kept current.
/// tmux keeps the agent; "Continue in dino" moves it over.
struct TmuxLook: View {
    @EnvironmentObject var model: DinoModel
    @Environment(\.dismiss) private var dismiss
    let session: FoundSession
    @State private var screen = ""
    @State private var gone = false

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 6) {
                AgentBadge(agent: session.agent)
                Text(session.title).font(.headline).lineLimit(1)
                Spacer()
                Text(session.tmux.map { "tmux \($0.label) · no client attached" } ?? "").font(.caption).foregroundStyle(.secondary)
            }
            ScrollView([.vertical, .horizontal]) {
                Text(gone ? "That tmux pane is gone." : screen)
                    .font(.system(.callout, design: .monospaced))
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(8)
            }
            .background(Color(nsColor: .textBackgroundColor), in: RoundedRectangle(cornerRadius: 6))
            HStack {
                Text("Attach to its tmux session to type in it.").font(.caption).foregroundStyle(.secondary)
                Spacer()
                Button("Continue in dino…") {
                    dismiss()
                    model.confirmMove = session
                }
                Button("Done") { dismiss() }.keyboardShortcut(.defaultAction)
            }
        }
        .padding(16)
        .frame(width: 760, height: 520)
        .task {
            guard let place = session.tmux else { return }
            while !Task.isCancelled {
                let text = try? await Task.detached { try DinoConnection(path: DinoEnvironment.socketPath).tmuxScreen(place) }.value
                if let text { if text != screen { screen = text } } else if !gone { gone = true }
                try? await Task.sleep(for: .seconds(1))
            }
        }
    }
}

/// Once, for someone whose tabs are mostly tmux: dino can show their agents in it and open new
/// tabs in it (Settings → tmux). Gone for good once answered either way.
struct TmuxSuggestion: View {
    @EnvironmentObject var model: DinoModel
    @AppStorage("tmux.suggested") private var answered = false
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        let shells = model.sessions.filter { $0.agent_id == "shell" && !$0.exited }
        let inTmux = shells.filter { $0.tmux != nil }.count
        if !answered, !model.tmuxOptionsOn, inTmux >= 2, inTmux * 2 > shells.count {
            HStack(spacing: 8) {
                Image(systemName: "rectangle.split.3x1").foregroundStyle(.secondary)
                Text("You use tmux. dino can show your agents as tmux windows and open new tabs in tmux.")
                    .lineLimit(1)
                    .truncationMode(.tail)
                Spacer(minLength: 8)
                Button("Set Up…") {
                    answered = true
                    SettingsPane.tmux.select()
                    openWindow(id: SettingsView.windowID)
                }
                Button("Not Now") { answered = true }
            }
            .font(.callout)
            .controlSize(.small)
            .padding(.horizontal, 10)
            .padding(.vertical, 5)
            .background(.bar)
            .overlay(alignment: .bottom) { Divider() }
        }
    }
}
