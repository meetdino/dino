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

    var id: UInt32 { number }
    var isOwn: Bool { number == 1 }
    var name: String { isOwn ? "Your signed-in account" : "Account \(number)" }
}

struct ClaudeAccountsInfo: Codable, Equatable {
    var accounts: [ClaudeAccountInfo]
    var added: UInt32?
    /// The shell running `claude setup-token`.
    var creating: String?
    var error: String?
}

private struct ClaudeAccountsResponse: Decodable {
    var accounts: ClaudeAccountsInfo
}

extension DinoConnection {
    /// `status`, `add` (`value`: a token), `create`, `remove` (`account`) or `order` (`order`).
    func claudeAccounts(_ action: String, value: String? = nil, account: UInt32? = nil, order: [UInt32] = []) throws -> ClaudeAccountsInfo {
        var body: [String: Any] = ["type": "claude_accounts", "action": action]
        if let value { body["value"] = value }
        if let account { body["account"] = account }
        if !order.isEmpty { body["order"] = order }
        return try JSONDecoder().decode(ClaudeAccountsResponse.self, from: send(body)).accounts
    }
}

/// Settings → Agents: your other Claude accounts, which Claude Code goes on with when the one it
/// signed in with is at its limit. Which answers now, and when a spent one resets.
struct ClaudeAccountsSection: View {
    /// Show a shell dinod opened, in the main window.
    let act: (String) -> Void
    @State private var info: ClaudeAccountsInfo?
    @State private var adding = false
    @State private var error: String?

    private var others: [ClaudeAccountInfo] { info?.accounts.filter { !$0.isOwn } ?? [] }

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
            Text("Claude Code Accounts")
        } footer: {
            VStack(alignment: .leading, spacing: 8) {
                if info?.creating != nil {
                    HStack(spacing: 8) {
                        ProgressView().controlSize(.small)
                        Text("Waiting for you to sign in with the other account in your browser…")
                            .font(.callout).foregroundStyle(.secondary)
                    }
                    .padding(.leading, 10)
                }
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
        // What the proxy finds (a limit, a reset) shows while this is on screen; while setup-token
        // runs, sooner, until its token is in.
        .task(id: info?.creating) {
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(info?.creating != nil ? 1 : 5))
                refresh()
            }
        }
        .sheet(isPresented: $adding) {
            AddClaudeAccountSheet(
                add: { token, done in call("add", value: token, then: done) },
                create: {
                    adding = false
                    call("create") { r in if case .success(let i) = r, let shell = i.creating { act(shell) } }
                },
                cancel: { adding = false }
            )
        }
    }

    private func row(_ a: ClaudeAccountInfo) -> some View {
        HStack(spacing: 10) {
            Image(systemName: "person.crop.circle")
                .font(.title2)
                .foregroundStyle(.secondary)
                .symbolRenderingMode(.hierarchical)
            VStack(alignment: .leading, spacing: 2) {
                Text(a.name)
                let s = status(a)
                HStack(spacing: 5) {
                    Circle().fill(s.color).frame(width: 7, height: 7)
                    Text(s.text)
                }
                .font(.callout)
                .foregroundStyle(.secondary)
            }
            Spacer()
            if !a.isOwn {
                Menu {
                    menuItems(a)
                } label: {
                    Image(systemName: "ellipsis.circle")
                }
                .menuStyle(.borderlessButton)
                .menuIndicator(.hidden)
                .fixedSize()
                .help("Move or remove \(a.name)")
            }
        }
        .contextMenu { if !a.isOwn { menuItems(a) } }
    }

    @ViewBuilder
    private func menuItems(_ a: ClaudeAccountInfo) -> some View {
        let at = others.firstIndex(of: a) ?? 0
        Button("Move Up") { reorder(a, by: -1) }.disabled(at == 0)
        Button("Move Down") { reorder(a, by: 1) }.disabled(at == others.count - 1)
        Divider()
        Button("Remove…", role: .destructive) { confirmRemove(a) }
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
                    // A sheet says what's wrong with what was pasted itself.
                    if action != "add" { self.error = error.localizedDescription }
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
        alert.messageText = "Remove Claude \(a.name.lowercased())?"
        alert.informativeText = "Claude Code will no longer switch to this account when another reaches its limit. The token stays valid until it expires or you revoke it."
        alert.addButton(withTitle: "Remove")
        alert.addButton(withTitle: "Cancel")
        alert.buttons.first?.hasDestructiveAction = true
        if alert.runModal() == .alertFirstButtonReturn {
            call("remove", account: a.number)
        }
    }
}

/// Add Account…: paste the token `claude setup-token` printed, or have dino run it in a new tab.
private struct AddClaudeAccountSheet: View {
    let add: (String, @escaping @MainActor (Result<ClaudeAccountsInfo, Error>) -> Void) -> Void
    let create: () -> Void
    let cancel: () -> Void
    @State private var token = ""
    @State private var error: String?
    @State private var busy = false

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Add a Claude Account").font(.headline)
            Text("Run `claude setup-token`, sign in with that account, and paste the token it prints.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            SecureField("sk-ant-oat01-…", text: $token)
                .frame(width: 400)
                .onSubmit(keep)
            if let error {
                Text(error).font(.callout).foregroundStyle(.red).fixedSize(horizontal: false, vertical: true)
            }
            HStack {
                Button("Run in New Tab") { create() }
                    .help("Runs claude setup-token in a new tab. Sign in with that account in your browser, and dino adds the token.")
                Spacer()
                Button("Cancel") { token = ""; cancel() }.keyboardShortcut(.cancelAction)
                Button("Add Account") { keep() }
                    .keyboardShortcut(.defaultAction)
                    .disabled(busy || token.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .padding(20)
        .frame(width: 440)
    }

    private func keep() {
        let value = token.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty, !busy else { return }
        busy = true
        add(value) { r in
            busy = false
            switch r {
            case .success:
                token = ""
                cancel()
            case .failure(let e):
                error = e.localizedDescription
            }
        }
    }
}
