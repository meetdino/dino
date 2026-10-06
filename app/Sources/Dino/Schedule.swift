import AppKit
import SwiftUI

/// When a scheduled task runs (see `Frequency` in crates/dino-core/src/schedule.rs). dinod
/// reads only the fields its kind needs, so all of them travel.
struct Frequency: Codable, Equatable {
    /// "manual", "hourly", "daily", "weekdays" or "weekly".
    var every = "daily"
    var hour = 9
    var minute = 0
    /// 0 is Sunday.
    var weekday = 1

    static let kinds: [(id: String, label: String)] = [
        ("manual", "Manually"), ("hourly", "Hourly"), ("daily", "Daily"), ("weekdays", "Weekdays"), ("weekly", "Weekly"),
    ]

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        every = try c.decode(String.self, forKey: .every)
        hour = try c.decodeIfPresent(Int.self, forKey: .hour) ?? 9
        minute = try c.decodeIfPresent(Int.self, forKey: .minute) ?? 0
        weekday = try c.decodeIfPresent(Int.self, forKey: .weekday) ?? 1
    }

    var label: String {
        switch every {
        case "manual": "Manually"
        case "hourly": String(format: "Every hour at :%02d", minute)
        case "weekdays": "Weekdays at \(time)"
        case "weekly": "\(Calendar.current.weekdaySymbols[weekday])s at \(time)"
        default: "Every day at \(time)"
        }
    }

    /// The hour and minute in the user's clock format.
    var time: String {
        Calendar.current.date(from: DateComponents(hour: hour, minute: minute))?.formatted(date: .omitted, time: .shortened) ?? ""
    }
}

/// What starts an automation (see `Trigger` in crates/dino-core/src/schedule.rs). One struct for
/// every kind, each reading the fields it needs, so all of them travel.
struct AutomationTrigger: Codable, Equatable {
    /// "schedule", "pr_opened", "pr_merged", "review_requested", "ci_failed", "issue_labeled",
    /// "comment", "new_commits", "behind", "files" or "after".
    var on = "schedule"
    var repo = ""
    var branch = ""
    var mine = false
    var label = ""
    var phrase = ""
    var path = ""
    var glob = ""
    var interval: UInt32 = 0
    var after = ""
    /// "any", "success" or "failure".
    var when = ""

    static let kinds: [(id: String, label: String, icon: String)] = [
        ("schedule", "On a schedule", "clock"),
        ("pr_opened", "A pull request opens", "arrow.triangle.pull"),
        ("pr_merged", "A pull request merges", "arrow.triangle.merge"),
        ("review_requested", "Your review is requested", "eye"),
        ("ci_failed", "A check fails", "xmark.octagon"),
        ("issue_labeled", "An issue gets a label", "tag"),
        ("comment", "A comment says something", "text.bubble"),
        ("new_commits", "New commits land", "arrow.down.circle"),
        ("behind", "The branch falls behind", "arrow.uturn.down.circle"),
        ("files", "Files change", "doc.badge.clock"),
        ("after", "Another run finishes", "link"),
    ]

    /// The kinds as the editor's menu groups them.
    static let groups: [(title: String, kinds: [String])] = [
        ("Time", ["schedule"]),
        ("GitHub", ["pr_opened", "pr_merged", "review_requested", "ci_failed", "issue_labeled", "comment"]),
        ("Git and files", ["new_commits", "behind", "files"]),
        ("Chains", ["after"]),
    ]

    var github: Bool { ["pr_opened", "pr_merged", "review_requested", "ci_failed", "issue_labeled", "comment"].contains(on) }
    var git: Bool { ["new_commits", "behind"].contains(on) }
    var icon: String { Self.kinds.first { $0.id == on }?.icon ?? "bolt" }

    /// The placeholders its events fill, for the prompt (as dinod fills them).
    var placeholders: [String] {
        switch on {
        case "pr_opened", "review_requested": ["pr.url", "pr.title", "pr.number", "pr.author", "pr.branch", "repo"]
        case "pr_merged": ["pr.url", "pr.title", "pr.number", "pr.author", "pr.branch", "pr.base", "pr.sha", "repo"]
        case "ci_failed": ["ci.check", "ci.log", "ci.branch", "ci.sha", "pr.url", "pr.number", "repo"]
        case "issue_labeled": ["issue.url", "issue.title", "issue.number", "label", "repo"]
        case "comment": ["comment.body", "comment.author", "comment.url", "issue.url", "issue.title", "issue.number", "repo"]
        case "new_commits": ["commits.log", "commits.range", "branch"]
        case "behind": ["branch", "behind", "commits.log"]
        case "files": ["files", "path"]
        case "after": ["after.summary", "after.outcome", "after.name", "after.session", "after.dir", "after.base"]
        default: []
        }
    }

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        on = try c.decodeIfPresent(String.self, forKey: .on) ?? "schedule"
        repo = try c.decodeIfPresent(String.self, forKey: .repo) ?? ""
        branch = try c.decodeIfPresent(String.self, forKey: .branch) ?? ""
        mine = try c.decodeIfPresent(Bool.self, forKey: .mine) ?? false
        label = try c.decodeIfPresent(String.self, forKey: .label) ?? ""
        phrase = try c.decodeIfPresent(String.self, forKey: .phrase) ?? ""
        path = try c.decodeIfPresent(String.self, forKey: .path) ?? ""
        glob = try c.decodeIfPresent(String.self, forKey: .glob) ?? ""
        interval = try c.decodeIfPresent(UInt32.self, forKey: .interval) ?? 0
        after = try c.decodeIfPresent(String.self, forKey: .after) ?? ""
        when = try c.decodeIfPresent(String.self, forKey: .when) ?? ""
    }
}

/// What an automation does.
struct AutomationAction: Codable, Equatable {
    /// "agent", "continue", "fanout" (several agents, a worktree each) or "command".
    var kind = "agent"
    var session = ""
    var agents: [String] = []
    var command = ""
    /// "never", "failure" or "always": the agent after the command.
    var then_agent = "never"

    enum CodingKeys: String, CodingKey {
        case kind = "do", session, agents, command, then_agent
    }

    static let kinds: [(id: String, label: String)] = [
        ("agent", "Start an agent"), ("continue", "Continue a session"), ("fanout", "Start several agents"), ("command", "Run a command"),
    ]

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        kind = try c.decodeIfPresent(String.self, forKey: .kind) ?? "agent"
        session = try c.decodeIfPresent(String.self, forKey: .session) ?? ""
        agents = try c.decodeIfPresent([String].self, forKey: .agents) ?? []
        command = try c.decodeIfPresent(String.self, forKey: .command) ?? ""
        then_agent = try c.decodeIfPresent(String.self, forKey: .then_agent) ?? "never"
        if then_agent.isEmpty { then_agent = "never" }
    }
}

