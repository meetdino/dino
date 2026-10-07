import SwiftUI

/// A prompt an agent scheduled for itself (Claude's CronCreate, `/loop`), shown under Automations
/// with the session it belongs to. It's the agent's, not dino's: dino doesn't run, change or keep
/// it, and the row goes when the agent deletes it, has run it (once), lets it expire or exits.
struct AgentCron: Identifiable, Equatable {
    let session: SessionInfo
    let cron: CronItem

    /// The row's sidebar tag; a click goes to the session.
    var id: String { "cron:\(session.id):\(cron.id)" }
}

/// When a scheduled prompt is next due, as `whenText` says an automation's next run ("today
/// 1:23 PM", "Thu 11:59 PM"), but with the date beyond the week ahead: an agent's cron can be months
/// out (Dec 31), where a weekday alone would name the wrong week.
func cronWhen(_ secs: UInt64, now: Date = Date()) -> String {
    let date = Date(timeIntervalSince1970: TimeInterval(secs))
    guard date.timeIntervalSince(now) >= 6 * 86400 else { return whenText(secs) }
    let time = date.formatted(date: .omitted, time: .shortened)
    return "\(date.formatted(.dateTime.month(.abbreviated).day())) \(time)"
}

extension DinoModel {
    /// What the sidebar's sessions have scheduled for themselves, session by session as they're
    /// listed, each one's soonest first.
    var agentCrons: [AgentCron] {
        sidebarSessions.filter { !$0.exited }.flatMap { s in
            (s.tasks?.crons ?? [])
                .sorted { ($0.next_due ?? .max, $0.id) < ($1.next_due ?? .max, $1.id) }
                .map { AgentCron(session: s, cron: $0) }
        }
    }

    func cronFacts(_ c: AgentCron) -> CronFacts {
        let s = c.session
        return CronFacts(
            prompt: c.cron.prompt,
            agent: launchers.first { $0.agent_id == s.agent }?.label ?? s.agentWord,
            session: tabName(s),
            schedule: c.cron.schedule,
            human: c.cron.human,
            recurring: c.cron.recurring,
            next: c.cron.next_due.map { cronWhen($0) }
        )
    }
}

/// What an agent's scheduled prompt's row says: on the row, the prompt and when it's next due; the rest
/// in its tooltip and to VoiceOver. Only what the agent said and what its cron expression reads.
struct CronFacts: Equatable {
    var prompt: String
    var agent: String
    var session: String
    var schedule: String
    var human: String?
    var recurring: Bool
    /// When its expression next matches ("today 12:23 PM").
    var next: String?

    /// The prompt's first line: the row's name.
    var title: String {
        let line = prompt.split(whereSeparator: \.isNewline).first.map(String.init) ?? ""
        return line.isEmpty ? "Scheduled prompt" : line
    }

    /// "Every hour at :23 (23 * * * *)", or the expression alone when that's all the agent said.
    var when: String {
        let how = human.flatMap { $0.isEmpty || $0 == schedule ? nil : "\($0) (\(schedule))" } ?? schedule
        return recurring ? how : "Once, at \(how)"
    }

    var tooltip: [String] {
        var lines = [prompt.count > 300 ? String(prompt.prefix(300)) + "…" : prompt]
        lines.append("Scheduled by \(agent) in “\(session)”")
        lines.append(when)
        // The agent starts it a little after the time its expression gives (Claude's jitter).
        if let next { lines.append("Next due about \(next)") }
        lines.append("It's the session's own: it goes when \(agent) deletes it or the session ends.")
        return lines
    }

    var help: String { (tooltip + ["Click to go to the session."]).joined(separator: "\n") }

    var accessibility: String {
        (["Scheduled prompt: \(title)", "\(agent) in \(session)", when] + (next.map { ["next due about \($0)"] } ?? [])).joined(separator: ", ")
    }
}

/// One line: a clock (a loop when it repeats), the prompt, and when it's next due.
struct AgentCronRow: View {
    @EnvironmentObject var model: DinoModel
    let item: AgentCron

    var body: some View {
        let facts = model.cronFacts(item)
        HStack(spacing: 6) {
            Image(systemName: item.cron.recurring ? "clock.arrow.circlepath" : "alarm")
                .font(.callout)
                .foregroundStyle(.secondary)
                .frame(width: 14)
            Text(facts.title).lineLimit(1).truncationMode(.tail)
                .foregroundStyle(.secondary)
            Spacer(minLength: 4)
            // When it's next due, as dino's own automations say their next run. The session is in
            // the tooltip; a click goes to it.
            if let next = item.cron.next_due {
                Text(cronWhen(next)).font(.caption.monospacedDigit()).foregroundStyle(.secondary).fixedSize()
            }
        }
        .padding(.vertical, 3)
        .contentShape(Rectangle())
        .help(facts.help)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(facts.accessibility)
        .accessibilityAddTraits(.isButton)
        .contextMenu {
            Button("Go to Session") { model.select(item.session.id) }
            Button("Copy Prompt") {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(item.cron.prompt, forType: .string)
            }
        }
    }
}
