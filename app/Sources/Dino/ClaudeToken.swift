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

/// Settings → Agents: the token `claude setup-token` makes, for Claude Code where it can't sign
/// in in a browser. Only the Claude Code dino starts gets it.
struct ClaudeTokenSection: View {
    @EnvironmentObject var store: SettingsStore
    /// Show a shell dinod opened, in the main window.
    let act: (String) -> Void
    @State private var info: ClaudeTokenInfo?
    @State private var pasting = false
    @State private var pasted = ""
    @State private var error: String?

    private var use: Binding<DinoSettings.ClaudeTokenUse> {
        Binding(
            get: { store.settings?.machine.claude_token ?? .standard },
            set: { u in store.update { $0.machine.claude_token = u } }
        )
    }

    var body: some View {
        Section {
            HStack(spacing: 10) {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Subscription token")
                    Text(detail).font(.callout).foregroundStyle(.secondary)
                }
                Spacer()
                if info?.creating != nil {
                    ProgressView().controlSize(.small).help("Waiting for claude setup-token to finish in the main window")
                }
                if info?.set == true {
                    Button("Remove…", role: .destructive) { confirmRemove() }
                } else {
                    Button("Paste…") { pasting = true }
                    Button("Create…") { create() }
                        .disabled(info?.creating != nil)
                        .help("Runs claude setup-token in a new shell. Sign in with your browser, and dino saves the token.")
                }
            }
            if info?.set == true {
                Toggle("Use on SSH hosts", isOn: use.ssh)
                Toggle("Use on this Mac, even when Claude Code is signed in", isOn: use.local)
            }
            if let e = error ?? info?.error {
                Text(e).font(.callout).foregroundStyle(.red)
            }
        } header: {
            Text("Claude Code Subscription Token")
        } footer: {
            Footnote("Lets Claude Code use your Claude plan where it can't sign in through a browser, such as on SSH hosts. The token comes from claude setup-token and lasts a year. Only Claude Code sessions that dino starts use it, never other agents. On this Mac, it's used only while Claude Code isn't signed in, unless you turn on “Use on this Mac, even when Claude Code is signed in”. The token stays on this Mac and never syncs.")
        }
        .task { refresh() }
        // While setup-token runs, look until the token is in.
        .task(id: info?.creating) {
            while info?.creating != nil, !Task.isCancelled {
                try? await Task.sleep(for: .seconds(1))
                refresh()
            }
        }
        .sheet(isPresented: $pasting) { pasteSheet }
    }

    private var detail: String {
        guard let info, info.set else {
            if info?.creating != nil { return "Waiting for you to sign in with your browser…" }
            return "Not set. Claude Code on SSH hosts uses its own sign-in."
        }
        var parts = [info.masked ?? "Set"]
        if let expires = info.expires {
            parts.append("expires \(Date(timeIntervalSince1970: TimeInterval(expires)).formatted(date: .abbreviated, time: .omitted))")
        }
        if info.signed_in == false {
            parts.append("also used on this Mac, where Claude Code isn't signed in")
        }
        return parts.joined(separator: " · ")
    }

    private var pasteSheet: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Paste a Claude Code subscription token").font(.headline)
            Text("Paste the token that claude setup-token printed. It starts with sk-ant-oat.").font(.callout).foregroundStyle(.secondary)
            SecureField("sk-ant-oat01-…", text: $pasted).frame(width: 380)
            HStack {
                Spacer()
                Button("Cancel") { pasted = ""; pasting = false }.keyboardShortcut(.cancelAction)
                Button("Save") { keep() }.keyboardShortcut(.defaultAction).disabled(pasted.trimmingCharacters(in: .whitespaces).isEmpty)
            }
        }
        .padding(20)
    }

    private func call(_ action: String, value: String? = nil, then: @escaping @MainActor (ClaudeTokenInfo) -> Void = { _ in }) {
        Task.detached {
            do {
                let i = try DinoConnection(path: DinoEnvironment.socketPath).claudeToken(action, value: value)
                await MainActor.run {
                    info = i
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

    private func create() {
        call("create") { i in
            if let shell = i.creating { act(shell) }
        }
    }

    private func keep() {
        let value = pasted.trimmingCharacters(in: .whitespacesAndNewlines)
        pasted = ""
        pasting = false
        call("set", value: value) { _ in store.load() }
    }

    private func confirmRemove() {
        let alert = NSAlert()
        alert.messageText = "Remove the Claude subscription token?"
        alert.informativeText = "Claude Code on SSH hosts will need to sign in on its own. The token stays valid until it expires or you revoke it."
        alert.addButton(withTitle: "Remove")
        alert.addButton(withTitle: "Cancel")
        if alert.runModal() == .alertFirstButtonReturn {
            call("remove") { _ in store.load() }
        }
    }
}
