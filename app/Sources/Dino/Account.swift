import SwiftUI

/// The dino account and settings sync, as dinod reports them (`dino sync status`).
struct SyncStatus: Codable, Equatable {
    var phase: String
    var server: String
    var email: String?
    var account_url: String?
    var last_sync: UInt64?
    var pending: Int
    /// Settings set to something other than their default: a default isn't stored.
    var synced: Int
    /// The same, in a person's terms ("Claude Code defaults", "2 terminal settings"); nil from an
    /// older dinod.
    var synced_what: [String]?
    /// Where a sign-in link went, while it waits to be opened.
    var email_sent_to: String?
    var device_code: String?
    var device_url: String?
    /// Only here, only in the account, set differently.
    var conflict: [Int]?
    var snapshots: Int
    var message: String?
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

/// Settings → Dino Account's view of dinod. Asks while the pane is showing, and while a sign-in
/// started here is under way (finished in the browser, with the pane long closed), not otherwise.
@MainActor
final class SyncStore: ObservableObject {
    static let shared = SyncStore()

    @Published var status: SyncStatus?
    @Published var error: String?
    private var watching = 0
    private var timer: Timer?
    /// A sign-in started from this app: when it ends, the app comes forward to show how.
    private var signingIn = false
    /// Opens Settings on the Account pane; set by the app once it can open windows.
    var showAccount: (() -> Void)?

    /// Ask now and then while a view that shows it is up.
    func watch() {
        watching += 1
        refresh()
        startTimer()
    }

    private func startTimer() {
        guard timer == nil else { return }
        timer = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { _ in
            Task { @MainActor in SyncStore.shared.refresh() }
        }
    }

    func unwatch() {
        watching = max(0, watching - 1)
        if watching == 0, !signingIn {
            timer?.invalidate()
            timer = nil
        }
    }

    func refresh() { act("status") }

    /// The sign-in finished (or failed) in the browser: bring dino forward on the Account pane,
    /// which says who is signed in, or asks which settings to keep when the two sides differ.
    private func signInEnded(_ phase: String) {
        guard signingIn, phase != "signing_in", phase != "joining" else { return }
        signingIn = false
        if watching == 0 {
            timer?.invalidate()
            timer = nil
        }
        NSApp.activate(ignoringOtherApps: true)
        showAccount?()
    }

    /// Runs `action` in dinod; `login` opens the browser.
    func act(_ action: String, _ value: String? = nil) {
        if action == "login" || action == "login_email" {
            signingIn = true
            startTimer()
        } else if action == "cancel_login" {
            signingIn = false
        }
        Task.detached {
            do {
                let (status, url) = try DinoConnection(path: DinoEnvironment.socketPath).sync(action, value)
                await MainActor.run {
                    if action != "status" { self.error = nil }
                    if let status, status != self.status { self.status = status }
                    if let phase = status?.phase { self.signInEnded(phase) }
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
            case "conflict": Conflict(counts: sync.status?.conflict ?? [0, 0, 0])
            case "ready": Ready(status: sync.status)
            default: SignedOut()
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
    var symbol = "person.crop.circle.fill"
    let title: String
    let text: String

    var body: some View {
        VStack(spacing: 8) {
            Image(systemName: symbol)
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
    @ObservedObject private var sync = SyncStore.shared
    @State private var byEmail = false
    @State private var email = ""

    var body: some View {
        Section {
            VStack(spacing: 14) {
                Hero(title: "Dino Account", text: "Sign in to keep your settings the same on every Mac you use dino on.")
                if byEmail {
                    HStack {
                        TextField("Email", text: $email, prompt: Text("you@example.com"))
                            .textContentType(.emailAddress)
                            .onSubmit(send)
                        Button("Send Link", action: send)
                            .keyboardShortcut(.defaultAction)
                            .disabled(!email.contains("@"))
                    }
                    .frame(maxWidth: 360)
                    Button("Sign in with GitHub instead") { byEmail = false }.buttonStyle(.link)
                } else {
                    Button { sync.act("login") } label: {
                        Text("Sign in with GitHub").fontWeight(.semibold).frame(minWidth: 220)
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(Brand.green)
                    .controlSize(.extraLarge)
                    .keyboardShortcut(.defaultAction)
                    Button("Use email instead") { byEmail = true }.buttonStyle(.link)
                }
            }
            .frame(maxWidth: .infinity)
            .padding(.bottom, 6)
        }
        Section("What syncs") {
            Text("Agent defaults, which agents you use and their limits, worktree and terminal settings, SSH hosts, and repository variables (matched by git remote).")
                .foregroundStyle(.secondary)
        }
        Section("What never leaves this Mac") {
            Text("API keys and tokens, sessions, conversations, terminal content, shell history, and your agents' own logins.")
                .foregroundStyle(.secondary)
        }
    }

    private func send() {
        let e = email.trimmingCharacters(in: .whitespaces)
        guard e.contains("@") else { return }
        sync.act("login_email", e)
    }
}

private struct SigningIn: View {
    let status: SyncStatus?
    @ObservedObject private var sync = SyncStore.shared

    var body: some View {
        Section {
            VStack(spacing: 12) {
                if let sent = status?.email_sent_to {
                    Hero(symbol: "envelope.badge", title: "Check your email", text: "We sent a sign-in link to \(sent). Open it on any device, and this Mac signs in by itself.")
                    ProgressView().controlSize(.small)
                    Button("Use a different email") { sync.act("cancel_login") }.buttonStyle(.link)
                } else if let code = status?.device_code, let url = status?.device_url {
                    ProgressView()
                    Text("Go to \(url) and enter").foregroundStyle(.secondary)
                    Text(code).font(.system(.title, design: .monospaced).weight(.semibold)).textSelection(.enabled)
                    Button("Cancel") { sync.act("cancel_login") }
                } else {
                    ProgressView()
                    Text("Finish signing in in your browser.").foregroundStyle(.secondary)
                    Button("Cancel") { sync.act("cancel_login") }
                }
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 12)
        }
    }
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
    @State private var confirmSignOut = false

    var body: some View {
        Section {
            LabeledContent("Signed in as", value: status?.email ?? "")
            LabeledContent("Last synced", value: status?.last_sync.map(ago) ?? "Not yet")
            LabeledContent("Synced settings") {
                Text(syncedText).multilineTextAlignment(.trailing)
            }
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
            Footnote("Settings sync is on. A setting you change from dino's default goes to your other Macs. API keys and tokens, sessions, terminal content and your agents' logins never leave this Mac.")
        }
        Section {
            if (status?.snapshots ?? 0) > 0 {
                Button("Undo Last Sync Change") { sync.act("undo") }
            }
            Button("Sign Out…") { confirmSignOut = true }
        }
        .confirmationDialog("Sign out of your dino account?", isPresented: $confirmSignOut) {
            Button("Sign Out") { sync.act("logout") }
        } message: {
            Text("This Mac stops syncing. Its settings stay as they are.")
        }
    }

    /// What the account holds, never a bare count: a default isn't stored, so none means every
    /// setting is at its default.
    private var syncedText: String {
        if let what = status?.synced_what {
            return what.isEmpty ? "Nothing yet: all at their defaults" : what.joined(separator: ", ")
        }
        let n = status?.synced ?? 0
        return n == 0 ? "Nothing yet: all at their defaults" : n == 1 ? "1 setting" : "\(n) settings"
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
