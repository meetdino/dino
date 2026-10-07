import AppKit
import SwiftUI

/// A ready automation to start from: a trigger and an action that exist in dinod, with a prompt
/// that does the job. Applying one only fills in an editor; nothing is saved until the user says.
struct AutomationTemplate: Identifiable {
    enum Group: String, CaseIterable {
        case github = "Pull requests and issues"
        case local = "While you work"
        case timed = "On a schedule"

        var tint: Color {
            switch self {
            case .github: .indigo
            case .local: Brand.green
            case .timed: .orange
            }
        }
    }

    let id: String
    let group: Group
    let icon: String
    let title: String
    let blurb: String
    let apply: (inout ScheduledTask, TemplateContext) -> Void

    /// `task` (the folder and agent already chosen) set up as this template says.
    func task(_ base: ScheduledTask, _ context: TemplateContext) -> ScheduledTask {
        var t = base
        t.name = title
        t.trigger = AutomationTrigger()
        t.action = AutomationAction()
        t.conditions = AutomationConditions()
        t.output = AutomationOutput()
        t.frequency = Frequency()
        apply(&t, context)
        return t
    }
}

/// What a template can know about where it's set up.
struct TemplateContext {
    /// How the project in the folder runs its tests, when it can be told from its files.
    var testCommand: String?
    /// Another automation to come after, for a chain.
    var previous: String?

    init(folder: String, previous: String?) {
        self.previous = previous
        testCommand = Self.testCommand(in: folder)
    }

    static func testCommand(in folder: String) -> String? {
        let has = { (f: String) in FileManager.default.fileExists(atPath: (folder as NSString).appendingPathComponent(f)) }
        if has("Cargo.toml") { return "cargo test" }
        if has("Package.swift") { return "swift test" }
        if has("go.mod") { return "go test ./..." }
        if has("package.json") { return "npm test" }
        if has("pyproject.toml") || has("pytest.ini") || has("setup.py") { return "python3 -m pytest -q" }
        if has("Gemfile") { return "bundle exec rake test" }
        if has("Makefile") { return "make test" }
        return nil
    }
}

