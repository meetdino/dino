import SwiftUI

/// How a PR's state reads at a glance: one icon, one color, a few words.
struct PRLook {
    let icon: String
    let color: Color
    let label: String

    static let merged = Color(red: 0.66, green: 0.5, blue: 1.0)

    init(_ pr: PrInfo) {
        let c = pr.checks
        switch pr.state {
        case "merged": (icon, color, label) = ("arrow.triangle.merge", Self.merged, "Merged")
        case "closed": (icon, color, label) = ("xmark.circle", .secondary, "Closed")
        default:
            if c.failed > 0 {
                (icon, color, label) = ("xmark.circle.fill", SessionStatus.exited.color, "\(c.failed) failing")
            } else if c.pending > 0 {
                (icon, color, label) = ("circle.dashed", .orange, "Checks running")
            } else if c.passed > 0 {
                (icon, color, label) = ("checkmark.circle.fill", Brand.green, "Checks passed")
            } else {
                (icon, color, label) = ("arrow.triangle.pull", .secondary, pr.draft ? "Draft" : "Open")
            }
        }
    }
}

/// "#12" in a session's sidebar row, tinted by its checks.
struct PRChip: View {
    let pr: PrInfo
    var auto: AutoPr?

    var body: some View {
        let look = PRLook(pr)
        let automatic = pr.isOpen && auto?.any == true
        HStack(spacing: 3) {
            Image(systemName: look.icon)
            Text("#\(pr.number)").monospacedDigit()
            if automatic {
                Text("auto").font(.caption2.weight(.medium)).foregroundStyle(.secondary)
            }
        }
        .font(.caption)
        .foregroundStyle(look.color)
        .help("PR #\(pr.number): \(pr.title) · \(look.label)" + (automatic ? " · \(Self.describe(auto!))" : ""))
    }

    static func describe(_ a: AutoPr) -> String {
        switch (a.fix, a.merge) {
        case (true, true): "fixes failing checks and merges when they pass"
        case (true, false): "fixes failing checks"
        default: "merges when checks pass"
        }
    }
}

/// The toolbar's PR button, once the branch has one: its number and state. Creating one is in the
/// Session menu and the command palette.
struct PRToolbarButton: View {
    @EnvironmentObject var model: DinoModel

    var body: some View {
        let session = model.selectedSession
        let pr = session.flatMap { model.pr(of: $0) }
        Group {
            if let pr {
                let look = PRLook(pr)
                Button { model.showPR.toggle() } label: {
                    Label {
                        Text("#\(pr.number)")
                    } icon: {
                        Image(systemName: look.icon).foregroundStyle(look.color)
                    }
                    .labelStyle(.titleAndIcon)
                }
                .help("PR #\(pr.number) · \(look.label)")
            }
        }
        .popover(isPresented: $model.showPR, arrowEdge: .bottom) {
            if let session, let pr {
                PRPopover(session: session, pr: pr).environmentObject(model)
            }
        }
    }
}

struct PRPopover: View {
    @EnvironmentObject var model: DinoModel
    let session: SessionInfo
    let pr: PrInfo
    @State private var busy = false
    @State private var confirmMerge = false
    @State private var error: String?

