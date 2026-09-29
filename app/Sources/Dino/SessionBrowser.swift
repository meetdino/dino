import SwiftUI

/// Every agent session on this Mac and in the cloud: running elsewhere, idle, done. Pick one to
/// read its conversation, then resume or move it into dino.
struct ContinueSheet: View {
    @EnvironmentObject var model: DinoModel
    @Environment(\.dismiss) private var dismiss
    @State private var query = ""
    @State private var agent = "all"
    @State private var folder: String?
    @State private var selection: String?
    @State private var doneShown = 50

    private func matches(_ f: FoundSession) -> Bool {
        if agent != "all" && f.agent != agent { return false }
        if let folder, f.cwd != folder { return false }
        guard !query.isEmpty else { return true }
        return [f.title, f.cwd ?? "", f.terminal ?? "", f.session_id].contains { $0.localizedCaseInsensitiveContains(query) }
    }

    private var groups: [Bucket] {
        let all = model.found.filter(matches)
        return [
            Bucket(id: "running", title: "Running", note: "Busy in another terminal; moves here when its turn ends",
                  items: all.filter { $0.source == "running" && $0.isBusy }),
            Bucket(id: "idle", title: "Idle", note: "Open in another terminal, waiting",
                  items: all.filter { $0.source == "running" && !$0.isBusy }),
            Bucket(id: "done", title: "Done", note: nil, items: all.filter { $0.source == "recent" }),
            Bucket(id: "cloud", title: "Cloud", note: model.loadingCloud ? "Checking…" : nil,
                  items: all.filter { $0.source == "cloud" }),
        ]
    }

    private var visible: [FoundSession] {
        groups.flatMap { $0.id == "done" ? Array($0.items.prefix(doneShown)) : $0.items }
    }

    private var selected: FoundSession? { model.found.first { $0.id == selection } }

    /// Folders with sessions, most recently used first.
    private var folders: [String] {
        var seen = Set<String>()
        return model.found.sorted { $0.updated_at > $1.updated_at }.compactMap(\.cwd).filter { seen.insert($0).inserted }
    }

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            HStack(spacing: 0) {
                list.frame(width: 390)
                Divider()
                if let f = selected {
                    SessionDetail(session: f).id(f.id)
                } else {
                    Text(visible.isEmpty ? "" : "Select a session to read it")
                        .foregroundStyle(.secondary).frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }
            Divider()
            footer
        }
        .frame(width: 1040, height: 660)
        .onAppear { if selection == nil { selection = visible.first?.id } }
        .onChange(of: visible.map(\.id)) { _, ids in
            if selection.map({ !ids.contains($0) }) ?? true { selection = ids.first }
        }
    }

    private var header: some View {
        HStack(spacing: 10) {
            Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
            TextField("Search sessions by title, folder or id", text: $query)
                .textFieldStyle(.plain).font(.title3)
            Picker("Agent", selection: $agent) {
                Text("All").tag("all")
                Text("Claude").tag("claude")
                Text("Codex").tag("codex")
            }
            .pickerStyle(.segmented).labelsHidden().fixedSize()
            Menu {
                Button("Any folder") { folder = nil }
                Divider()
                ForEach(folders.prefix(30), id: \.self) { f in
                    Button(shortPath(f)) { folder = f }
                }
            } label: {
                Label(folder.map { URL(fileURLWithPath: $0).lastPathComponent } ?? "Any folder", systemImage: "folder")
            }
            .menuStyle(.borderlessButton).fixedSize()
            .help(folder.map(shortPath) ?? "Only sessions in one folder")
        }
        .padding(.horizontal, 14).padding(.vertical, 11)
    }

    private var list: some View {
        List(selection: $selection) {
            ForEach(groups) { g in
                let items = g.id == "done" ? Array(g.items.prefix(doneShown)) : g.items
                if !items.isEmpty || (g.id == "cloud" && model.loadingCloud) || (g.id == "done" && !model.loadedHistory) {
                    Section {
                        ForEach(items) { f in
                            FoundRow(session: f).tag(f.id).contextMenu { menu(f) }
                        }
                        if g.id == "done" && !model.loadedHistory {
                            HStack(spacing: 6) { ProgressView().controlSize(.small); Text("Reading conversations…").foregroundStyle(.secondary) }
                        }
                        if g.items.count > items.count {
                            Button("Show \(min(100, g.items.count - items.count)) more of \(g.items.count - items.count)") { doneShown += 100 }
                                .buttonStyle(.link).font(.caption)
                        }
                    } header: {
                        HStack(spacing: 6) {
                            Text(g.title)
                            Text("\(g.items.count)").foregroundStyle(.tertiary).monospacedDigit()
                            if let note = g.note { Text(note).foregroundStyle(.tertiary).font(.caption).lineLimit(1) }
                        }
                    }
                }
            }
        }
        .listStyle(.sidebar)
        .overlay {
            if visible.isEmpty && model.loadedHistory {
                Text(query.isEmpty && folder == nil ? "No sessions found" : "Nothing matches").foregroundStyle(.secondary)
            }
        }
    }

    @ViewBuilder
    private func menu(_ f: FoundSession) -> some View {
        Button(f.source == "running" ? "Move to dino…" : "Resume in dino") { continueIn(f) }
        if let url = f.url.flatMap(URL.init(string:)) {
            Button("Open in Browser") { NSWorkspace.shared.open(url) }
        }
        if !f.session_id.isEmpty {
            Button("Copy Session ID") {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(f.session_id, forType: .string)
            }
        }
        if let cwd = f.cwd {
            Button("Only This Folder") { folder = cwd }
        }
    }

    private func continueIn(_ f: FoundSession) {
        if f.source == "running" {
            model.showContinue = false
            model.confirmMove = f
        } else {
            model.adopt(f)
        }
    }

    private var footer: some View {
        HStack {
            let local = model.found.filter { $0.source != "cloud" }.count
            Text("\(local) on this Mac · ↑↓ browse · ↵ \(selected?.source == "running" ? "move here" : "resume") · esc close")
                .font(.caption).foregroundStyle(.secondary)
            Spacer()
            Button("Close") { dismiss() }.keyboardShortcut(.cancelAction)
        }
        .padding(10)
    }

    struct Bucket: Identifiable {
        let id: String
        let title: String
        let note: String?
        let items: [FoundSession]
    }
}