struct AutomationConditions: Codable, Equatable {
    var if_changed = false
    /// "skip", "fallback" or "run".
    var on_limit = "skip"
    var ac_power = false
    var lid_open = false
    var retries: UInt32 = 0
    var backoff: UInt32 = 60
    var max_runs: UInt32 = 0
    var parallel = false

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        if_changed = try c.decodeIfPresent(Bool.self, forKey: .if_changed) ?? false
        on_limit = try c.decodeIfPresent(String.self, forKey: .on_limit) ?? "skip"
        ac_power = try c.decodeIfPresent(Bool.self, forKey: .ac_power) ?? false
        lid_open = try c.decodeIfPresent(Bool.self, forKey: .lid_open) ?? false
        retries = try c.decodeIfPresent(UInt32.self, forKey: .retries) ?? 0
        backoff = try c.decodeIfPresent(UInt32.self, forKey: .backoff) ?? 60
        max_runs = try c.decodeIfPresent(UInt32.self, forKey: .max_runs) ?? 0
        parallel = try c.decodeIfPresent(Bool.self, forKey: .parallel) ?? false
    }

    var isDefault: Bool { self == AutomationConditions() }
}

struct AutomationOutput: Codable, Equatable {
    var pr_comment = false
    var notify = true

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        pr_comment = try c.decodeIfPresent(Bool.self, forKey: .pr_comment) ?? false
        notify = try c.decodeIfPresent(Bool.self, forKey: .notify) ?? true
    }
}

/// What happened that fired a run.
struct AutomationEvent: Codable, Equatable {
    var on: String?
    var key: String?
    var title: String?
    var url: String?
    var fields: [String: String]?
}

/// What dinod keeps for a trigger; only `queue` and `runs` are of interest here.
struct AutomationState: Codable, Equatable {
    var queue: [AutomationEvent]?
    var runs: UInt32?
}

/// An automation: when something happens (`trigger`), dinod does something (`action`). Older
/// dinods only know scheduled tasks: those read as a schedule that starts `launcher` with `prompt`.
struct ScheduledTask: Codable, Equatable, Identifiable {
    /// Empty until dinod saves it.
    var id = ""
    var name = ""
    var prompt = ""
    var launcher = ""
    var cwd = ""
    var worktree = true
    /// Extra arguments for the agent, as typed: `--model haiku`.
    var args = ""
    var frequency = Frequency()
    var enabled = true
    var created_at: UInt64 = 0
    var last_due: UInt64?
    /// Oldest first, the last 20.
    var history: [ScheduledRun] = []
    var next_run: UInt64?
    var trigger = AutomationTrigger()
    var action = AutomationAction()
    var conditions = AutomationConditions()
    var output = AutomationOutput()
    var route: ProviderRoute?
    var state: AutomationState?
    /// Why its trigger can't look now (gh signed out, no such repo).
    var problem: String?

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        name = try c.decode(String.self, forKey: .name)
        prompt = try c.decode(String.self, forKey: .prompt)
        launcher = try c.decode(String.self, forKey: .launcher)
        cwd = try c.decode(String.self, forKey: .cwd)
        worktree = try c.decode(Bool.self, forKey: .worktree)
        args = try c.decodeIfPresent(String.self, forKey: .args) ?? ""
        frequency = try c.decode(Frequency.self, forKey: .frequency)
        enabled = try c.decode(Bool.self, forKey: .enabled)
        created_at = try c.decodeIfPresent(UInt64.self, forKey: .created_at) ?? 0
        last_due = try c.decodeIfPresent(UInt64.self, forKey: .last_due)
        history = try c.decodeIfPresent([ScheduledRun].self, forKey: .history) ?? []
        next_run = try c.decodeIfPresent(UInt64.self, forKey: .next_run)
        trigger = try c.decodeIfPresent(AutomationTrigger.self, forKey: .trigger) ?? AutomationTrigger()
        action = try c.decodeIfPresent(AutomationAction.self, forKey: .action) ?? AutomationAction()
        conditions = try c.decodeIfPresent(AutomationConditions.self, forKey: .conditions) ?? AutomationConditions()
        output = try c.decodeIfPresent(AutomationOutput.self, forKey: .output) ?? AutomationOutput()
        route = try c.decodeIfPresent(ProviderRoute.self, forKey: .route)
        state = try c.decodeIfPresent(AutomationState.self, forKey: .state)
        problem = try c.decodeIfPresent(String.self, forKey: .problem)
    }

    var scheduled: Bool { trigger.on == "schedule" }

    /// The action starts an agent of `launcher` (directly, or after its command).
    var usesAgent: Bool { action.kind == "agent" || (action.kind == "command" && action.then_agent != "never") }

    /// Events waiting for the run before them.
    var waiting: Int { state?.queue?.count ?? 0 }
}

struct ScheduledRun: Codable, Equatable {
    var at: UInt64
    /// The scheduled time it was for; nil for Run Now and events.
    var due: UInt64?
    var session: String?
    /// "started", "skipped" or "failed".
    var outcome: String
    var reason: String?
    /// Made up for a time missed while the Mac slept or dinod was down.
    var catch_up: Bool
    var id: String?
    var event: AutomationEvent?
    var attempt: UInt32?
    var sessions: [String]?
    var finished_at: UInt64?
    /// "success" or "failure", once it's over.
    var result: String?
    /// The agent's last message, or the command's last lines.
    var summary: String?
    var changes: DiffStat?
    var pr: String?
    var exit: Int32?
    var output: String?
    var commented: String?
}

private struct ScheduleResponse: Decodable {
    var tasks: [ScheduledTask]
}

extension DinoConnection {
    func scheduleList() throws -> [ScheduledTask] {
        try JSONDecoder().decode(ScheduleResponse.self, from: send(["type": "schedule_list"])).tasks
    }

    /// Create (empty id) or replace a task; throws dinod's reason it can't run as set up.
    func schedulePut(_ task: ScheduledTask) throws -> [ScheduledTask] {
        let encoded = try JSONSerialization.jsonObject(with: JSONEncoder().encode(task))
        return try JSONDecoder().decode(ScheduleResponse.self, from: send(["type": "schedule_put", "task": encoded])).tasks
    }

    func scheduleDelete(_ id: String) throws -> [ScheduledTask] {
        try JSONDecoder().decode(ScheduleResponse.self, from: send(["type": "schedule_delete", "id": id])).tasks
    }