    var body: some View {
        let look = PRLook(pr)
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Image(systemName: look.icon).foregroundStyle(look.color)
                VStack(alignment: .leading, spacing: 2) {
                    Text(pr.title).font(.headline).lineLimit(2)
                    Text("#\(pr.number) · \(stateLabel)").font(.caption).foregroundStyle(.secondary)
                }
            }
            if pr.isOpen {
                VStack(alignment: .leading, spacing: 5) {
                    checks
                    if let review = pr.review { reviewLine(review) }
                }
                .font(.callout)
                automation
            }
            if let error {
                Text(error).font(.callout).foregroundStyle(SessionStatus.exited.color).fixedSize(horizontal: false, vertical: true)
            }
            HStack(spacing: 8) {
                Button("Open on GitHub") {
                    if let url = URL(string: pr.url) { NSWorkspace.shared.open(url) }
                }
                Spacer()
                if busy { ProgressView().controlSize(.small) }
                if pr.isOpen, pr.checks.failed > 0, session.agent_id != "shell" {  // a shell has no one to read it
                    Button("Ask \(session.display) to Fix") { run { try await model.fixPR(session.id) } }
                        .help("Paste the failing checks' logs into \(session.display) and ask it to fix them")
                }
                if pr.canMerge {
                    Button("Merge") { confirmMerge = true }
                        .buttonStyle(.borderedProminent)
                        .tint(Brand.green)
                }
                if !pr.isOpen, let w = model.dinoWorktree(of: session) {
                    Button("Close Worktree…") {
                        model.showPR = false
                        model.closingWorktree = ClosingWorktree(path: w.path, label: w.branch ?? session.display, apply: false)
                    }
                    .help("The PR is \(pr.state): stop the session and remove its worktree and branch")
                }
            }
            .disabled(busy)
        }
        .padding(16)
        .frame(width: 360)
        .confirmationDialog("Squash and merge PR #\(pr.number)?", isPresented: $confirmMerge) {
            Button("Squash and Merge") { run { try await model.mergePR(session.id) } }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Its commits land on the base branch on GitHub as one.")
        }
    }

    private var stateLabel: String {
        switch pr.state {
        case "merged": "Merged"
        case "closed": "Closed"
        default: pr.draft ? "Draft" : "Open"
        }
    }

    /// Claude desktop's Auto-fix and Auto-merge: dinod acts on the next poll, whether or not the app is open.
    @ViewBuilder
    private var automation: some View {
        let auto = session.auto
        VStack(alignment: .leading, spacing: 6) {
            if session.agent_id != "shell" {  // a shell has no one to read the logs
                autoRow(
                    "Auto-fix failing checks",
                    detail: auto.flatMap { $0.fix && $0.fixes > 0 ? "\($0.fixes) of 3 asked" : nil },
                    help: "When checks fail, paste their logs into \(session.display) and ask it to fix them: once per push, up to three times",
                    isOn: Binding(get: { auto?.fix ?? false }, set: { on in run { try await model.setAutoPR(session.id, fix: on) } })
                )
            }
            autoRow(
                "Auto-merge when checks pass",
                detail: nil,
                help: "Squash-merge once every check passes, unless a reviewer asked for changes",
                isOn: Binding(get: { auto?.merge ?? false }, set: { on in run { try await model.setAutoPR(session.id, merge: on) } })
            )
            if let note = auto?.note {
                Label(note, systemImage: "exclamationmark.triangle.fill")
                    .font(.caption)
                    .foregroundStyle(.orange)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .disabled(auto == nil)  // an older dinod
        .padding(.top, 2)
    }

    private func autoRow(_ title: String, detail: String?, help: String, isOn: Binding<Bool>) -> some View {
        HStack(spacing: 6) {
            Text(title).font(.callout)
            if let detail { Text(detail).font(.caption).foregroundStyle(.secondary) }
            Spacer()
            Toggle(title, isOn: isOn).labelsHidden().toggleStyle(.switch).controlSize(.mini)
        }
        .help(help)
    }

    @ViewBuilder
    private var checks: some View {
        let c = pr.checks
        if c.passed + c.failed + c.pending == 0 {
            Text("No checks").foregroundStyle(.secondary)
        } else {
            if c.failed > 0 {
                line("xmark.circle.fill", SessionStatus.exited.color, "\(c.failed) failed: \(c.failing.joined(separator: ", "))")
            }
            if c.pending > 0 {
                line("circle.dashed", .orange, "\(c.pending) running")
            }
            if c.passed > 0 {
                line("checkmark.circle.fill", Brand.green, "\(c.passed) passed")
            }
        }
    }

    @ViewBuilder
    private func reviewLine(_ review: String) -> some View {
        switch review {
        case "approved": line("person.fill.checkmark", Brand.green, "Approved")
        case "changes_requested": line("person.fill.xmark", SessionStatus.exited.color, "Changes requested")
        default: line("person", .secondary, "Review required")
        }
    }

    private func line(_ icon: String, _ color: Color, _ text: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            Image(systemName: icon).foregroundStyle(color).frame(width: 16)
            Text(text).lineLimit(2)
        }
    }

    private func run(_ action: @escaping () async throws -> Void) {
        busy = true
        error = nil
        Task {
            do { try await action() } catch { self.error = error.localizedDescription }
            busy = false
        }
    }
}