struct FoundRow: View {
    let session: FoundSession

    var body: some View {
        HStack(spacing: 8) {
            AgentBadge(agent: session.agent)
            VStack(alignment: .leading, spacing: 2) {
                Text(session.title).lineLimit(1)
                Text(detail).font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.head)
            }
            Spacer(minLength: 4)
            if session.isBusy {
                Circle().fill(SessionStatus.working.color).frame(width: 6, height: 6).help("Working")
            }
            Text(ago(session.updated_at)).font(.caption.monospacedDigit()).foregroundStyle(.tertiary)
        }
        .padding(.vertical, 2)
    }

    private var detail: String {
        var parts: [String] = []
        if let cwd = session.cwd { parts.append(shortPath(cwd)) }
        if let t = session.terminal { parts.append(session.source == "cloud" ? t : "in \(t)") }
        if session.source == "cloud", let s = session.status { parts.append(s) }
        return parts.joined(separator: " · ")
    }
}

/// The selected session: what it is, its conversation, and what can be done with it.
private struct SessionDetail: View {
    @EnvironmentObject var model: DinoModel
    let session: FoundSession
    @State private var path: String?

    private var state: (String, Color) {
        switch session.source {
        case "running": session.isBusy ? ("Running", SessionStatus.working.color) : ("Idle", .secondary)
        case "cloud": (session.status ?? "Cloud", .blue)
        default: ("Done", SessionStatus.done.color)
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: 6) {
                Text(session.title).font(.title3.weight(.semibold)).lineLimit(2).textSelection(.enabled)
                HStack(spacing: 6) {
                    AgentBadge(agent: session.agent)
                    Text(state.0).font(.caption.weight(.medium)).foregroundStyle(state.1)
                    Text(facts).font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle)
                }
                HStack(spacing: 8) {
                    actions
                    Spacer()
                    if !session.session_id.isEmpty {
                        Text(session.session_id).font(.caption2.monospaced()).foregroundStyle(.tertiary).textSelection(.enabled).lineLimit(1)
                    }
                }
                .padding(.top, 4)
            }
            .padding(14)
            Divider()
            if session.source == "cloud" {
                CloudNote(session: session)
            } else {
                SessionPreview(session: session, path: $path)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private var facts: String {
        var parts: [String] = []
        if let cwd = session.cwd { parts.append(shortPath(cwd)) }
        if session.source == "running", let t = session.terminal { parts.append("in \(t)") }
        if let pid = session.pid { parts.append("pid \(pid)") }
        let when = ago(session.updated_at)
        if !when.isEmpty { parts.append(when) }
        return parts.joined(separator: " · ")
    }

    @ViewBuilder
    private var actions: some View {
        switch session.source {
        case "running":
            Button("Move Here") {
                model.showContinue = false
                model.confirmMove = session
            }
            .keyboardShortcut(.defaultAction)
            .help(session.isBusy ? "Waits for its current turn, closes it in \(session.terminal ?? "its terminal") and continues it in dino" : "Closes it in \(session.terminal ?? "its terminal") and continues it in dino")
        case "cloud":
            if let url = session.url.flatMap(URL.init(string:)) {
                Button("Open in Browser") { NSWorkspace.shared.open(url) }.keyboardShortcut(.defaultAction)
            }
            Button(session.session_id.isEmpty ? "Pick a Web Session…" : "Continue in dino") { model.adopt(session) }
        default:
            Button("Resume Here") { model.adopt(session) }
                .keyboardShortcut(.defaultAction)
                .help("Continues this conversation as a dino session in \(session.cwd.map(shortPath) ?? "your home folder")")
        }
        if let path {
            Button {
                NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: path)])
            } label: {
                Image(systemName: "doc.text.magnifyingglass")
            }
            .help("Show the transcript file in Finder")
        }
    }
}

