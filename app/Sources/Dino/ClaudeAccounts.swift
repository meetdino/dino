import AppKit
import SwiftUI

/// One of your Claude accounts as dinod holds it: never its token.
struct ClaudeAccountInfo: Codable, Equatable, Identifiable {
    /// 1 for the account Claude Code signed in with, 2 on for the others.
    var number: UInt32
    /// Claude Code's calls go to this one now.
    var answering: Bool
    /// Found at its subscription limit, and not tried again yet.
    var spent: Bool
    var resets_at: UInt64?
    var retry_at: UInt64?
    /// Its usage windows as Anthropic last reported them on a call it signed; nil until then.
    var windows: [WindowInfo]?
    /// The name the user gave it, else for account 1 the email Claude Code is signed in with; nil
    /// for "Account 2". From an older dinod, always nil.
    var name: String?
    /// Account 1's email and plan ("max"), as `claude auth status` says; never known for the others.
    var email: String?
    var plan: String?

    var id: UInt32 { number }
    /// What it's called: "Work", "you@example.com", "Account 2".
    var label: String { name ?? "Account \(number)" }
    /// As the sidebar says it: "Work", or "Claude account 2" with no name. The same as the proxy
    /// names a session on it, so the two can be matched.
    var short: String { name ?? "Claude account \(number)" }
    /// When it can answer again: its reset, or else when dino tries it again.
    var backAt: UInt64? { resets_at ?? retry_at }
    var isOwn: Bool { number == 1 }
    /// Its plan as Anthropic names it: "Max", "Pro".
    var planName: String? { plan.map { $0.prefix(1).uppercased() + $0.dropFirst() } }

    /// Account 1 as the state last named it: for what doesn't hold the list, such as a session's
    /// "On Work · you@example.com back at 14:00". Set by the model as the state comes in.
    static var ownShort = "Claude account 1"
    /// "5h 27% · 7d 98% used", as Anthropic last reported them; nil until a call did.
    var usage: String? {
        let w = (windows ?? []).filter { $0.isWindow && !$0.isPast }
        guard !w.isEmpty else { return nil }
        return w.map { "\($0.name) \(Int((Double($0.utilization) * 100).rounded()))%" }.joined(separator: " · ") + " used"
    }
}

/// Adding an account by signing in to it in the browser: dinod runs Claude Code's own
/// `claude setup-token` out of sight and adds the token it prints. Never the token.
struct ClaudeLoginInfo: Codable, Equatable {
    var id: UInt64
    /// `starting`, `browser`, `checking`, `added`, `failed` or `cancelled`.
    var stage: String
    /// claude.ai's sign-in page, for another browser or a private window: it ends on a code to paste.
    var url: String?
    var account: UInt32?
    var error: String?
    /// Failed because that account is here already: its number (1 for Claude Code's own).
    var duplicate: UInt32?

    var underWay: Bool { ["starting", "browser", "checking"].contains(stage) }
}

struct ClaudeAccountsInfo: Codable, Equatable {
    var accounts: [ClaudeAccountInfo]
    var added: UInt32?
    /// The shell running `claude setup-token`.
    var creating: String?
    var error: String?
    /// The browser sign-in under way, or how the last one ended.
    var login: ClaudeLoginInfo?
}

private struct ClaudeAccountsResponse: Decodable {
    var accounts: ClaudeAccountsInfo
}

extension DinoConnection {
    /// `status`, `add` (`value`: a token), `login` (sign in in the browser), `login_code` (`value`:
    /// the code claude.ai showed), `login_cancel`, `rename` (`account`, `value`: its name, empty
    /// for none), `remove` (`account`) or `order` (`order`).
    func claudeAccounts(_ action: String, value: String? = nil, account: UInt32? = nil, order: [UInt32] = []) throws -> ClaudeAccountsInfo {
        var body: [String: Any] = ["type": "claude_accounts", "action": action]
        if let value { body["value"] = value }
        if let account { body["account"] = account }
        if !order.isEmpty { body["order"] = order }
        return try JSONDecoder().decode(ClaudeAccountsResponse.self, from: send(body)).accounts
    }
}

