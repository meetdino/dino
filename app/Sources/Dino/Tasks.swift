import AppKit
import SwiftUI

extension SessionInfo {
    /// Whether its agent reports a task list, subagents or background commands (Claude's hooks).
    var reportsTasks: Bool { agent_id == "claude" || !(tasks?.isEmpty ?? true) }
}

extension DinoModel {
    /// The session the Tasks pane is about: the selected one, else the one it was about before a
    /// folder got selected from it.
    var tasksSession: SessionInfo? {
        selectedSession ?? sessions.first { $0.id == tasksFallback }
    }

    func toggleTasks() {
        if sidePane == .tasks {
            closeSidePane()
            return
        }
        guard openFile?.confirmClose() ?? true else { return }
        openFile?.stop()
        sidePane = .tasks
    }

    /// Select a subagent's worktree row in the sidebar; the pane keeps showing `session`'s tasks.
    func revealWorktree(_ path: String, of session: String) {
        tasksFallback = session
        select("dir:\(path)")
    }

    /// Show what subagent `agent` of `session` is doing; the pane keeps showing `session`'s tasks.
    func showSubagent(_ agent: String, of session: String) {
        tasksFallback = session
        select(SubagentRef.agent(session: session, id: agent).selection)
    }

    /// File → Open File…, starting in `folder`.
    func chooseFile(in folder: String, session: String?) {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = false
        panel.directoryURL = URL(fileURLWithPath: folder)
        guard panel.runModal() == .OK, let url = panel.url else { return }
        openFile(url.path, session: session)
    }
}

struct TasksPane: View {
    @EnvironmentObject var model: DinoModel
    let session: SessionInfo?

    private var tasks: SessionTasks? { session?.tasks }

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            if let session, let tasks, !tasks.isEmpty {
                List {
                    if !tasks.todos.isEmpty { todoSection(tasks.todos) }
                    if !tasks.subagents.isEmpty { subagentSection(tasks.subagents, session: session) }
                    if !tasks.background.isEmpty { backgroundSection(tasks.background) }
                }
                .listStyle(.inset)
                .scrollContentBackground(.hidden)
            } else {
                empty
            }
        }
        .background(Color(nsColor: .textBackgroundColor))
    }

    private var header: some View {
        SidePaneHeader(title: "Tasks", subtitle: session?.display, closeHelp: "Close (⌥⌘T, ⌘W or Esc)", close: { model.closeSidePane() }) {
            Image(systemName: "checklist")
        } trailing: {
            EmptyView()
        }
    }

    private var empty: some View {
        VStack(spacing: 8) {
            Image(systemName: "checklist").font(.system(size: 28)).foregroundStyle(.tertiary)
            if let session, !session.reportsTasks {
                Text("This agent doesn't report its tasks.").foregroundStyle(.secondary)
            } else {
                Text("Nothing yet").foregroundStyle(.secondary)
                Text("The agent's task list, subagents and background commands show here as it works.")
                    .font(.caption).foregroundStyle(.tertiary).multilineTextAlignment(.center)
            }
        }
        .padding(24)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    // MARK: Sections

    private func todoSection(_ todos: [TodoItem]) -> some View {
        let done = todos.filter { $0.status == "completed" }.count
        return Section {
            ForEach(todos) { TodoRow(todo: $0) }
        } header: {
            SectionTitle(title: "Task List", detail: "\(done) of \(todos.count) done")
        }
    }

    private func subagentSection(_ all: [SubagentItem], session: SessionInfo) -> some View {
        let running = all.filter(\.running)
        let finished = all.filter { !$0.running }.reversed()
        return Section {
            ForEach(running) { SubagentRow(agent: $0, session: session.id) }
            if !finished.isEmpty {
                FinishedGroup(key: "subagents:\(session.id)", count: finished.count) {
                    ForEach(Array(finished)) { SubagentRow(agent: $0, session: session.id) }
                }
            }
        } header: {
            SectionTitle(title: "Subagents", detail: running.isEmpty ? nil : "\(running.count) running")
        }
    }

    private func backgroundSection(_ all: [BackgroundItem]) -> some View {
        let running = all.filter(\.running)
        let finished = all.filter { !$0.running }.reversed()
        return Section {
            ForEach(running) { BackgroundRow(item: $0) }
            if !finished.isEmpty {
                FinishedGroup(key: "background:\(session?.id ?? "")", count: finished.count) {
                    ForEach(Array(finished)) { BackgroundRow(item: $0) }
                }
            }
        } header: {
            SectionTitle(title: "In the Background", detail: running.isEmpty ? nil : "\(running.count) running")
        }
    }
}

private struct SectionTitle: View {
    let title: String
    let detail: String?

    var body: some View {
        HStack {
            Text(title)
            Spacer()
            if let detail { Text(detail).foregroundStyle(.secondary).fontWeight(.regular) }
        }
    }
}

/// Finished work, folded away once there's more than a little of it.
private struct FinishedGroup<Content: View>: View {
    let key: String
    let count: Int
    @ViewBuilder let content: () -> Content
    @State private var open: Bool?

    var body: some View {
        DisclosureGroup(isExpanded: Binding(get: { open ?? (count <= 3) }, set: { open = $0 })) {
            content()
        } label: {
            Text("Finished (\(count))").foregroundStyle(.secondary)
        }
    }
}