extension AutomationTemplate {
    static let all: [AutomationTemplate] = [
        AutomationTemplate(
            id: "ci-fix", group: .github, icon: "wrench.and.screwdriver", title: "Fix CI when it fails on my PRs",
            blurb: "A check fails on a pull request you opened: an agent finds out why, fixes it and pushes to the PR."
        ) { t, _ in
            t.trigger.on = "ci_failed"
            t.trigger.mine = true
            t.worktree = true
            t.output.pr_comment = true
            t.prompt = """
            CI failed on {ci.branch} ({pr.url}): {ci.check}. Details: {ci.log}

            Start from that branch: `git fetch origin {ci.branch} && git reset --hard FETCH_HEAD`. Find out why it failed (for GitHub Actions, `gh run view --log-failed` shows the log), fix the cause and run the check locally if you can. Then commit and `git push origin HEAD:{ci.branch}`. Say in a few lines what broke and what you changed.
            """
        },
        AutomationTemplate(
            id: "review-new", group: .github, icon: "text.magnifyingglass", title: "Review every new pull request",
            blurb: "A pull request opens: an agent reads the diff and posts what it found as a comment on the PR."
        ) { t, _ in
            t.trigger.on = "pr_opened"
            t.worktree = false
            t.output.pr_comment = true
            t.prompt = """
            Review pull request #{pr.number} "{pr.title}" by {pr.author}: {pr.url}

            Read it with `gh pr view {pr.number}` and `gh pr diff {pr.number}`. Look for bugs, missing tests and anything risky. Don't change any files. Reply with a short review: the problems worth fixing, most important first, each with its file and line.
            """
        },
        AutomationTemplate(
            id: "answer-mention", group: .github, icon: "at", title: "Answer when someone writes @dino",
            blurb: "A comment on an issue or PR mentions @dino: an agent does what it asks and replies in the thread."
        ) { t, _ in
            t.trigger.on = "comment"
            t.trigger.phrase = "@dino"
            t.worktree = true
            t.output.pr_comment = true
            t.prompt = """
            {comment.author} wrote on #{issue.number} "{issue.title}" ({comment.url}):

            {comment.body}

            Do what they ask. If it takes code changes, make them, commit, push the branch and open a pull request that mentions #{issue.number}. Your last message is posted as the reply, so write it to them.
            """
        },
        AutomationTemplate(
            id: "triage", group: .github, icon: "ladybug", title: "Triage issues labeled bug",
            blurb: "An issue gets the bug label: an agent looks for the cause and replies on the issue with what it found."
        ) { t, _ in
            t.trigger.on = "issue_labeled"
            t.trigger.label = "bug"
            t.worktree = false
            t.output.pr_comment = true
            t.prompt = """
            Issue #{issue.number} "{issue.title}" was labeled {label}: {issue.url}

            Read it with `gh issue view {issue.number} --comments`. Find where in the code the problem most likely is and how to reproduce it. Don't change any files. Reply with the likely cause (file and line), how to reproduce it, a suggested fix, and whether it looks like a duplicate.
            """
        },
        AutomationTemplate(
            id: "merged-docs", group: .github, icon: "arrow.triangle.merge", title: "Keep the docs in step with merged PRs",
            blurb: "A pull request merges: an agent checks the docs it touches still match the code, and opens a PR if not."
        ) { t, _ in
            t.trigger.on = "pr_merged"
            t.worktree = true
            t.prompt = """
            Pull request #{pr.number} "{pr.title}" just merged into {pr.base}: {pr.url}

            Start from it: `git fetch origin {pr.base} && git reset --hard origin/{pr.base}`. Read what it changed (`gh pr diff {pr.number}`) and check that the README, the docs and the comments near what it changed still describe the code. If something is out of date, fix it, commit, push the branch and open a pull request. If everything still matches, change nothing and say so.
            """
        },
        AutomationTemplate(
            id: "review-requested", group: .github, icon: "eye", title: "First pass when my review is requested",
            blurb: "Someone asks for your review: an agent reads the PR and leaves notes for you here, not on GitHub."
        ) { t, _ in
            t.trigger.on = "review_requested"
            t.worktree = false
            t.prompt = """
            My review was requested on {pr.url} ("{pr.title}" by {pr.author}).

            Read it with `gh pr view {pr.url}` and `gh pr diff {pr.url}`. Don't change any files and don't post anything. Give me a first pass: what it does in two lines, then what I should look at closely, most important first, each with its file and line.
            """
        },
        AutomationTemplate(
            id: "tests", group: .local, icon: "checkmark.seal", title: "Run tests when files change, fix failures",
            blurb: "You save a file: dino runs your tests, and if they fail, an agent fixes what broke."
        ) { t, c in
            t.trigger.on = "files"
            t.action.kind = "command"
            t.action.command = c.testCommand ?? "make test"
            t.action.then_agent = "failure"
            t.worktree = false
            t.output.notify = false
            t.prompt = """
            The tests failed after {files} changed:

            {cmd.output}

            Find the cause and fix it. Change the code, not the tests, unless a test is wrong. Run the tests again to check, and say in a line what you fixed.
            """
        },
        AutomationTemplate(
            id: "rebase", group: .local, icon: "arrow.triangle.branch", title: "Keep my branch rebased on main",
            blurb: "New commits land on the default branch: dino rebases your checkout onto them. On conflicts, an agent resolves them."
        ) { t, _ in
            t.trigger.on = "new_commits"
            t.action.kind = "command"
            t.action.command = "git rebase --autostash origin/{branch}"
            t.action.then_agent = "failure"
            t.worktree = false
            t.prompt = """
            Rebasing onto origin/{branch} stopped:

            {cmd.output}

            If it stopped on conflicts, resolve each one keeping what both sides meant, `git add` the files and `git rebase --continue` until it's done, then run the tests. If it refused to start (uncommitted changes, a rebase already going), change nothing and say why.
            """
        },
        AutomationTemplate(
            id: "review-after", group: .local, icon: "arrow.turn.down.right", title: "Review an agent's work when it finishes",
            blurb: "Another automation or a session finishes: a second agent reviews the changes it made."
        ) { t, c in
            t.trigger.on = "after"
            t.trigger.after = c.previous ?? ""
            t.trigger.when = "success"
            t.worktree = false
            t.prompt = """
            {after.name} just finished. Review the changes it made: `git -C {after.dir} diff {after.base}` (and `git -C {after.dir} log {after.base}..` for its commits).

            Look for bugs, missing tests and anything it left half done. Don't change any files. Reply with what should be fixed, most important first.
            """
        },
        AutomationTemplate(
            id: "deps", group: .timed, icon: "shippingbox", title: "Update dependencies nightly and open a PR",
            blurb: "Every night: an agent updates dependencies in a worktree, runs the tests, and opens a pull request if they pass."
        ) { t, _ in
            t.frequency.every = "daily"
            t.frequency.hour = 3
            t.frequency.minute = 0
            t.worktree = true
            t.conditions.ac_power = true
            t.prompt = """
            Update this project's dependencies to their newest compatible versions. Run the build and the tests. If they pass, commit, push the branch and open a pull request listing what changed. If an update breaks something, keep only the ones that work. If nothing needs updating, change nothing and say so.
            """
        },
        AutomationTemplate(
            id: "digest", group: .timed, icon: "sun.horizon", title: "Every morning, summarize what changed",
            blurb: "Each weekday at 9: an agent reads the new commits and pull requests and tells you what matters."
        ) { t, _ in
            t.frequency.every = "weekdays"
            t.frequency.hour = 9
            t.frequency.minute = 0
            t.worktree = false
            t.prompt = """
            Run `git fetch --quiet`, then summarize what changed in this repo since yesterday morning: the commits on the default branch (`git log --since=yesterday origin/HEAD`) and, if `gh` works here, the pull requests opened or merged (`gh pr list --state all --limit 30`). Group it by area, say who did what, and point out anything that needs my attention. Don't change any files.
            """
        },
    ]
}

