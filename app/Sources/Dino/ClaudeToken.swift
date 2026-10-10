import AppKit
import SwiftUI

/// The Claude subscription token as dinod holds it: never the token itself.
struct ClaudeTokenInfo: Codable, Equatable {
    var set: Bool
    var masked: String?
    var created: UInt64?
    var expires: UInt64?
    /// Claude Code on this Mac signed in on its own, when dinod last looked.
    var signed_in: Bool?
    /// The shell running `claude setup-token`.
    var creating: String?
    var error: String?
    /// Without a token of its own: the account Claude Code signs in with instead.
    var account: UInt32?
}

private struct ClaudeTokenResponse: Decodable {
    var token: ClaudeTokenInfo
}

extension DinoConnection {
    /// `status`, `create`, `set` (with `value`) or `remove`.
    func claudeToken(_ action: String, value: String? = nil) throws -> ClaudeTokenInfo {
        var body: [String: Any] = ["type": "claude_token", "action": action]
        if let value { body["value"] = value }
        return try JSONDecoder().decode(ClaudeTokenResponse.self, from: send(body)).token
    }
}

/// Settings → Accounts: which Claude account Claude Code signs in with on SSH hosts (and on this
/// Mac where it isn't signed in): the first added account, or a token kept from before. No sign-in
/// of its own: accounts are added in one place, the list above.
struct ClaudeTokenSection: View {
    @EnvironmentObject var store: SettingsStore
    @State private var info: ClaudeTokenInfo?
    @State private var error: String?

    private var use: Binding<DinoSettings.ClaudeTokenUse> {
        Binding(
            get: { store.settings?.machine.claude_token ?? .standard },
            set: { u in store.update { $0.machine.claude_token = u } }
        )
    }

    private var signsIn: Bool { info?.set == true || info?.account != nil }

    var body: some View {
        Section {
            HStack(spacing: 10) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(title)
                    if let d = detail { Text(d).font(.callout).foregroundStyle(.secondary) }
                }
                Spacer()
                if info?.set == true {
                    Button("Remove…", role: .destructive) { confirmRemove() }
                }
            }
            if signsIn {
                Toggle("Use on SSH hosts", isOn: use.ssh)
                Toggle("Use on this Mac, even when Claude Code is signed in", isOn: use.local)
            }
            if let e = error ?? info?.error {
                Text(e).font(.callout).foregroundStyle(.red)
            }
        } header: {
            Text("Claude Code on SSH Hosts")
        } footer: {
            Footnote("Claude Code on SSH hosts can't sign in through a browser, so dino signs it in with one of your accounts. On this Mac, only while Claude Code isn't signed in here, unless you turn that on. Only Claude Code sessions that dino starts use it.")
        }
        // Which account signs in follows the list above as accounts are added and removed.
        .task {
            while !Task.isCancelled {
                refresh()
                try? await Task.sleep(for: .seconds(2))
            }
        }
    }

    private var title: String {
        guard let info else { return "Looking…" }
        if info.set { return "Signs in with a token you added before" }
        if let n = info.account { return "Signs in with Account \(n)" }
        return "Add a Claude account above to use Claude Code on SSH hosts"
    }

    private var detail: String? {
        guard let info, info.set else { return nil }
        let instead = "Remove it to sign in with your first account instead."
        guard let expires = info.expires else { return instead }
        return "Expires \(Date(timeIntervalSince1970: TimeInterval(expires)).formatted(date: .abbreviated, time: .omitted)). \(instead)"
    }

    private func call(_ action: String, then: @escaping @MainActor (ClaudeTokenInfo) -> Void = { _ in }) {
        Task.detached {
            do {
                let i = try DinoConnection(path: DinoEnvironment.socketPath).claudeToken(action)
                await MainActor.run {
                    if info != i { info = i }
                    error = nil
                    then(i)
                }
            } catch {
                await MainActor.run { self.error = error.localizedDescription }
            }
        }
    }

    private func refresh() {
        call("status")
    }

    private func confirmRemove() {
        let alert = NSAlert()
        alert.messageText = "Remove the token you added before?"
        alert.informativeText = "Claude Code on SSH hosts will sign in with your first Claude account instead, if you've added one. The token stays valid until it expires or you revoke it."
        alert.addButton(withTitle: "Remove")
        alert.addButton(withTitle: "Cancel")
        if alert.runModal() == .alertFirstButtonReturn {
            call("remove") { _ in store.load() }
        }
    }
}