private struct TodoRow: View {
    let todo: TodoItem

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            switch todo.status {
            case "completed":
                Image(systemName: "checkmark.circle.fill").foregroundStyle(.secondary)
                Text(todo.subject).strikethrough().foregroundStyle(.secondary)
            case "in_progress":
                Image(systemName: "circle.lefthalf.filled").foregroundStyle(Brand.green)
                Text(todo.active ?? todo.subject).fontWeight(.medium)
            default:
                Image(systemName: "circle").foregroundStyle(.tertiary)
                Text(todo.subject)
            }
        }
        .padding(.vertical, 1)
        .help(todo.subject)
    }
}

private struct SubagentRow: View {
    @EnvironmentObject var model: DinoModel
    let agent: SubagentItem
    let session: String

    var body: some View {
        HStack(alignment: .center, spacing: 8) {
            // Shows what it's doing; one in a worktree of its own through that worktree's sidebar row.
            Button {
                if let wt = agent.worktree {
                    model.revealWorktree(wt, of: session)
                } else {
                    model.showSubagent(agent.id, of: session)
                }
            } label: {
                HStack(alignment: .center, spacing: 8) {
                    Group {
                        if agent.running {
                            ProgressView().controlSize(.small)
                        } else {
                            Image(systemName: "checkmark").foregroundStyle(.secondary)
                        }
                    }
                    .frame(width: 16)
                    VStack(alignment: .leading, spacing: 1) {
                        Text(agent.description ?? agent.agent_type ?? "Subagent").lineLimit(2)
                        HStack(spacing: 4) {
                            if let t = agent.agent_type { Text(t) }
                            if agent.worktree != nil {
                                Text("·")
                                Image(systemName: "arrow.triangle.branch")
                                Text("own worktree")
                            }
                        }
                        .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                    }
                    Spacer(minLength: 6)
                    Elapsed(started: agent.started, finished: agent.finished, running: agent.running)
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help(agent.worktree.map { "Runs in \(shortPath($0)). Click to see what it's doing" } ?? "Click to see what it's doing")
            if let wt = agent.worktree { WorktreeMenu(path: wt, session: session) }
        }
        .padding(.vertical, 2)
        .contextMenu {
            if let wt = agent.worktree { WorktreeMenuItems(path: wt, session: session) }
            Button("Copy Agent ID") { copy(agent.id) }
        }
    }
}

/// What to do with a subagent's worktree.
private struct WorktreeMenu: View {
    let path: String
    let session: String

    var body: some View {
        Menu {
            WorktreeMenuItems(path: path, session: session)
        } label: {
            Image(systemName: "folder")
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .help("Its worktree: open a file from it, find it in the sidebar, or open it in another app")
    }
}

struct WorktreeMenuItems: View {
    @EnvironmentObject var model: DinoModel
    let path: String
    let session: String
    var inSidebar = true

    var body: some View {
        Button("Open a File from Its Worktree…") { model.chooseFile(in: path, session: session) }
        if inSidebar { Button("Show in Sidebar") { model.revealWorktree(path, of: session) } }
        if let editor = ExternalEditor.preferred(for: path) {
            Button("Open in \(editor.name)") { editor.openFolder(path) }
        }
        Button("Show in Finder") { NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: path) }
    }
}

private struct BackgroundRow: View {
    let item: BackgroundItem

    var body: some View {
        HStack(alignment: .center, spacing: 8) {
            Group {
                if item.running {
                    ProgressView().controlSize(.small)
                } else {
                    Image(systemName: item.kind == "monitor" ? "eye.slash" : "checkmark").foregroundStyle(.secondary)
                }
            }
            .frame(width: 16)
            VStack(alignment: .leading, spacing: 1) {
                Text(item.description ?? item.command ?? item.id).lineLimit(2)
                if item.description != nil, let c = item.command {
                    Text(c).font(.system(.caption, design: .monospaced)).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle)
                } else if item.kind == "monitor" {
                    Text("Monitor").font(.caption).foregroundStyle(.secondary)
                }
            }
            Spacer(minLength: 6)
            Elapsed(started: item.started, finished: item.finished, running: item.running)
        }
        .padding(.vertical, 2)
        .contextMenu {
            if let c = item.command { Button("Copy Command") { copy(c) } }
        }
        .help(item.command ?? "")
    }
}

/// How long it has run, ticking while it runs; how long it took once it's done.
private struct Elapsed: View {
    let started: UInt64
    let finished: UInt64?
    let running: Bool

    var body: some View {
        if started > 0 {
            if running {
                TimelineView(.periodic(from: .now, by: 1)) { ctx in
                    label(UInt64(max(0, ctx.date.timeIntervalSince1970)))
                }
            } else if let finished {
                label(finished)
            }
        }
    }

    private func label(_ end: UInt64) -> some View {
        Text(duration(end > started ? end - started : 0))
            .font(.caption.monospacedDigit())
            .foregroundStyle(.secondary)
    }
}

func duration(_ secs: UInt64) -> String {
    switch secs {
    case ..<60: "\(secs)s"
    case ..<3600: "\(secs / 60)m \(String(format: "%02d", secs % 60))s"
    default: "\(secs / 3600)h \(String(format: "%02d", secs % 3600 / 60))m"
    }
}

private func copy(_ s: String) {
    NSPasteboard.general.clearContents()
    NSPasteboard.general.setString(s, forType: .string)
}