struct CreatePRSheet: View {
    @EnvironmentObject var model: DinoModel
    @Environment(\.dismiss) private var dismiss
    let session: SessionInfo
    @State private var draft: PrDraft?
    @State private var title = ""
    @State private var body_ = ""
    @State private var base = ""
    @State private var isDraft = false
    @State private var creating = false
    @State private var error: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Label("Create pull request", systemImage: "arrow.triangle.pull").font(.title2.weight(.semibold))
            if let d = draft {
                if let note = d.note {
                    Text(note).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                    HStack {
                        Spacer()
                        Button("OK") { dismiss() }.keyboardShortcut(.defaultAction)
                    }
                } else {
                    form(d)
                }
            } else if let error {
                Text(error).foregroundStyle(SessionStatus.exited.color).font(.callout)
                HStack {
                    Spacer()
                    Button("OK") { dismiss() }.keyboardShortcut(.defaultAction)
                }
            } else {
                ProgressView().controlSize(.small).frame(maxWidth: .infinity, minHeight: 80)
            }
        }
        .padding(22)
        .frame(width: 560)
        .task {
            do {
                let d = try await model.prDraft(session.id)
                (title, body_, base) = (d.title, d.body, d.base)
                draft = d
            } catch {
                self.error = error.localizedDescription
            }
        }
    }

    @ViewBuilder
    private func form(_ d: PrDraft) -> some View {
        HStack(spacing: 6) {
            Image(systemName: "arrow.triangle.branch").foregroundStyle(.secondary)
            Text(d.branch).font(.system(.callout, design: .monospaced))
            Image(systemName: "arrow.right").foregroundStyle(.tertiary)
            TextField("base", text: $base)
                .font(.system(.callout, design: .monospaced))
                .textFieldStyle(.roundedBorder)
                .frame(width: 140)
            Spacer()
            if d.commits > 0 {
                Text("\(d.commits) commit\(d.commits == 1 ? "" : "s")").font(.callout).foregroundStyle(.secondary)
            }
        }
        TextField("Title", text: $title)
            .textFieldStyle(.roundedBorder)
            .font(.body)
        TextEditor(text: $body_)
            .font(.body)
            .scrollContentBackground(.hidden)
            .padding(8)
            .frame(height: 150)
            .background(RoundedRectangle(cornerRadius: 8).fill(Color(nsColor: .textBackgroundColor)))
            .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(.quaternary))
            .overlay(alignment: .topLeading) {
                if body_.isEmpty {
                    Text("Description (optional)").foregroundStyle(.tertiary).padding(13).allowsHitTesting(false)
                }
            }
        Toggle("Draft", isOn: $isDraft)
            .help("Open it as a draft: not ready to review or merge yet")
        if d.uncommitted > 0 {
            Label("Commits \(d.uncommitted) changed file\(d.uncommitted == 1 ? "" : "s") as “\(title)” first", systemImage: "info.circle")
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        if let error {
            Text(error).foregroundStyle(SessionStatus.exited.color).font(.callout).fixedSize(horizontal: false, vertical: true)
        }
        HStack {
            Spacer()
            Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
            Button {
                create()
            } label: {
                if creating { ProgressView().controlSize(.small).frame(width: 90) } else { Text("Create PR").frame(width: 90) }
            }
            .keyboardShortcut(.return, modifiers: .command)
            .buttonStyle(.borderedProminent)
            .tint(Brand.green)
            .disabled(title.trimmingCharacters(in: .whitespaces).isEmpty || base.trimmingCharacters(in: .whitespaces).isEmpty || creating)
        }
    }

    private func create() {
        creating = true
        error = nil
        Task {
            do {
                try await model.createPR(session.id, title: title, body: body_, base: base, draft: isDraft)
                dismiss()
                model.showPR = true
            } catch {
                self.error = error.localizedDescription
                creating = false
            }
        }
    }
}