/// A local session's conversation, read from its transcript. Running ones follow along.
private struct SessionPreview: View {
    let session: FoundSession
    @Binding var path: String?
    @State private var page: ConversationPage?
    @State private var failed: String?
    @State private var loadingEarlier = false

    var body: some View {
        SwiftUI.Group {
            if let page, !page.turns.isEmpty || page.start > 0 {
                ConversationView(
                    turns: page.turns, agent: session.agent,
                    earlier: page.start > 0 ? { loadEarlier() } : nil,
                    loadingEarlier: loadingEarlier)
            } else if page != nil {
                Text("Nothing said yet").foregroundStyle(.secondary).frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if let failed {
                Text(failed).foregroundStyle(.secondary).multilineTextAlignment(.center).padding().frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .task {
            await load()
            // A live session keeps talking; follow it unless earlier parts were loaded.
            while session.source == "running" && !Task.isCancelled {
                try? await Task.sleep(for: .seconds(3))
                if !Task.isCancelled && !loadingEarlier && (page?.start ?? 0) == (latestStart ?? 0) { await load() }
            }
        }
    }

    /// Where the newest page began; if `page` starts earlier, the user loaded more.
    @State private var latestStart: UInt64?

    private func fetch(before: UInt64?) async -> Result<ConversationPage, Error> {
        let (agent, id) = (session.agent, session.session_id)
        return await Task.detached {
            Result { try DinoConnection(path: DinoEnvironment.socketPath).conversation(agent: agent, sessionID: id, before: before) }
        }.value
    }

    private func load() async {
        switch await fetch(before: nil) {
        case .success(let p):
            latestStart = p.start
            if p != page { page = p }
            path = p.path
        case .failure(let e):
            if page == nil { failed = e.localizedDescription }
        }
    }

    private func loadEarlier() {
        guard let page, !loadingEarlier else { return }
        loadingEarlier = true
        Task {
            if case .success(let p) = await fetch(before: page.start) {
                self.page = ConversationPage(turns: p.turns + page.turns, start: p.start, path: page.path)
            }
            loadingEarlier = false
        }
    }
}

/// Cloud sessions have no transcript here: what the provider says, and where to see it.
private struct CloudNote: View {
    let session: FoundSession

    var body: some View {
        VStack(spacing: 10) {
            Image(systemName: "cloud").font(.system(size: 30)).foregroundStyle(.tertiary)
            if session.session_id.isEmpty {
                Text("Claude Code on the web keeps its sessions in the cloud. Continuing opens the picker, and the one you choose is brought into a checkout here.")
            } else {
                Text("This task runs in \(session.agent == "codex" ? "Codex" : "Claude")'s cloud; its conversation is read there.")
                if let url = session.url, let u = URL(string: url) {
                    Link(url, destination: u).font(.caption)
                }
            }
        }
        .foregroundStyle(.secondary).multilineTextAlignment(.center)
        .frame(maxWidth: 380).frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}