/// Settings → Accounts: your Claude accounts, which Claude Code goes on with when the one it
/// signed in with is at its limit. Which answers now, how much each has used, and when a spent one
/// resets. Add Account… signs in to another in the browser: the one way to add one.
struct ClaudeAccountsSection: View {
    @State private var info: ClaudeAccountsInfo?
    @State private var adding = false
    @State private var renaming: ClaudeAccountInfo?
    @State private var error: String?

    private var others: [ClaudeAccountInfo] { info?.accounts.filter { !$0.isOwn } ?? [] }
    private var signingIn: Bool { info?.login?.underWay == true }

    var body: some View {
        // One row holding them all: the grouped form is a list, which lost row views when an
        // account's row went in among rows already laid out (drawn outside the group, or not at
        // all). The buttons sit below, as System Settings puts a list's.
        Section {
            VStack(spacing: 0) {
                ForEach(Array((info?.accounts ?? []).enumerated()), id: \.element.number) { i, a in
                    if i > 0 { Divider().padding(.vertical, 8) }
                    row(a)
                }
            }
        } header: {
            Text("Claude Accounts")
        } footer: {
            VStack(alignment: .leading, spacing: 8) {
                if let e = error ?? info?.error {
                    Text(e).font(.callout).foregroundStyle(.red).padding(.leading, 10)
                }
                HStack(alignment: .firstTextBaseline) {
                    Footnote("When an account reaches its usage limit, Claude Code switches to the next account in the list until the limit resets.")
                    Button("Add Account…") { adding = true }
                        .disabled(info == nil)
                }
            }
        }
        .task { refresh() }
        // What the proxy finds (a limit, a reset) shows while this is on screen; while a sign-in
        // runs, much sooner, so the sheet follows it.
        .task(id: signingIn) {
            while !Task.isCancelled {
                try? await Task.sleep(for: signingIn ? .milliseconds(500) : .seconds(5))
                refresh()
            }
        }
        .sheet(isPresented: $adding) {
            AddClaudeAccountSheet(
                info: info,
                call: { action, value, account, done in call(action, value: value, account: account, then: done) },
                close: { adding = false }
            )
        }
        .sheet(item: $renaming) { a in
            NameClaudeAccount(account: a, initial: a.label, title: "Rename \(a.label)", skip: "Cancel", standalone: true) { name in
                call("rename", value: name, account: a.number) { r in
                    if case .success = r { renaming = nil }
                }
            } close: {
                renaming = nil
            }
        }
    }

    private func row(_ a: ClaudeAccountInfo) -> some View {
        HStack(spacing: 10) {
            Image(systemName: "person.crop.circle")
                .font(.title2)
                .foregroundStyle(.secondary)
                .symbolRenderingMode(.hierarchical)
            VStack(alignment: .leading, spacing: 2) {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Text(a.label).lineLimit(1).truncationMode(.middle)
                    if let about = about(a) {
                        Text(about).font(.callout).foregroundStyle(.tertiary).lineLimit(1).truncationMode(.middle)
                    }
                }
                let s = status(a)
                HStack(spacing: 5) {
                    Circle().fill(s.color).frame(width: 7, height: 7)
                    Text(s.text)
                    if let u = a.usage {
                        Text("·")
                        Text(u).monospacedDigit()
                    }
                }
                .font(.callout)
                .foregroundStyle(.secondary)
            }
            Spacer()
            Menu {
                menuItems(a)
            } label: {
                Image(systemName: "ellipsis.circle")
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .fixedSize()
            .help(a.isOwn ? "Rename \(a.label)" : "Rename, move or remove \(a.label)")
        }
        .contextMenu { menuItems(a) }
    }

    /// Beside its name: which it is, where the name doesn't say. Account 1 is Claude Code's own
    /// sign-in (with its email, once it has a name of its own, and its plan); a named one keeps
    /// its number, as `dino claude-token` knows it.
    private func about(_ a: ClaudeAccountInfo) -> String? {
        if a.isOwn {
            let email = a.email.flatMap { $0 == a.name ? nil : $0 }
            return (["Claude Code's sign-in", email, a.planName]).compactMap { $0 }.joined(separator: " · ")
        }
        return a.name == nil ? nil : "Account \(a.number)"
    }