    /// Run it now, whatever its schedule; returns the new session's id.
    func scheduleRun(_ id: String) throws -> String? {
        try request(["type": "schedule_run", "id": id]).id
    }
}

/// "today 9:00 AM", "tomorrow 9:00 AM", "Mon 9:00 AM".
func whenText(_ secs: UInt64) -> String {
    let date = Date(timeIntervalSince1970: TimeInterval(secs))
    let time = date.formatted(date: .omitted, time: .shortened)
    let cal = Calendar.current
    if cal.isDateInToday(date) { return "today \(time)" }
    if cal.isDateInTomorrow(date) { return "tomorrow \(time)" }
    if cal.isDateInYesterday(date) { return "yesterday \(time)" }
    return "\(date.formatted(.dateTime.weekday(.abbreviated))) \(time)"
}

// MARK: - Sidebar

struct ScheduledRow: View {
    @EnvironmentObject var model: DinoModel
    let task: ScheduledTask
    @State private var hovering = false

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 8) {
                Image(systemName: task.enabled ? task.trigger.icon : "pause.circle")
                    .foregroundStyle(task.problem != nil ? SessionStatus.exited.color : task.enabled ? Brand.green : .secondary)
                    .frame(width: 14)
                Image(systemName: open ? "chevron.down" : "chevron.right")
                    .font(.caption2.weight(.semibold))
                    .foregroundStyle(.tertiary)
                    .accessibilityHidden(true)
                Text(task.name).lineLimit(1)
                Spacer()
                if hovering {
                    Button { model.editingTask = task } label: { Image(systemName: "pencil") }
                        .buttonStyle(.plain)
                        .foregroundStyle(.secondary)
                        .help("Edit…")
                        .accessibilityLabel("Edit \(task.name)")
                }
                if model.isRunning(task) {
                    ProgressView().controlSize(.mini).help("Running now")
                } else if task.waiting > 0 {
                    Text("\(task.waiting) waiting").font(.caption).foregroundStyle(SessionStatus.needsYou.color)
                } else if !task.enabled {
                    Text("Paused").font(.caption).foregroundStyle(.tertiary)
                } else {
                    // How the last run went, and when the next one is (or else when that one was).
                    HStack(spacing: 4) {
                        if let last = task.history.last {
                            let state = model.runState(last)
                            Circle().fill(state.color).frame(width: 6, height: 6)
                                .help("Last run: \(state.label.lowercased()), \(whenText(last.at))")
                                .accessibilityLabel("Last run \(state.label)")
                        }
                        if let next = task.next_run {
                            Text(whenText(next)).font(.caption.monospacedDigit()).foregroundStyle(.secondary).help("Next run")
                        } else if let last = task.history.last {
                            Text(ago(last.at)).font(.caption.monospacedDigit()).foregroundStyle(.tertiary).help("Last run \(whenText(last.at))")
                        }
                    }
                    .fixedSize()
                }
            }
            if let problem = task.problem, task.enabled {
                Text(problem)
                    .font(.caption)
                    .foregroundStyle(SessionStatus.exited.color)
                    .lineLimit(1)
                    .padding(.leading, 22)
                    .help(problem)
            } else if let last = task.history.last, last.outcome != "started" {
                Text(last.reason ?? last.outcome.capitalized)
                    .font(.caption)
                    .foregroundStyle(last.outcome == "failed" ? SessionStatus.exited.color : SessionStatus.needsYou.color)
                    .lineLimit(1)
                    .padding(.leading, 22)
                    .help(last.reason ?? "")
            } else {
                Text(model.automationLine(task))
                    .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                    .padding(.leading, 22)
            }
        }
        .padding(.vertical, 2)
        .opacity(task.enabled ? 1 : 0.7)
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
        .help(task.history.isEmpty ? "\(model.triggerText(task)). Hasn't run yet." : "\(model.triggerText(task)). Click to see its runs.")
        .contextMenu { ScheduledMenu(task: task) }
    }

    private var open: Bool { model.openTasks.contains(task.id) }
}

/// An automation's runs, newest first, under its row: each opens the session it started, or that
/// session's conversation once it's archived.
struct TaskRuns: View {
    @EnvironmentObject var model: DinoModel
    let task: ScheduledTask

    var body: some View {
        let runs = Array(task.history.enumerated().reversed())
        if runs.isEmpty {
            Text(task.enabled ? (task.scheduled ? "No runs yet. Next run: \(task.next_run.map(whenText) ?? "soon")" : "No runs yet. Runs when \(model.triggerText(task).lowercasedFirst)") : "No runs yet")
                .font(.caption).foregroundStyle(.tertiary)
                .lineLimit(2)
                .padding(.leading, 22)
        }
        ForEach(runs, id: \.offset) { index, run in
            TaskRunRow(task: task, run: run).tag(run.session.map { "run:\($0)" } ?? "runinfo:\(task.id):\(index)")
        }
    }
}

struct TaskRunRow: View {
    @EnvironmentObject var model: DinoModel
    let task: ScheduledTask
    let run: ScheduledRun

    var body: some View {
        // It shows its session's title, which changes without the model saying: redrawn as it does.
        if let s = run.session.flatMap({ id in model.sessions.first { $0.id == id } }) {
            Live(s) { _ in row }
        } else {
            row
        }
    }

    private var row: some View {
        let state = model.runState(run)
        return VStack(alignment: .leading, spacing: 1) {
            HStack(spacing: 6) {
                Circle().fill(state.color).frame(width: 6, height: 6).accessibilityHidden(true)
                Text(whenText(run.at)).font(.caption.monospacedDigit())
                Text(state.label).font(.caption).foregroundStyle(state.color).lineLimit(1).fixedSize()
                Text(run.event?.title ?? state.title ?? "").font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.tail)
                Spacer(minLength: 0)
                if let c = run.changes, c.files > 0 {
                    Text("+\(c.added) −\(c.removed)")
                        .font(.caption2.monospacedDigit())
                        .foregroundStyle(.secondary)
                        .help("\(c.files) \(c.files == 1 ? "file" : "files") changed")
                }
            }
            if let summary = run.summary?.firstLine, !summary.isEmpty {
                Text(summary)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.tail)
                    .padding(.leading, 12)
            }
        }
        .padding(.leading, 22)
        .opacity(state.reachable ? 1 : 0.6)
        .help(help(state))
        .accessibilityElement(children: .combine)
        .contextMenu { RunMenu(task: task, run: run) }
        // Selected by the list, clicked or arrowed onto alike (see Sidebar): a live run's session
        // opens, an archived one's conversation shows in the main area. One with nothing to show
        // can't be selected, so it never keeps a highlight nothing else agrees with.
        .selectionDisabled(!state.reachable)
    }

    private func help(_ state: RunState) -> String {
        var parts = [state.help]
        if let e = run.event?.title { parts.append(e) }
        if let s = run.summary { parts.append(s) }
        if let c = run.commented { parts.append("Commented on the PR: \(c)") }
        return parts.joined(separator: "\n\n")
    }
}

