import SwiftUI

/// The dino account and settings sync, as dinod reports them (`dino sync status`).
struct SyncStatus: Codable, Equatable {
    var phase: String
    var server: String
    var email: String?
    var account_url: String?
    var last_sync: UInt64?
    var pending: Int
    var synced: Int
    var key_sync: Bool
    var recovery_key: String?
    var device_code: String?
    var device_url: String?
    /// Only here, only in the account, set differently.
    var conflict: [Int]?
    var snapshots: Int
    var message: String?
    /// While this Mac needs the key: its request to the account's other Macs.
    var join: JoinRequest?
    var approvals: [ApprovalRequest]?
}

/// This Mac asking another for the key; `code` once one has answered.
struct JoinRequest: Codable, Equatable {
    var expires_at: UInt64
    var code: String?
}

/// Another Mac asking this one for the key; `code` once this Mac answered and it revealed.
struct ApprovalRequest: Codable, Equatable, Identifiable {
    var id: String
    var device: String
    var os: String
    var expires_at: UInt64
    var code: String?
}

private struct SyncResponse: Decodable {
    let type: String
    let status: SyncStatus?
    let url: String?
}

extension DinoConnection {
    /// One sync action; the page to open for `login`, else the status after it.
    fileprivate func sync(_ action: String, _ value: String? = nil) throws -> (SyncStatus?, URL?) {
        var body: [String: Any] = ["type": "sync", "action": action]
        if let value { body["value"] = value }
        let r = try JSONDecoder().decode(SyncResponse.self, from: send(body))
        return (r.status, r.url.flatMap(URL.init(string:)))
    }
}

/// Settings → Dino Account's view of dinod. Asks while the pane is showing, not otherwise.
@MainActor
final class SyncStore: ObservableObject {
    static let shared = SyncStore()

    @Published var status: SyncStatus?
    @Published var error: String?
    private var watching = 0
    private var timer: Timer?

    /// Ask now and then while a view that shows it is up.
    func watch() {
        watching += 1
        refresh()
        guard timer == nil else { return }
        timer = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { _ in
            Task { @MainActor in SyncStore.shared.refresh() }
        }
    }

    func unwatch() {
        watching = max(0, watching - 1)
        if watching == 0 {
            timer?.invalidate()
            timer = nil
        }
    }

    func refresh() { act("status") }

    /// Runs `action` in dinod; `login` opens the browser.
    func act(_ action: String, _ value: String? = nil) {
        Task.detached {
            do {
                let (status, url) = try DinoConnection(path: DinoEnvironment.socketPath).sync(action, value)
                await MainActor.run {
                    if action != "status" { self.error = nil }
                    if let status, status != self.status { self.status = status }
                    if let url { NSWorkspace.shared.open(url) }
                }
                if url != nil { await MainActor.run { self.refresh() } }
            } catch {
                await MainActor.run { self.error = "\(error)" }
            }
        }
    }
}

/// The account at the top of Settings' sidebar.
struct AccountRow: View {
    @ObservedObject private var sync = SyncStore.shared

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "person.crop.circle.fill")
                .font(.system(size: 30))
                .foregroundStyle(signedIn ? AnyShapeStyle(Brand.green) : AnyShapeStyle(.secondary))
            VStack(alignment: .leading, spacing: 1) {
                Text(signedIn ? (sync.status?.email ?? "Dino Account") : "Sign In").fontWeight(.semibold).lineLimit(1)
                Text(signedIn ? "Settings sync" : "with your Dino Account").font(.caption).foregroundStyle(.secondary)
            }
        }
        .onAppear { sync.refresh() }
    }

    private var signedIn: Bool { sync.status.map { $0.phase != "signed_out" && $0.phase != "signing_in" } ?? false }
}

struct AccountPane: View {
    @ObservedObject private var sync = SyncStore.shared

    var body: some View {
        Form {
            switch sync.status?.phase ?? "signed_out" {
            case "signing_in": SigningIn(status: sync.status)
            case "needs_key": NeedsKey()
            case "conflict": Conflict(counts: sync.status?.conflict ?? [0, 0, 0])
            case "ready", "joining": Ready(status: sync.status)
            default: SignedOut(status: sync.status)
            }
            if let m = sync.status?.message {
                Section { Label(m, systemImage: "info.circle").foregroundStyle(.secondary) }
            }
            if let e = sync.error {
                Section { Label(e, systemImage: "exclamationmark.triangle.fill").foregroundStyle(.red) }
            }
        }
        .formStyle(.grouped)
        .onAppear { sync.watch() }
        .onDisappear { sync.unwatch() }
    }
}