    @ViewBuilder
    private func menuItems(_ a: ClaudeAccountInfo) -> some View {
        Button("Rename…") { renaming = a }
        if !a.isOwn {
            let at = others.firstIndex(of: a) ?? 0
            Divider()
            Button("Move Up") { reorder(a, by: -1) }.disabled(at == 0)
            Button("Move Down") { reorder(a, by: 1) }.disabled(at == others.count - 1)
            Divider()
            Button("Remove…", role: .destructive) { confirmRemove(a) }
        }
    }

    /// "Answering now", "Ready", "At its limit until 14:00".
    private func status(_ a: ClaudeAccountInfo) -> (text: String, color: Color) {
        if a.answering { return ("Answering now", .green) }
        guard a.spent else { return ("Ready", .secondary) }
        if let t = a.resets_at { return ("At its limit until \(Clock.short(t))", .orange) }
        if let t = a.retry_at { return ("At its limit, trying again at \(Clock.short(t))", .orange) }
        return ("At its limit", .orange)
    }

    private func call(_ action: String, value: String? = nil, account: UInt32? = nil, order: [UInt32] = [], then: @escaping @MainActor (Result<ClaudeAccountsInfo, Error>) -> Void = { _ in }) {
        Task.detached {
            do {
                let i = try DinoConnection(path: DinoEnvironment.socketPath).claudeAccounts(action, value: value, account: account, order: order)
                await MainActor.run {
                    if info != i { info = i }
                    error = nil
                    then(.success(i))
                }
            } catch {
                await MainActor.run {
                    // The sheet says what's wrong with what it was given itself.
                    if !["add", "login", "login_code", "login_cancel", "rename"].contains(action) { self.error = error.localizedDescription }
                    then(.failure(error))
                }
            }
        }
    }

    private func refresh() {
        call("status")
    }

    private func reorder(_ a: ClaudeAccountInfo, by step: Int) {
        var numbers = others.map(\.number)
        guard let at = numbers.firstIndex(of: a.number), numbers.indices.contains(at + step) else { return }
        numbers.swapAt(at, at + step)
        call("order", order: numbers)
    }

    private func confirmRemove(_ a: ClaudeAccountInfo) {
        let alert = NSAlert()
        alert.messageText = "Remove \(a.label)?"
        alert.informativeText = "Claude Code will no longer switch to this account when another reaches its limit. The token stays valid until it expires or you revoke it."
        alert.addButton(withTitle: "Remove")
        alert.addButton(withTitle: "Cancel")
        alert.buttons.first?.hasDestructiveAction = true
        if alert.runModal() == .alertFirstButtonReturn {
            call("remove", account: a.number)
        }
    }
}

/// Add Account…: one way in. "Sign in with Claude" runs Claude Code's own `claude setup-token`
/// in dinod, which opens claude.ai in the browser; once the user signs in there, the account is
/// checked and added. A small link pastes a token instead. Every state says plainly where it is:
/// signing in, checking, added, already added (which account), or failed (why, and Try Again).
private struct AddClaudeAccountSheet: View {
    /// What dinod says now, polled by the section while a sign-in runs.
    let info: ClaudeAccountsInfo?
    /// A `claude_accounts` action, with its value and account; the answer to `done`.
    let call: (String, String?, UInt32?, @escaping @MainActor (Result<ClaudeAccountsInfo, Error>) -> Void) -> Void
    let close: () -> Void

    /// The sign-in this sheet started, by its id.
    @State private var login: UInt64?
    @State private var pasting = false
    @State private var otherBrowser = false
    @State private var token = ""
    @State private var code = ""
    @State private var error: String?
    @State private var busy = false
    /// The account a pasted token just added.
    @State private var pasted: UInt32?