/// What can be done with a run: open what it started, look at what it changed, follow its links.
struct RunMenu: View {
    @EnvironmentObject var model: DinoModel
    let task: ScheduledTask
    let run: ScheduledRun

    var body: some View {
        let live = run.session.flatMap { id in model.sessions.first { $0.id == id } }
        if let s = live {
            Button("Open Session") { model.openRun(s.id) }
            Button("Review Changes") {
                model.openRun(s.id)
                model.showReview = true
            }
            .disabled((run.changes?.files ?? 1) == 0)
        } else if let id = run.session, model.archived.contains(where: { $0.id == id }) {
            Button("Read Conversation") { model.openRun(id) }
        }
        if let pr = live?.pr?.url ?? run.pr, let url = URL(string: pr) {
            Button("Open Pull Request") { NSWorkspace.shared.open(url) }
        }
        if let link = run.event?.url, let url = URL(string: link) {
            Button("Open \(eventNoun)") { NSWorkspace.shared.open(url) }
        }
        if let c = run.commented, let url = URL(string: c) {
            Button("Open Comment") { NSWorkspace.shared.open(url) }
        }
        if let summary = run.summary {
            Divider()
            Button("Copy Summary") {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(summary, forType: .string)
            }
        }
        if let output = run.output, !output.isEmpty {
            Button("Copy Command Output") {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(output, forType: .string)
            }
        }
    }

    private var eventNoun: String {
        switch run.event?.on {
        case "ci_failed": "Failed Check"
        case "comment": "Comment"
        case "issue_labeled": "Issue"
        default: "Pull Request on GitHub"
        }
    }
}

extension String {
    var firstLine: String? {
        split(whereSeparator: \.isNewline).first { !$0.trimmingCharacters(in: .whitespaces).isEmpty }.map(String.init)
    }

    var lowercasedFirst: String {
        guard let f = first, dropFirst().first?.isUppercase != true else { return self }
        return f.lowercased() + dropFirst()
    }
}

/// A scheduled run whose session is archived, selected in the sidebar: its conversation, in the
/// main area, with the way back.
struct ArchivedRunPane: View {
    @EnvironmentObject var model: DinoModel
    let session: ArchivedInfo
    @State private var deleting = false

    var body: some View {
        ArchivedPreview(session: session, delete: { deleting = true }, fill: true)
            .alert("Delete “\(session.display)”?", isPresented: $deleting) {
                Button("Delete", role: .destructive) { model.deleteArchived(session) }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text(session.branch.map { "It's permanently removed from the archive. Its branch \($0) stays in the repo." } ?? "It's permanently removed from the archive.")
            }
    }
}

/// What a run looks like now: the session it started, as it is now, or why there's none.
struct RunState {
    var label: String
    var color: Color
    var title: String?
    var help: String
    /// Clicking it shows something.
    var reachable: Bool
}

extension DinoModel {
    func toggleRuns(_ task: String) {
        if openTasks.contains(task) { openTasks.remove(task) } else { openTasks.insert(task) }
    }

    func runState(_ run: ScheduledRun) -> RunState {
        let when = run.catch_up ? " (caught up)" : (run.attempt ?? 0) > 0 ? " (retry \(run.attempt ?? 0))" : run.event != nil ? "" : run.due == nil ? " (run now)" : ""
        switch run.outcome {
        case "skipped":
            return RunState(label: "Skipped", color: SessionStatus.needsYou.color, title: run.reason, help: "Skipped\(when): \(run.reason ?? "")", reachable: false)
        case "failed":
            return RunState(label: "Didn't start", color: SessionStatus.exited.color, title: run.reason, help: "Didn't start\(when): \(run.reason ?? "")", reachable: false)
        default:
            break
        }
        let finished = run.result.map { $0 == "success" ? "Done" : "Failed" }
        let finishedColor = run.result == "failure" ? SessionStatus.exited.color : SessionStatus.done.color
        guard let id = run.session else {
            if let finished {
                return RunState(label: finished, color: finishedColor, title: nil, help: "\(finished)\(when)", reachable: false)
            }
            let label = run.outcome == "started" ? "Running" : run.outcome.capitalized
            return RunState(label: label, color: run.outcome == "started" ? SessionStatus.working.color : .secondary, title: nil, help: label + when, reachable: false)
        }
        if let s = sessions.first(where: { $0.id == id }), let finished, !s.exited {
            return RunState(label: finished, color: finishedColor, title: s.title ?? s.display, help: "\(finished)\(when), in \(s.display): click to open it", reachable: true)
        }
        if let s = sessions.first(where: { $0.id == id }) {
            let st = status(of: s)
            let label = s.exited ? ((s.exit_code ?? 0) == 0 ? "Ended" : "Failed") : st.label
            let color = s.exited ? ((s.exit_code ?? 0) == 0 ? Color.secondary : SessionStatus.exited.color) : st.color
            return RunState(label: label, color: color, title: s.title ?? s.display, help: "Started\(when) as \(s.display): click to open it", reachable: true)
        }
        if let a = archived.first(where: { $0.id == id }) {
            return RunState(label: finished ?? "Archived", color: finished == nil ? .secondary : finishedColor, title: a.display, help: "Started\(when) as \(a.display), now archived: click to read it", reachable: true)
        }
        return RunState(label: finished ?? "Gone", color: finished == nil ? .secondary : finishedColor, title: nil, help: "Started\(when); its session has since been closed or deleted", reachable: false)
    }

    /// A run's session, selected and its tab opened, while it's around; an archived one's
    /// conversation, in the main area (`archivedRun`).
    func openRun(_ session: String, keepKeyboard: Bool = false) {
        if sessions.contains(where: { $0.id == session }) {
            select(session, keepKeyboard: keepKeyboard)
        } else if archived.contains(where: { $0.id == session }) {
            select("run:\(session)", keepKeyboard: keepKeyboard)
        }
    }

    /// The archived session whose run is selected, read-only in the main area.
    var archivedRun: ArchivedInfo? {
        guard let sel = selected, sel.hasPrefix("run:") else { return nil }
        let id = sel.dropFirst(4)
        return archived.first { $0.id == id }
    }
}

struct ScheduledMenu: View {
    @EnvironmentObject var model: DinoModel
    let task: ScheduledTask