extension DinoModel {
    /// The editor, with `template` set up for the current folder and agent.
    func newTask(from template: AutomationTemplate) {
        editingTask = templated(template, base: blankTask())
    }

    func blankTask() -> ScheduledTask {
        var t = ScheduledTask()
        t.cwd = folder.path
        t.launcher = launchers.first { $0.agent_id != "shell" }?.short ?? launchers.first?.short ?? ""
        return t
    }

    func templated(_ template: AutomationTemplate, base: ScheduledTask) -> ScheduledTask {
        var t = template.task(base, TemplateContext(folder: base.cwd, previous: scheduled.last?.id))
        // Names are unique: a second one from the same template gets a number.
        let taken = Set(scheduled.map { $0.name.lowercased() })
        if taken.contains(t.name.lowercased()) {
            t.name = (2...).lazy.map { "\(t.name) \($0)" }.first { !taken.contains($0.lowercased()) } ?? t.name
        }
        return t
    }
}

/// The GitHub repo (owner/name) a folder is a clone of, from its origin; nil when it isn't one.
func githubRepo(of folder: String) async -> String? {
    await Task.detached {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/usr/bin/env")
        p.arguments = ["git", "-C", folder, "remote", "get-url", "origin"]
        let out = Pipe()
        p.standardOutput = out
        p.standardError = FileHandle.nullDevice
        guard (try? p.run()) != nil else { return nil }
        let data = out.fileHandleForReading.readDataToEndOfFile()
        p.waitUntilExit()
        guard p.terminationStatus == 0, let url = String(data: data, encoding: .utf8)?.trimmingCharacters(in: .whitespacesAndNewlines) else { return nil }
        guard let range = url.range(of: "github.com") else { return nil }
        var rest = url[range.upperBound...].drop { $0 == "/" || $0 == ":" }
        if rest.hasSuffix(".git") { rest = rest.dropLast(4) }
        let parts = rest.split(separator: "/")
        return parts.count == 2 ? parts.joined(separator: "/") : nil
    }.value
}

