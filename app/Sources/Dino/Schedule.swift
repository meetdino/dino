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

/// A prompt dinod sends to a new session of `launcher` on a schedule.
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
    }
}

struct ScheduledRun: Codable, Equatable {
    var at: UInt64
    /// The scheduled time it was for; nil for Run Now.
    var due: UInt64?
    var session: String?
    /// "started", "skipped" or "failed".
    var outcome: String
    var reason: String?
    /// Made up for a time missed while the Mac slept or dinod was down.
    var catch_up: Bool
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

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 8) {
                Image(systemName: task.enabled ? "clock" : "pause.circle")
                    .foregroundStyle(task.enabled ? Brand.green : .secondary)
                    .frame(width: 14)
                Text(task.name).lineLimit(1)
                Spacer()
                if let next = task.next_run {
                    Text(whenText(next)).font(.caption.monospacedDigit()).foregroundStyle(.secondary)
                } else if !task.enabled {
                    Text("Paused").font(.caption).foregroundStyle(.tertiary)
                }
            }
            if let last = task.history.last, last.outcome != "started" {
                Text(last.reason ?? last.outcome.capitalized)
                    .font(.caption)
                    .foregroundStyle(last.outcome == "failed" ? SessionStatus.exited.color : SessionStatus.needsYou.color)
                    .lineLimit(1)
                    .padding(.leading, 22)
                    .help(last.reason ?? "")
            } else {
                Text("\(task.frequency.label) · \(model.launcherLabel(task.launcher))")
                    .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                    .padding(.leading, 22)
            }
        }
        .padding(.vertical, 2)
        .opacity(task.enabled ? 1 : 0.7)
        .contextMenu { ScheduledMenu(task: task) }
    }
}

struct ScheduledMenu: View {
    @EnvironmentObject var model: DinoModel
    let task: ScheduledTask

    var body: some View {
        Button("Run Now") { model.runTask(task) }
        Button(task.enabled ? "Pause" : "Resume") { model.setTask(task, enabled: !task.enabled) }
        Button("Edit…") { model.editingTask = task }
        if let last = task.history.last(where: { $0.session != nil })?.session, model.sessions.contains(where: { $0.id == last }) {
            Button("Show Last Run") { model.select(last) }
        }
        Divider()
        Button("Delete…", role: .destructive) { model.deletingTask = task }
    }
}

// MARK: - Editor

struct ScheduleSheet: View {
    @EnvironmentObject var model: DinoModel
    @Environment(\.dismiss) private var dismiss
    @State var task: ScheduledTask
    @State private var saving = false
    @State private var error: String?
    @FocusState private var focused: Bool