    var body: some View {
        Button("Run Now") { model.runTask(task) }
        Button(task.enabled ? "Pause" : "Resume") { model.setTask(task, enabled: !task.enabled) }
        Button("Edit…") { model.editingTask = task }
        Button("Duplicate…") { model.duplicateTask(task) }
        Button(model.openTasks.contains(task.id) ? "Hide Runs" : "Show Runs") { model.toggleRuns(task.id) }
        if let last = task.history.last(where: { $0.session != nil })?.session, model.sessions.contains(where: { $0.id == last }) {
            Button("Show Last Run") { model.select(last) }
        }
        Divider()
        Button("Delete…", role: .destructive) { model.deletingTask = task }
    }
}

// MARK: - Editor

/// A step of an automation, read top to bottom as a sentence: When, Do, Only if, Then. A dot on a
/// rail, the step's name and what it's set to in words, and its controls under them.
private struct FlowStep<Content: View>: View {
    let title: String
    let icon: String
    let tint: Color
    var summary: String?
    var last = false
    @ViewBuilder var content: Content

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            VStack(spacing: 0) {
                Image(systemName: icon)
                    .font(.system(size: 11, weight: .bold))
                    .foregroundStyle(.white)
                    .frame(width: 24, height: 24)
                    .background(Circle().fill(tint.gradient))
                if !last {
                    Rectangle().fill(.quaternary).frame(width: 2).frame(maxHeight: .infinity).padding(.vertical, 3)
                }
            }
            .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 8) {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(title).font(.headline)
                    if let summary {
                        Text(summary).foregroundStyle(.secondary).lineLimit(1).truncationMode(.tail)
                    }
                    Spacer(minLength: 0)
                }
                .frame(minHeight: 24)
                content
            }
            .padding(.bottom, last ? 0 : 16)
        }
    }
}

/// Words and controls on one line, read as a phrase: "on [my pull requests] in [owner/name]".
private struct Words<Content: View>: View {
    @ViewBuilder var content: Content

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) { content }
    }
}

private extension View {
    /// A step's controls, set apart from the rail.
    func stepCard() -> some View {
        padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: 10).fill(.background.secondary))
            .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(Color.primary.opacity(0.07)))
    }

    func inlinePicker() -> some View {
        labelsHidden().fixedSize()
    }
}

struct ScheduleSheet: View {
    @EnvironmentObject var model: DinoModel
    @Environment(\.dismiss) private var dismiss
    @State var task: ScheduledTask
    /// A new one shows the templates first.
    @State private var picking: Bool
    @State private var saving = false
    @State private var error: String?
    @State private var showConditions = false
    /// The GitHub repo the folder is a clone of, for what an empty repo means.
    @State private var origin: String?
    @FocusState private var focused: Bool

    init(task: ScheduledTask) {
        _task = State(initialValue: task)
        _picking = State(initialValue: task.id.isEmpty && task.name.isEmpty)
        _showConditions = State(initialValue: !task.conditions.isDefault)
    }

    private var isNew: Bool { task.id.isEmpty }
    private var launcher: LauncherInfo? { model.launchers.first { $0.short == task.launcher } }
    private var isShell: Bool { launcher?.agent_id == "shell" }
    /// It sends a prompt to an agent, so it needs one.
    private var needsPrompt: Bool {
        switch task.action.kind {
        case "continue", "fanout": true
        case "agent": !isShell
        default: false
        }
    }

    private var time: Binding<Date> {
        Binding(
            get: { Calendar.current.date(from: DateComponents(hour: task.frequency.hour, minute: task.frequency.minute)) ?? Date() },
            set: {
                let c = Calendar.current.dateComponents([.hour, .minute], from: $0)
                task.frequency.hour = c.hour ?? 9
                task.frequency.minute = c.minute ?? 0
            }
        )
    }