// MARK: - Gallery

/// Where a new automation starts: a ready one to adjust, or a blank one.
struct TemplateGallery: View {
    @EnvironmentObject var model: DinoModel
    let folder: String
    let repo: String?
    let pick: (AutomationTemplate?) -> Void
    let changeFolder: () -> Void

    private let columns = [GridItem(.flexible(), spacing: 10), GridItem(.flexible(), spacing: 10)]

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            VStack(alignment: .leading, spacing: 4) {
                Text("New automation").font(.title2.weight(.semibold))
                Text("dino does something by itself when something happens. Start from one of these and change anything before it's saved.")
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            HStack(spacing: 6) {
                Image(systemName: "folder").foregroundStyle(.secondary)
                Text(shortPath(folder)).lineLimit(1).truncationMode(.middle)
                if let repo {
                    Text("· \(repo)").foregroundStyle(.secondary).lineLimit(1)
                }
                Button("Change…", action: changeFolder).buttonStyle(.link)
            }
            .font(.callout)
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    ForEach(AutomationTemplate.Group.allCases, id: \.self) { group in
                        VStack(alignment: .leading, spacing: 8) {
                            HStack(spacing: 6) {
                                Text(group.rawValue.uppercased()).font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                                if group == .github, repo == nil {
                                    Text("· this folder isn't a GitHub clone: you'll name the repo")
                                        .font(.caption).foregroundStyle(.tertiary)
                                }
                            }
                            LazyVGrid(columns: columns, alignment: .leading, spacing: 10) {
                                ForEach(AutomationTemplate.all.filter { $0.group == group }) { t in
                                    TemplateCard(template: t) { pick(t) }
                                }
                            }
                        }
                    }
                    Button { pick(nil) } label: {
                        HStack(spacing: 10) {
                            Image(systemName: "plus").frame(width: 28, height: 28)
                                .background(RoundedRectangle(cornerRadius: 7).strokeBorder(.tertiary, style: StrokeStyle(lineWidth: 1, dash: [3])))
                            VStack(alignment: .leading, spacing: 2) {
                                Text("Start from scratch").font(.callout.weight(.medium))
                                Text("Any trigger: a schedule, GitHub, git, files or another run. Any action: an agent, a session, a command.")
                                    .font(.caption).foregroundStyle(.secondary)
                            }
                            Spacer(minLength: 0)
                        }
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                }
                .padding(.trailing, 4)
            }
            .frame(maxHeight: 540)
            .fixedSize(horizontal: false, vertical: true)
        }
    }
}

private struct TemplateCard: View {
    let template: AutomationTemplate
    let action: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            HStack(alignment: .top, spacing: 10) {
                Image(systemName: template.icon)
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(.white)
                    .frame(width: 28, height: 28)
                    .background(RoundedRectangle(cornerRadius: 7).fill(template.group.tint.gradient))
                VStack(alignment: .leading, spacing: 3) {
                    Text(template.title).font(.callout.weight(.semibold)).lineLimit(2).fixedSize(horizontal: false, vertical: true)
                    Text(template.blurb).font(.caption).foregroundStyle(.secondary).lineLimit(3).fixedSize(horizontal: false, vertical: true)
                }
                Spacer(minLength: 0)
            }
            .padding(10)
            .frame(maxWidth: .infinity, minHeight: 78, alignment: .topLeading)
            .background(RoundedRectangle(cornerRadius: 9).fill(hovering ? AnyShapeStyle(.quaternary) : AnyShapeStyle(.background.secondary)))
            .overlay(RoundedRectangle(cornerRadius: 9).strokeBorder(hovering ? template.group.tint.opacity(0.6) : Color.primary.opacity(0.08)))
            .contentShape(RoundedRectangle(cornerRadius: 9))
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .accessibilityLabel(template.title)
        .accessibilityHint(template.blurb)
    }
}