    private var isNew: Bool { task.id.isEmpty }
    private var launcher: LauncherInfo? { model.launchers.first { $0.short == task.launcher } }
    private var isShell: Bool { launcher?.agent_id == "shell" }

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

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Label(isNew ? "New scheduled task" : "Edit scheduled task", systemImage: "clock").font(.title2.weight(.semibold))
            Text("dino starts a new session and sends it this prompt on schedule, while your Mac is awake. A time missed while it slept runs once when it wakes.")
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            VStack(alignment: .leading, spacing: 10) {
                HStack(spacing: 12) {
                    Text("Name").frame(width: 64, alignment: .trailing)
                    TextField("", text: $task.name, prompt: Text("Morning triage"))
                        .textFieldStyle(.roundedBorder)
                        .frame(maxWidth: .infinity)
                        .focused($focused)
                }
                HStack(spacing: 12) {
                    Text("Agent").frame(width: 64, alignment: .trailing)
                    HStack {
                        Picker("", selection: $task.launcher) {
                            ForEach(model.launchers) { l in Text(l.label).tag(l.short) }
                        }
                        .labelsHidden()
                        .fixedSize()
                        if !isShell {
                            TextField("", text: $task.args, prompt: Text("Options: --model haiku"))
                                .textFieldStyle(.roundedBorder)
                                .font(.system(.body, design: .monospaced))
                                .frame(maxWidth: .infinity)
                        }
                    }
                }
                HStack(spacing: 12) {
                    Text("Repeats").frame(width: 64, alignment: .trailing)
                    HStack(spacing: 10) {
                        Picker("", selection: $task.frequency.every) {
                            ForEach(Frequency.kinds, id: \.id) { Text($0.label).tag($0.id) }
                        }
                        .labelsHidden()
                        .fixedSize()
                        switch task.frequency.every {
                        case "manual":
                            Text("Only when you choose Run Now").foregroundStyle(.secondary)
                        case "hourly":
                            Text("at minute")
                            Picker("", selection: $task.frequency.minute) {
                                ForEach(Array(stride(from: 0, to: 60, by: 5)), id: \.self) { Text(String(format: ":%02d", $0)).tag($0) }
                            }
                            .labelsHidden()
                            .fixedSize()
                        default:
                            if task.frequency.every == "weekly" {
                                Picker("", selection: $task.frequency.weekday) {
                                    ForEach(0..<7, id: \.self) { Text(Calendar.current.weekdaySymbols[$0]).tag($0) }
                                }
                                .labelsHidden()
                                .fixedSize()
                            }
                            Text("at")
                            DatePicker("", selection: time, displayedComponents: .hourAndMinute)
                                .labelsHidden()
                                .fixedSize()
                        }
                    }
                }
                HStack(spacing: 12) {
                    Text("Folder").frame(width: 64, alignment: .trailing)
                    HStack(spacing: 6) {
                        Image(systemName: "folder").foregroundStyle(.secondary)
                        Text(shortPath(task.cwd)).lineLimit(1).truncationMode(.middle)
                        Button("Change…") { chooseFolder() }.buttonStyle(.link)
                    }
                }
                HStack(spacing: 12) {
                    Color.clear.frame(width: 64, height: 1)
                    Toggle("Run each time in a new worktree", isOn: $task.worktree)
                        .help("Each run gets its own branch and checkout, so runs don't step on your work or on each other.")
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
            TextEditor(text: $task.prompt)
                .font(.body)
                .scrollContentBackground(.hidden)
                .padding(8)
                .frame(height: 120)
                .background(RoundedRectangle(cornerRadius: 8).fill(Color(nsColor: .textBackgroundColor)))
                .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(.quaternary))
                .overlay(alignment: .topLeading) {
                    if task.prompt.isEmpty {
                        Text(isShell ? "A command to run (optional)" : "What should it do each time?")
                            .foregroundStyle(.tertiary).padding(13).allowsHitTesting(false)
                    }
                }
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
                    if saving { ProgressView().controlSize(.small).frame(width: 90) } else { Text(isNew ? "Schedule" : "Save").frame(width: 90) }
                }
                .keyboardShortcut(.return, modifiers: .command)
                .buttonStyle(.borderedProminent)
                .tint(Brand.green)
                .disabled(task.name.trimmingCharacters(in: .whitespaces).isEmpty || (!isShell && task.prompt.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty) || saving)
            }
        }
        .padding(22)
        .frame(width: 580)
        .onAppear { focused = isNew }
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
            ForEach(Array(runs.suffix(5).reversed().enumerated()), id: \.offset) { _, run in
                HStack(spacing: 6) {
                    Image(systemName: run.outcome == "started" ? "checkmark.circle" : run.outcome == "skipped" ? "forward.circle" : "xmark.circle")
                        .foregroundStyle(run.outcome == "started" ? Brand.green : run.outcome == "skipped" ? SessionStatus.needsYou.color : SessionStatus.exited.color)
                    Text(whenText(run.at)).monospacedDigit()
                    if run.catch_up { Text("caught up").foregroundStyle(.secondary) }
                    if run.due == nil { Text("run now").foregroundStyle(.secondary) }
                    Text(run.reason ?? "").foregroundStyle(.secondary).lineLimit(1).help(run.reason ?? "")
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