    private var interval: Binding<Int> {
        Binding(get: { Int(task.trigger.interval) }, set: { task.trigger.interval = UInt32(max(0, $0)) })
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            if picking {
                TemplateGallery(folder: task.cwd, repo: origin, pick: pick, changeFolder: chooseFolder)
                HStack {
                    Spacer()
                    Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
                }
            } else {
                editor
            }
        }
        .padding(22)
        .frame(width: 640)
        .task(id: task.cwd) { origin = await githubRepo(of: task.cwd) }
        .onChange(of: task.action.kind) { _, kind in
            // Something sensible to start from.
            if kind == "fanout", task.action.agents.isEmpty {
                task.action.agents = Array(model.launchers.filter { $0.agent_id != "shell" }.prefix(2).map(\.short))
            }
            if kind == "continue", task.action.session.isEmpty {
                task.action.session = continuable.first?.id ?? ""
            }
        }
    }

    private func pick(_ template: AutomationTemplate?) {
        if let template {
            task = model.templated(template, base: task)
            showConditions = !task.conditions.isDefault
        }
        withAnimation(.easeOut(duration: 0.15)) { picking = false }
        focused = template == nil
    }

    @ViewBuilder private var editor: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                if isNew {
                    Button {
                        withAnimation(.easeOut(duration: 0.15)) { picking = true }
                    } label: {
                        Label("Templates", systemImage: "chevron.left").labelStyle(.iconOnly)
                    }
                    .buttonStyle(.borderless)
                    .help("Back to the templates")
                    .accessibilityLabel("Back to the templates")
                }
                TextField("", text: $task.name, prompt: Text("Name this automation"))
                    .textFieldStyle(.plain)
                    .font(.title2.weight(.semibold))
                    .focused($focused)
                    .accessibilityLabel("Name")
            }
            // What it does, in one sentence, as it's set up right now.
            Text(model.sentence(task, origin: origin))
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
                .textSelection(.enabled)
                .accessibilityLabel("Summary: \(model.sentence(task, origin: origin))")
        }
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                FlowStep(title: "When", icon: task.trigger.icon, tint: .indigo, summary: nil) {
                    whenStep.stepCard()
                }
                FlowStep(title: "Do", icon: "bolt.fill", tint: Brand.green, summary: nil) {
                    doStep.stepCard()
                }
                FlowStep(title: "Only if", icon: "line.3.horizontal.decrease", tint: .orange, summary: conditionsSummary ?? "every time") {
                    onlyIfStep
                }
                FlowStep(title: "Then", icon: "checkmark", tint: .teal, summary: nil, last: true) {
                    thenStep.stepCard()
                }
            }
            .padding(.trailing, 6)
            .padding(.top, 4)
        }
        .frame(maxHeight: 560)
        .fixedSize(horizontal: false, vertical: true)
        if !task.history.isEmpty {
            RecentRuns(runs: task.history)
        }
        if let error {
            Text(error).foregroundStyle(SessionStatus.exited.color).font(.callout)
        }
        HStack {
            if !isNew {
                Toggle("Paused", isOn: Binding(get: { !task.enabled }, set: { task.enabled = !$0 }))
                    .toggleStyle(.checkbox)
            }
            Spacer()
            Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
            Button {
                save()
            } label: {
                if saving { ProgressView().controlSize(.small).frame(width: 90) } else { Text(isNew ? "Create" : "Save").frame(width: 90) }
            }
            .keyboardShortcut(.return, modifiers: .command)
            .buttonStyle(.borderedProminent)
            .tint(Brand.green)
            .disabled(!ready || saving)
            .help(missing ?? "")
        }
    }

    /// What still has to be filled in before it can be saved.
    private var missing: String? {
        let blank = { (s: String) in s.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
        if blank(task.name) { return "Name it" }
        if task.trigger.on == "issue_labeled", blank(task.trigger.label) { return "Name the label" }
        if task.trigger.on == "comment", blank(task.trigger.phrase) { return "Give the words a comment has to say" }
        if task.trigger.on == "after", task.trigger.after.isEmpty { return "Choose what it comes after" }
        if task.action.kind == "command", blank(task.action.command) { return "Give the command to run" }
        if task.action.kind == "continue", task.action.session.isEmpty { return "Choose the session to continue" }
        if needsPrompt, blank(task.prompt) { return "Say what the agent should do" }
        return nil
    }

    private var ready: Bool { missing == nil }

    private var explanation: String {
        switch task.trigger.on {
        case "schedule": task.frequency.every == "manual" ? "It runs only when you choose Run Now." : "Runs on schedule while your Mac is awake. If your Mac was asleep at a scheduled time, the automation runs once when it wakes."
        case let k where task.trigger.github: "dino checks GitHub about once a minute\(k == "review_requested" && task.trigger.repo.isEmpty ? " (every few minutes when watching all repos)" : ""), using your gh sign-in, and runs once for each new event."
        case "new_commits", "behind": "dino fetches every \(task.trigger.interval == 0 ? 10 : Int(task.trigger.interval)) minutes and runs once each time the branch changes."
        case "files": "Runs once changes in the folder settle. Files git ignores don't count, and changes made during a run don't start another run."
        default: "Runs each time the automation or session you choose finishes."
        }
    }

    // MARK: When

    @ViewBuilder private var whenStep: some View {
        VStack(alignment: .leading, spacing: 10) {
            Picker("", selection: $task.trigger.on) {
                ForEach(AutomationTrigger.groups, id: \.title) { g in
                    Section(g.title) {
                        ForEach(g.kinds, id: \.self) { id in
                            if let k = AutomationTrigger.kinds.first(where: { $0.id == id }) {
                                Label(k.label, systemImage: k.icon).tag(k.id)
                            }
                        }
                    }
                }
            }
            .inlinePicker()
            .accessibilityLabel("When")
            triggerFields
            Text(explanation)
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var repoPrompt: String {
        if task.trigger.on == "review_requested" { return "any repo" }
        return origin.map { "\($0) (this folder's)" } ?? "owner/name"
    }

    @ViewBuilder private var repoField: some View {
        Words {
            Text("in")
            TextField("", text: $task.trigger.repo, prompt: Text(repoPrompt))
                .textFieldStyle(.roundedBorder)
                .font(.system(.body, design: .monospaced))
                .frame(maxWidth: 300)
                .accessibilityLabel("Repo")
        }
    }

    /// Which pull requests' checks count: mine, any, or one branch's.
    private var ciScope: Binding<String> {
        Binding(
            get: { !task.trigger.branch.isEmpty ? "branch" : task.trigger.mine ? "mine" : "any" },
            set: {
                switch $0 {
                case "branch": task.trigger.branch = task.trigger.branch.isEmpty ? "main" : task.trigger.branch
                case "mine": task.trigger.branch = ""; task.trigger.mine = true
                default: task.trigger.branch = ""; task.trigger.mine = false
                }
            }
        )
    }

    @ViewBuilder private var triggerFields: some View {
        switch task.trigger.on {
        case "schedule":
            Words {
                Picker("", selection: $task.frequency.every) {
                    ForEach(Frequency.kinds, id: \.id) { Text($0.label).tag($0.id) }
                }
                .inlinePicker()
                .accessibilityLabel("Repeats")
                switch task.frequency.every {
                case "manual":
                    EmptyView()
                case "hourly":
                    Text("at minute")
                    Picker("", selection: $task.frequency.minute) {
                        ForEach(Array(stride(from: 0, to: 60, by: 5)), id: \.self) { Text(String(format: ":%02d", $0)).tag($0) }
                    }
                    .inlinePicker()
                default:
                    if task.frequency.every == "weekly" {
                        Text("on")
                        Picker("", selection: $task.frequency.weekday) {
                            ForEach(0..<7, id: \.self) { Text(Calendar.current.weekdaySymbols[$0]).tag($0) }
                        }
                        .inlinePicker()
                    }
                    Text("at")
                    DatePicker("", selection: time, displayedComponents: .hourAndMinute).inlinePicker()
                }
            }
        case "ci_failed":
            Words {
                Text("on")
                Picker("", selection: ciScope) {
                    Text("my pull requests").tag("mine")
                    Text("any pull request").tag("any")
                    Text("a branch").tag("branch")
                }
                .inlinePicker()
                .accessibilityLabel("Which checks")
                if !task.trigger.branch.isEmpty {
                    TextField("", text: $task.trigger.branch, prompt: Text("main"))
                        .textFieldStyle(.roundedBorder)
                        .frame(maxWidth: 160)
                        .accessibilityLabel("Branch")
                }
            }
            repoField
        case "issue_labeled":
            Words {
                Text("with the label")
                TextField("", text: $task.trigger.label, prompt: Text("bug"))
                    .textFieldStyle(.roundedBorder)
                    .frame(maxWidth: 160)
                    .accessibilityLabel("Label")
            }
            repoField
        case "comment":
            Words {
                Text("that says")
                TextField("", text: $task.trigger.phrase, prompt: Text("@dino"))
                    .textFieldStyle(.roundedBorder)
                    .frame(maxWidth: 160)
                    .accessibilityLabel("Says")
                Text("(any case), on an issue or PR").foregroundStyle(.secondary)
            }
            repoField
        case _ where task.trigger.github:
            repoField
        case "new_commits", "behind":
            Words {
                Text(task.trigger.on == "behind" ? "for" : "on")
                TextField("", text: $task.trigger.branch, prompt: Text(task.trigger.on == "behind" ? "the checked-out branch" : "the default branch"))
                    .textFieldStyle(.roundedBorder)
                    .frame(maxWidth: 200)
                    .accessibilityLabel("Branch")
                Text("checked every").foregroundStyle(.secondary)
                Picker("", selection: interval) {
                    ForEach([0, 5, 15, 30, 60], id: \.self) { Text($0 == 0 ? "10 min" : "\($0) min").tag($0) }
                }
                .inlinePicker()
                .accessibilityLabel("Every")
            }
        case "files":
            Words {
                Text("matching")
                TextField("", text: $task.trigger.glob, prompt: Text(verbatim: "any file, or *.swift, docs/**"))
                    .textFieldStyle(.roundedBorder)
                    .font(.system(.body, design: .monospaced))
                    .accessibilityLabel("Files")
            }
            Words {
                Text("in")
                TextField("", text: $task.trigger.path, prompt: Text("the whole folder, or a folder inside it"))
                    .textFieldStyle(.roundedBorder)
                    .accessibilityLabel("In")
            }
        default:
            Words {
                Picker("", selection: $task.trigger.after) {
                    Text("Choose…").tag("")
                    let others = model.scheduled.filter { $0.id != task.id }
                    if !others.isEmpty {
                        Section("Automations") {
                            ForEach(others) { Text($0.name).tag($0.id) }
                        }
                    }
                    let sessions = model.sessions.filter { !$0.exited && $0.agent_id != "shell" }
                    if !sessions.isEmpty {
                        Section("Sessions") {
                            ForEach(sessions) { Text(model.tabName($0)).tag($0.id) }
                        }
                    }
                }
                .inlinePicker()
                .accessibilityLabel("After")
                Picker("", selection: $task.trigger.when) {
                    Text("finishes").tag("")
                    Text("succeeds").tag("success")
                    Text("fails").tag("failure")
                }
                .inlinePicker()
                .accessibilityLabel("Outcome")
            }
        }
    }

    // MARK: Do

    /// Agents' sessions a prompt can be sent into.
    private var continuable: [SessionInfo] { model.sessions.filter { $0.agent_id != "shell" && $0.host == nil } }

    @ViewBuilder private var agentPicker: some View {
        Picker("", selection: $task.launcher) {
            ForEach(model.launchers) { l in Text(l.label).tag(l.short) }
        }
        .inlinePicker()
        .accessibilityLabel("Agent")
        if !isShell {
            TextField("", text: $task.args, prompt: Text("options: --model haiku"))
                .textFieldStyle(.roundedBorder)
                .font(.system(.callout, design: .monospaced))
                .accessibilityLabel("Agent options")
        }
    }

    @ViewBuilder private var doStep: some View {
        VStack(alignment: .leading, spacing: 10) {
            Picker("", selection: $task.action.kind) {
                ForEach(AutomationAction.kinds, id: \.id) {
                    Text($0.label).tag($0.id)
                }
            }
            .inlinePicker()
            .accessibilityLabel("Do")
            switch task.action.kind {
            case "continue":
                Words {
                    Text("send the prompt into")
                    Picker("", selection: $task.action.session) {
                        if task.action.session.isEmpty { Text("Choose…").tag("") }
                        ForEach(continuable) { Text(model.tabName($0)).tag($0.id) }
                    }
                    .inlinePicker()
                    .accessibilityLabel("Session")
                }
            case "fanout":
                Words {
                    Text("each in its own worktree:")
                    ForEach(model.launchers.filter { $0.agent_id != "shell" }) { l in
                        Toggle(l.label, isOn: Binding(
                            get: { task.action.agents.contains(l.short) },
                            set: { on in
                                if on { task.action.agents.append(l.short) } else { task.action.agents.removeAll { $0 == l.short } }
                            }
                        ))
                        .toggleStyle(.checkbox)
                        .fixedSize()
                    }
                }
            case "command":
                Words {
                    Text("run")
                    TextField("", text: $task.action.command, prompt: Text("make test"))
                        .textFieldStyle(.roundedBorder)
                        .font(.system(.body, design: .monospaced))
                        .accessibilityLabel("Command")
                }
                Words {
                    Text("then")
                    Picker("", selection: $task.action.then_agent) {
                        Text("stop").tag("never")
                        Text("start an agent if it fails").tag("failure")
                        Text("always start an agent").tag("always")
                    }
                    .inlinePicker()
                    .accessibilityLabel("Then")
                    if task.action.then_agent != "never" {
                        agentPicker
                    }
                }
            default:
                Words {
                    Text("with")
                    agentPicker
                }
            }
            if needsPrompt || task.usesAgent {
                promptEditor
            }
            Words {
                Image(systemName: "folder").foregroundStyle(.secondary)
                Text(shortPath(task.cwd)).lineLimit(1).truncationMode(.middle)
                Button("Change…") { chooseFolder() }.buttonStyle(.link)
                Spacer(minLength: 8)
                if task.usesAgent {
                    Toggle("Each run in its own worktree", isOn: $task.worktree)
                        .toggleStyle(.checkbox)
                        .fixedSize()
                        .help("Each run gets its own branch and checkout, so runs don't interfere with your work or with each other.")
                }
            }
            .font(.callout)
        }
    }

    // MARK: Prompt

    private var placeholders: [String] {
        var all = task.trigger.placeholders
        if task.action.kind == "command" { all += ["cmd.output", "cmd.exit"] }
        if task.trigger.on != "schedule" { all.append("event") }
        return all
    }

    @ViewBuilder private var promptEditor: some View {
        VStack(alignment: .leading, spacing: 6) {
            TextEditor(text: $task.prompt)
                .font(.body)
                .scrollContentBackground(.hidden)
                .padding(8)
                .frame(height: 120)
                .background(RoundedRectangle(cornerRadius: 8).fill(Color(nsColor: .textBackgroundColor)))
                .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(.quaternary))
                .overlay(alignment: .topLeading) {
                    if task.prompt.isEmpty {
                        Text(promptHint).foregroundStyle(.tertiary).padding(13).allowsHitTesting(false)
                    }
                }
                .accessibilityLabel("Prompt")
            if !placeholders.isEmpty {
                HStack(spacing: 4) {
                    Text("Insert").font(.caption).foregroundStyle(.secondary)
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 4) {
                            ForEach(placeholders, id: \.self) { p in
                                Button("{\(p)}") { insert(p) }
                                    .buttonStyle(.plain)
                                    .font(.caption.monospaced())
                                    .padding(.horizontal, 5).padding(.vertical, 2)
                                    .background(Capsule().fill(.quaternary))
                                    .help("Filled with what happened: \(p)")
                            }
                        }
                    }
                }
            }
        }
    }

    private var promptHint: String {
        switch task.action.kind {
        case "command": "What the agent should do (empty: fix what made the command fail, with its output)"
        case "continue": "What to send into the session"
        default: isShell ? "A command to run (optional)" : "What should it do each time?"
        }
    }

    private func insert(_ p: String) {
        if !task.prompt.isEmpty, !task.prompt.hasSuffix(" "), !task.prompt.hasSuffix("\n") { task.prompt += " " }
        task.prompt += "{\(p)}"
    }

    // MARK: Only if

    @ViewBuilder private var onlyIfStep: some View {
        Button {
            withAnimation(.easeOut(duration: 0.15)) { showConditions.toggle() }
        } label: {
            HStack(spacing: 6) {
                Image(systemName: showConditions ? "chevron.down" : "chevron.right").font(.caption.weight(.semibold))
                Text(showConditions ? "Hide conditions" : "Power, lid, changes, usage limits, retries…").font(.callout)
            }
            .foregroundStyle(.secondary)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(showConditions ? "Hide conditions" : "Show conditions")
        if showConditions {
            VStack(alignment: .leading, spacing: 8) {
                Toggle("Only if the repo changed since the last run", isOn: $task.conditions.if_changed)
                Toggle("Only while your Mac is plugged in", isOn: $task.conditions.ac_power)
                Toggle("Only while the lid is open", isOn: $task.conditions.lid_open)
                Toggle("Start a run while the one before is still going", isOn: $task.conditions.parallel)
                    .help("When off, a scheduled run is skipped and an event waits for the previous run to finish.")
                if task.usesAgent {
                    Words {
                        Text("When the agent is at its usage limit")
                        Picker("", selection: $task.conditions.on_limit) {
                            Text("skip the run").tag("skip")
                            Text("use its fallback agent").tag("fallback")
                            Text("start it anyway").tag("run")
                        }
                        .inlinePicker()
                        .help("The fallback agent is the one new sessions start with during a limit, set in Settings → Agents.")
                    }
                }
                Words {
                    Toggle("Retry a failed run", isOn: Binding(get: { task.conditions.retries > 0 }, set: { task.conditions.retries = $0 ? max(task.conditions.retries, 2) : 0 }))
                    if task.conditions.retries > 0 {
                        Stepper("\(task.conditions.retries) times", value: Binding(get: { Int(task.conditions.retries) }, set: { task.conditions.retries = UInt32(max(1, min(10, $0))) }), in: 1...10)
                            .fixedSize()
                        Text("first after \(Int(task.conditions.backoff)) s, then twice as long").font(.caption).foregroundStyle(.secondary)
                    }
                }
                Words {
                    Toggle("Pause after", isOn: Binding(get: { task.conditions.max_runs > 0 }, set: { task.conditions.max_runs = $0 ? max(task.conditions.max_runs, 1) : 0 }))
                    if task.conditions.max_runs > 0 {
                        Stepper("\(task.conditions.max_runs) \(task.conditions.max_runs == 1 ? "run" : "runs")", value: Binding(get: { Int(task.conditions.max_runs) }, set: { task.conditions.max_runs = UInt32(max(1, $0)) }), in: 1...1000)
                            .fixedSize()
                    } else {
                        Text("a number of runs").foregroundStyle(.secondary)
                    }
                }
            }
            .stepCard()
        }
    }

    private var conditionsSummary: String? {
        var parts: [String] = []
        let c = task.conditions
        if c.if_changed { parts.append("if the repo changed") }
        if c.ac_power { parts.append("plugged in") }
        if c.lid_open { parts.append("lid open") }
        if c.parallel { parts.append("overlapping") }
        if c.on_limit != "skip", task.usesAgent { parts.append(c.on_limit == "run" ? "even at the limit" : "fallback at the limit") }
        if c.retries > 0 { parts.append("\(c.retries) retries") }
        if c.max_runs > 0 { parts.append("\(c.max_runs) runs") }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }

    // MARK: Then

    /// Where the summary goes as a comment: the thread that started the run, or the run's own PR.
    private var commentTarget: String {
        switch task.trigger.on {
        case "comment": "Reply in the thread with the summary"
        case "issue_labeled": "Post the summary as a comment on the issue"
        case "pr_opened", "pr_merged", "review_requested", "ci_failed": "Post the summary as a comment on the PR"
        default: "Post the summary as a comment on the PR it opens"
        }
    }

    @ViewBuilder private var thenStep: some View {
        VStack(alignment: .leading, spacing: 8) {
            Label {
                Text("Keep the agent's summary and what it changed, under the automation in the sidebar")
            } icon: {
                Image(systemName: "checkmark.circle.fill").foregroundStyle(Brand.green)
            }
            .foregroundStyle(.secondary)
            .help("Always: each run shows its summary and its diff stats; open it to review the changes.")
            Toggle(commentTarget, isOn: $task.output.pr_comment)
                .help("Posts on the issue or PR that started the run, or else on the PR for the run's branch. Comments dino posts never start a comment automation again.")
            Toggle("Notify me when a run finishes", isOn: $task.output.notify)
        }
    }

    private func chooseFolder() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.directoryURL = URL(fileURLWithPath: task.cwd)
        panel.prompt = "Use Folder"
        if panel.runModal() == .OK, let url = panel.url { task.cwd = url.path }
    }

    private func save() {
        saving = true
        error = nil
        Task {
            do {
                try await model.saveTask(task)
                dismiss()
            } catch {
                self.error = error.localizedDescription
            }
            saving = false
        }
    }
}

private struct RecentRuns: View {
    @EnvironmentObject var model: DinoModel
    let runs: [ScheduledRun]

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("RECENT RUNS").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
            ForEach(Array(runs.suffix(4).reversed().enumerated()), id: \.offset) { _, run in
                let ok = run.outcome == "started" && run.result != "failure"
                HStack(spacing: 6) {
                    Image(systemName: run.outcome == "skipped" ? "forward.circle" : ok ? "checkmark.circle" : "xmark.circle")
                        .foregroundStyle(run.outcome == "skipped" ? SessionStatus.needsYou.color : ok ? Brand.green : SessionStatus.exited.color)
                    Text(whenText(run.at)).monospacedDigit()
                    if run.catch_up { Text("caught up").foregroundStyle(.secondary) }
                    Text(run.reason ?? run.summary?.firstLine ?? run.event?.title ?? "").foregroundStyle(.secondary).lineLimit(1).help(run.reason ?? run.summary ?? "")
                    Spacer()
                    if let id = run.session, model.sessions.contains(where: { $0.id == id }) {
                        Button("Show") { model.select(id); model.editingTask = nil }.buttonStyle(.link)
                    }
                }
                .font(.callout)
            }
        }
    }
}