    private var current: ClaudeLoginInfo? {
        guard let l = info?.login, l.id == login else { return nil }
        return l
    }
    /// The account just added, by signing in or from a pasted token.
    private var addedNumber: UInt32? {
        if let pasted { return pasted }
        guard let n = current?.account, current?.stage == "added" else { return nil }
        return n
    }
    private var added: ClaudeAccountInfo? {
        guard let n = addedNumber else { return nil }
        return info?.accounts.first { $0.number == n }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Add a Claude Account").font(.headline)
            if addedNumber != nil {
                done
            } else if pasting {
                pasteToken
            } else {
                switch current?.stage {
                case "starting", "browser": signingIn
                case "checking": checking
                case "added": done
                case "failed" where current?.duplicate != nil: already
                case "failed": failed
                default: start
                }
            }
        }
        .padding(20)
        .frame(width: 440)
        .onAppear {
            if let l = info?.login, l.underWay { login = l.id }
        }
        .onChange(of: current?.stage) { _, stage in
            busy = false
            if stage == "cancelled" { login = nil }
        }
    }

    // MARK: States

    private var start: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Your browser opens on claude.ai. Sign in with the account you want to add, and it shows up in the list.")
                .fixedSize(horizontal: false, vertical: true)
            failure(error)
            HStack {
                link("Paste a token instead") { error = nil; pasting = true }
                Spacer()
                Button("Cancel") { close() }.keyboardShortcut(.cancelAction)
                Button("Sign in with Claude") { signIn() }
                    .keyboardShortcut(.defaultAction)
                    .disabled(busy || info == nil)
            }
        }
    }

    private var signingIn: some View {
        VStack(alignment: .leading, spacing: 14) {
            progress("Signing in… Finish in your browser.")
            if otherBrowser, let url = current?.url {
                VStack(alignment: .leading, spacing: 8) {
                    Text("Open this sign-in in a browser that's on the right account, or a private window, then paste the code claude.ai shows at the end.")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                    HStack {
                        link("Copy the sign-in link") {
                            NSPasteboard.general.clearContents()
                            NSPasteboard.general.setString(url, forType: .string)
                        }
                        Spacer()
                    }
                    HStack {
                        TextField("Code from claude.ai", text: $code)
                            .textFieldStyle(.roundedBorder)
                            .onSubmit(sendCode)
                        Button("Continue") { sendCode() }
                            .disabled(busy || code.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    }
                    failure(error)
                }
            }
            HStack {
                if !otherBrowser, current?.url != nil {
                    link("Browser on a different account?") { otherBrowser = true }
                }
                Spacer()
                Button("Cancel") { cancel() }.keyboardShortcut(.cancelAction)
            }
        }
    }

    private var checking: some View {
        progress("Signed in. Checking the account with Anthropic…")
            .padding(.bottom, 8)
    }

    /// Added: the account, then its name, asked at once, while the user knows which they signed in to.
    private var done: some View {
        let n = addedNumber ?? 0
        return VStack(alignment: .leading, spacing: 14) {
            outcome("checkmark.circle.fill", .green, "Account \(n) added", added?.usage.map { "Signed in · \($0)" } ?? "Signed in")
            if let note = pasted == nil ? current?.error : nil {
                Text(note).font(.callout).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }
            Divider()
            NameClaudeAccount(account: added, initial: added?.email ?? "Account \(n)", title: "Name this account", skip: "Skip") { name in
                call("rename", name, n) { r in
                    if case .success = r { close() }
                }
            } close: {
                close()
            }
        }
    }

    private var already: some View {
        let n = current?.duplicate ?? 0
        let name = info?.accounts.first { $0.number == n }?.label ?? "Account \(n)"
        let which = n == 1 ? "\(name), the one Claude Code is signed in with" : name
        return VStack(alignment: .leading, spacing: 14) {
            outcome("person.crop.circle.badge.checkmark", .secondary, "Already added", "You signed in to \(which).")
            Text("To add another account, switch to it on claude.ai in your browser first (or sign out there), then sign in again.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            HStack {
                Spacer()
                Button("Cancel") { close() }.keyboardShortcut(.cancelAction)
                Button("Sign in Again") { signIn() }.keyboardShortcut(.defaultAction).disabled(busy)
            }
        }
    }

    private var failed: some View {
        VStack(alignment: .leading, spacing: 14) {
            outcome("exclamationmark.triangle.fill", .orange, "Couldn't add the account", current?.error ?? "The sign-in didn't finish.")
            HStack {
                link("Paste a token instead") { error = nil; pasting = true }
                Spacer()
                Button("Cancel") { close() }.keyboardShortcut(.cancelAction)
                Button("Try Again") { signIn() }.keyboardShortcut(.defaultAction).disabled(busy)
            }
        }
    }

    private var pasteToken: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Paste the token `claude setup-token` printed for the account. It starts with sk-ant-oat.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            SecureField("sk-ant-oat01-…", text: $token)
                .onSubmit(keep)
            failure(error)
            HStack {
                link("Sign in with Claude instead") { token = ""; error = nil; pasting = false }
                Spacer()
                Button("Cancel") { token = ""; close() }.keyboardShortcut(.cancelAction)
                Button("Add Account") { keep() }
                    .keyboardShortcut(.defaultAction)
                    .disabled(busy || token.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
    }

    // MARK: Parts

    @ViewBuilder
    private func failure(_ text: String?) -> some View {
        if let text {
            Text(text).font(.callout).foregroundStyle(.red).fixedSize(horizontal: false, vertical: true)
        }
    }

    private func outcome(_ symbol: String, _ color: Color, _ title: String, _ detail: String) -> some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: symbol).foregroundStyle(color).font(.title2)
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                Text(detail).font(.callout).foregroundStyle(.secondary).monospacedDigit().fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private func progress(_ text: String) -> some View {
        HStack(spacing: 8) {
            ProgressView().controlSize(.small)
            Text(text)
        }
    }

    private func link(_ title: String, _ action: @escaping () -> Void) -> some View {
        Button(title, action: action).buttonStyle(.link).font(.callout)
    }

    // MARK: Actions

    private func signIn() {
        busy = true
        error = nil
        code = ""
        otherBrowser = false
        call("login", nil, nil) { r in
            switch r {
            case .success(let i): login = i.login?.id
            case .failure(let e): error = e.localizedDescription
            }
            busy = false
        }
    }

    private func sendCode() {
        let value = code.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty, !busy else { return }
        busy = true
        error = nil
        call("login_code", value, nil) { r in
            busy = false
            if case .failure(let e) = r { error = e.localizedDescription } else { code = "" }
        }
    }

    private func cancel() {
        call("login_cancel", nil, nil) { _ in }
        login = nil
        close()
    }

    private func keep() {
        let value = token.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty, !busy else { return }
        busy = true
        call("add", value, nil) { r in
            busy = false
            switch r {
            case .success(let i):
                token = ""
                // Named next, as one signed in to is.
                if let n = i.added { pasted = n } else { close() }
            case .failure(let e):
                error = e.localizedDescription
            }
        }
    }
}