private struct Hero: View {
    let title: String
    let text: String

    var body: some View {
        VStack(spacing: 8) {
            Image(systemName: "person.crop.circle.fill")
                .font(.system(size: 52))
                .foregroundStyle(.secondary)
            Text(title).font(.title2.weight(.semibold))
            Text(text)
                .multilineTextAlignment(.center)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 8)
    }
}

private struct SignedOut: View {
    let status: SyncStatus?
    @ObservedObject private var sync = SyncStore.shared
    @State private var server = ""

    var body: some View {
        Section {
            VStack(spacing: 12) {
                Hero(title: "Dino Account", text: "Sign in to keep your settings the same on every Mac you use dino on.")
                Button("Sign In…") { sync.act("login", server.isEmpty ? nil : server) }
                    .controlSize(.large)
                    .keyboardShortcut(.defaultAction)
                Text("End-to-end encrypted: settings are encrypted on this Mac before they're sent, and the dino server can't read them.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .frame(maxWidth: .infinity)
        }
        Section("What syncs") {
            Text("Agent defaults, policies, worktree and terminal settings, SSH hosts, repository variables (matched by git remote), and API keys if you want.")
                .foregroundStyle(.secondary)
        }
        Section("What never leaves this Mac") {
            Text("Sessions, conversations, terminal content, shell history, and your agents' own logins.")
                .foregroundStyle(.secondary)
        }
        Section {
            TextField("Account server", text: $server, prompt: Text(status?.server ?? ""))
        } footer: {
            Footnote("Leave it empty for dino's own. A self-hosted server works the same.")
        }
    }
}

private struct SigningIn: View {
    let status: SyncStatus?

    var body: some View {
        Section {
            VStack(spacing: 12) {
                ProgressView()
                if let code = status?.device_code, let url = status?.device_url {
                    Text("Go to \(url) and enter").foregroundStyle(.secondary)
                    Text(code).font(.system(.title, design: .monospaced).weight(.semibold)).textSelection(.enabled)
                } else {
                    Text("Finish signing in in your browser.").foregroundStyle(.secondary)
                }
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 12)
        }
    }
}

private struct NeedsKey: View {
    @ObservedObject private var sync = SyncStore.shared
    @State private var key = ""
    @State private var confirmReset = false
    @State private var useRecovery = false

    var body: some View {
        if !useRecovery {
            Section {
                VStack(spacing: 12) {
                    Hero(title: "Approve this Mac", text: "This account already syncs from another Mac. Its settings are encrypted with a key only your Macs have: approve this one from a Mac that's signed in.")
                    if let code = sync.status?.join?.code {
                        Text("Check the other Mac shows").foregroundStyle(.secondary)
                        Text(code)
                            .font(.system(size: 34, weight: .semibold, design: .monospaced))
                            .textSelection(.enabled)
                            .accessibilityLabel("Approval code \(code.replacingOccurrences(of: "-", with: " "))")
                    } else if sync.status?.join != nil {
                        ProgressView().controlSize(.small)
                        Text("Open dino on another of your Macs and choose Approve. A code shows here once it does.")
                            .foregroundStyle(.secondary)
                            .multilineTextAlignment(.center)
                            .fixedSize(horizontal: false, vertical: true)
                    } else {
                        Button("Ask My Other Macs") { sync.act("ask") }.keyboardShortcut(.defaultAction)
                    }
                }
                .frame(maxWidth: .infinity)
                HStack {
                    Spacer()
                    Button("Use Recovery Key Instead") { useRecovery = true }.buttonStyle(.link)
                }
            } footer: {
                Footnote("The code is the same on both Macs only when nobody swapped keys on the way. Nothing is shared until you approve on the other Mac.")
            }
        } else {
            recovery
        }
    }

    @ViewBuilder private var recovery: some View {
        Section {
            Hero(title: "Enter your recovery key", text: "The recovery key you saved when you set up sync opens your settings here, without another Mac.")
            LabeledContent("Recovery key") {
                TextField("Recovery key", text: $key, prompt: Text("D1-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX"))
                    .labelsHidden()
                    .font(.system(.body, design: .monospaced))
                    .onSubmit(join)
            }
            HStack {
                Button("Lost it? Reset Sync…", role: .destructive) { confirmReset = true }
                Spacer()
                Button("Approve from Another Mac") { useRecovery = false }
                Button("Continue", action: join)
                    .keyboardShortcut(.defaultAction)
                    .disabled(key.trimmingCharacters(in: .whitespaces).isEmpty)
            }
        }
        .confirmationDialog("Reset sync?", isPresented: $confirmReset) {
            Button("Reset Sync", role: .destructive) { sync.act("reset") }
        } message: {
            Text("The account's synced settings are deleted and this Mac's become the account's, under a new key. Your other Macs will need the new recovery key.")
        }
    }

    private func join() { sync.act("join", key) }
}

private struct Conflict: View {
    let counts: [Int]
    @ObservedObject private var sync = SyncStore.shared

    var body: some View {
        Section {
            Hero(title: "This Mac has settings of its own", text: "\(counts[safe: 0] ?? 0) only here, \(counts[safe: 1] ?? 0) only in your account, \(counts[safe: 2] ?? 0) set differently.")
            HStack {
                Button("Use the Account's") { sync.act("resolve", "cloud") }
                Button("Keep This Mac's") { sync.act("resolve", "local") }
                Spacer()
                Button("Merge") { sync.act("resolve", "merge") }.keyboardShortcut(.defaultAction)
            }
        } footer: {
            Footnote("Merge keeps both; where they differ, the one changed more recently wins.")
        }
    }
}

private struct Ready: View {
    let status: SyncStatus?
    @ObservedObject private var sync = SyncStore.shared
    @State private var confirmReset = false
    @State private var confirmSignOut = false

    var body: some View {
        if let key = status?.recovery_key {
            Section {
                VStack(alignment: .leading, spacing: 8) {
                    Label("Save your recovery key", systemImage: "key.fill").font(.headline)
                    Text("It's shown once. Another Mac joining this account needs it, and without it and your Macs, sync starts over. Keep it in your password manager.")
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                    Text(key)
                        .font(.system(.title3, design: .monospaced).weight(.semibold))
                        .textSelection(.enabled)
                        .padding(.vertical, 4)
                    HStack {
                        Button("Copy") {
                            NSPasteboard.general.clearContents()
                            NSPasteboard.general.setString(key, forType: .string)
                        }
                        Spacer()
                        Button("I've Saved It") { sync.act("ack_recovery") }.keyboardShortcut(.defaultAction)
                    }
                }
                .padding(.vertical, 4)
            }
        }
        Section {
            LabeledContent("Signed in as", value: status?.email ?? "")
            LabeledContent("Last synced", value: status?.last_sync.map(ago) ?? "Not yet")
            LabeledContent("Synced settings", value: "\(status?.synced ?? 0)")
            if let pending = status?.pending, pending > 0 {
                LabeledContent("Waiting to send", value: "\(pending)")
            }
            HStack {
                if let url = status?.account_url.flatMap(URL.init(string:)) {
                    Link("Devices and Data…", destination: url)
                }
                Spacer()
                Button("Sync Now") { sync.act("now") }
            }
        } footer: {
            Footnote("Settings are encrypted on this Mac before they're sent: the dino server can't read them. Sessions, terminal content and your agents' logins never leave this Mac.")
        }
        Section {
            Toggle("Sync API keys", isOn: Binding(get: { status?.key_sync ?? false }, set: { sync.act("keys", $0 ? "on" : "off") }))
        } footer: {
            Footnote("Keys in dino's key store, encrypted the same way. Sign-ins to ChatGPT and your dino account stay on each Mac.")
        }
        Section {
            if (status?.snapshots ?? 0) > 0 {
                Button("Undo Last Sync Change") { sync.act("undo") }
            }
            Button("Sign Out…") { confirmSignOut = true }
            Button("Reset Sync…", role: .destructive) { confirmReset = true }
        }
        .confirmationDialog("Sign out of your dino account?", isPresented: $confirmSignOut) {
            Button("Sign Out") { sync.act("logout") }
        } message: {
            Text("This Mac stops syncing. Its settings stay as they are.")
        }
        .confirmationDialog("Reset sync?", isPresented: $confirmReset) {
            Button("Reset Sync", role: .destructive) { sync.act("reset") }
        } message: {
            Text("The account's synced settings are deleted and this Mac's become the account's, under a new key and recovery key. Your other Macs will need the new recovery key.")
        }
    }

    private func ago(_ t: UInt64) -> String {
        let d = max(0, Int(Date().timeIntervalSince1970) - Int(t))
        if d < 5 { return "Just now" }
        if d < 60 { return "\(d) seconds ago" }
        if d < 3600 { return "\(d / 60) minutes ago" }
        return "\(d / 3600) hours ago"
    }
}

private extension Array {
    subscript(safe i: Int) -> Element? { indices.contains(i) ? self[i] : nil }
}

// MARK: - Another Mac asking this one for the key

extension DinoModel {
    /// One sync action in dinod, off the main thread; `done` gets the error, if any.
    private func syncAction(_ action: String, _ value: String, done: (@MainActor (String?) -> Void)? = nil) {
        Task.detached {
            let failure: String?
            do {
                let (status, _) = try DinoConnection(path: DinoEnvironment.socketPath).sync(action, value)
                if let status { await MainActor.run { SyncStore.shared.status = status } }
                failure = nil
            } catch {
                failure = error.localizedDescription
            }
            let result = failure
            await MainActor.run { done?(result) }
        }
    }

    /// Take the request so both Macs can show the code. Gives nothing yet.
    func claimApproval(_ r: ApprovalRequest) {
        guard r.code == nil else { return }
        syncAction("claim", r.id) { e in if let e { self.error = e } }
    }

    func grantApproval(_ r: ApprovalRequest, done: @escaping @MainActor (String?) -> Void) {
        syncAction("grant", r.id, done: done)
    }

    func denyApproval(_ r: ApprovalRequest) {
        approvals.removeAll { $0.id == r.id }
        syncAction("deny", r.id)
    }

    /// What dinod reports with each state: published only when it changes, and a notification the
    /// first time a request shows up.
    func applyApprovals(_ next: [ApprovalRequest]) {
        guard next != approvals else { return }
        for r in next where !approvals.contains(where: { $0.id == r.id }) {
            Notifier.post(key: "approval-\(r.id)", title: "\(r.device) wants to sync your settings", body: "Open dino to compare codes and approve it.")
        }
        approvals = next
        if let open = approving, let now = next.first(where: { $0.id == open.id }) {
            if now != open { approving = now }
        }
    }
}

/// A slim bar over the terminals while another Mac waits for this one's approval.
struct ApprovalBanner: View {
    @EnvironmentObject var model: DinoModel

    var body: some View {
        if let r = model.approvals.first {
            HStack(spacing: 10) {
                Image(systemName: "laptopcomputer.and.arrow.down").foregroundStyle(Brand.green)
                Text("**\(r.device)** wants to sync your settings").lineLimit(1)
                Spacer()
                Button("Deny") { model.denyApproval(r) }
                // No default-key shortcut: Return belongs to the terminal under the bar.
                Button("Approve…") { model.approving = r }
            }
            .font(.callout)
            .padding(.horizontal, 12)
            .padding(.vertical, 7)
            .background(.bar)
            .overlay(alignment: .bottom) { Divider() }
        }
    }
}

/// Approving another Mac: take its request, show the code, and only on "they match" give it the key.
struct ApproveSheet: View {
    let request: ApprovalRequest
    @EnvironmentObject var model: DinoModel
    @Environment(\.dismiss) private var dismiss
    @State private var error: String?

    var body: some View {
        VStack(spacing: 14) {
            Image(systemName: "laptopcomputer.and.arrow.down")
                .font(.system(size: 40))
                .foregroundStyle(Brand.green)
            Text("Approve \(request.device)?").font(.title2.weight(.semibold))
            Text("It gets the key to your synced settings. Approve only if it shows the same code.")
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
            Group {
                if let code = request.code {
                    Text(code)
                        .font(.system(size: 34, weight: .semibold, design: .monospaced))
                        .textSelection(.enabled)
                        .accessibilityLabel("Approval code \(code.replacingOccurrences(of: "-", with: " "))")
                } else {
                    HStack(spacing: 8) {
                        ProgressView().controlSize(.small)
                        Text("Waiting for \(request.device)…").foregroundStyle(.secondary)
                    }
                }
            }
            .frame(height: 44)
            if let error { Text(error).foregroundStyle(.red).font(.callout) }
            HStack {
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
                Spacer()
                Button("Codes Don't Match") {
                    model.denyApproval(request)
                    dismiss()
                }
                .disabled(request.code == nil)
                Button("They Match: Approve") {
                    model.grantApproval(request) { e in
                        if let e { error = e } else { dismiss() }
                    }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(request.code == nil)
            }
        }
        .padding(24)
        .frame(width: 440)
        .onAppear { model.claimApproval(request) }
    }
}