/// "Name this account": a name for one of the user's Claude accounts, so they can tell them apart
/// ("Work", "Personal"). Anthropic tells dino nothing of whose an added account is (its token can
/// only make model calls), so the user names it; account 1 starts as Claude Code's email. Asked
/// right after an account is added, and from a row's Rename….
struct NameClaudeAccount: View {
    let account: ClaudeAccountInfo?
    /// What the field starts with: the account's email when known, else "Account 3".
    let initial: String
    let title: String
    /// The button that leaves it as it is: "Skip" after adding, "Cancel" from Rename….
    let skip: String
    /// A sheet of its own (Rename…), not a part of Add Account's.
    var standalone = false
    let save: (String) -> Void
    let close: () -> Void

    @State private var name = ""
    @State private var started = false
    @FocusState private var focused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(title).font(.headline)
            Text(account?.isOwn == true
                 ? "Shown in the account list, the sidebar and when sessions switch accounts. Leave it empty to use Claude Code's email."
                 : "Shown in the account list, the sidebar and when sessions switch accounts. Anthropic doesn't tell dino whose account a sign-in is, so name it while you know.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            TextField("Work, Personal…", text: $name)
                .textFieldStyle(.roundedBorder)
                .focused($focused)
                .onSubmit { save(name) }
                .accessibilityLabel("Account name")
            HStack {
                Spacer()
                Button(skip) { close() }.keyboardShortcut(.cancelAction)
                Button("Save") { save(name) }.keyboardShortcut(.defaultAction)
            }
        }
        .padding(standalone ? 20 : 0)
        .frame(width: standalone ? 400 : nil)
        .onAppear {
            guard !started else { return }
            started = true
            name = initial
            focused = true
        }
    }
}
