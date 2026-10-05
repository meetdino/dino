import AppKit
import ApplicationServices
import CoreGraphics
import SwiftUI

/// The macOS privacy permissions programs and agents in dino's terminals commonly need. They get
/// dino's (dinod runs as its launch agent), but macOS lists dino under Privacy & Security only once
/// the app itself has asked: a program in a terminal asking for Screen Recording fails without a
/// word. So the app asks, from Settings → General → Permissions, never by itself.
enum Permission: String, CaseIterable, Identifiable {
    case screenRecording = "screen_recording"
    case accessibility
    case fullDiskAccess = "full_disk_access"

    var id: String { rawValue }

    /// As System Settings names it; macOS 27 calls Accessibility "Device Control and Data Access".
    var title: String {
        switch self {
        case .screenRecording: "Screen & System Audio Recording"
        case .accessibility: ProcessInfo.processInfo.operatingSystemVersion.majorVersion >= 27 ? "Device Control and Data Access" : "Accessibility"
        case .fullDiskAccess: "Full Disk Access"
        }
    }

    /// What it's for, in a terminal.
    var use: String {
        switch self {
        case .screenRecording: "Screenshots (screencapture) and agents' computer use seeing your screen"
        case .accessibility: "Agents' computer use clicking and typing in your apps"
        case .fullDiskAccess: "Reading Mail, Messages, Safari and other folders macOS protects"
        }
    }

    /// macOS has no way to ask for Full Disk Access: it's added in System Settings.
    var requestable: Bool { self != .fullDiskAccess }

    /// Its list in System Settings → Privacy & Security.
    var pane: URL {
        let anchor = switch self {
        case .screenRecording: "Privacy_ScreenCapture"
        case .accessibility: "Privacy_Accessibility"
        case .fullDiskAccess: "Privacy_AllFiles"
        }
        return URL(string: "x-apple.systempreferences:com.apple.preference.security?\(anchor)")!
    }
}

@MainActor
final class Permissions: ObservableObject {
    static let shared = Permissions()

    /// This app as the prompt and System Settings name it: Finder's name for it ("dino"; "Dino" for
    /// a build to try things in, dev.dino.app.dev). The prompt adds this app's own bundle id.
    static let appName = FileManager.default.displayName(atPath: Bundle.main.bundlePath).replacingOccurrences(of: ".app", with: "")

    /// Allowed or not, by permission; missing until checked, or where macOS didn't say.
    @Published private(set) var allowed: [Permission: Bool] = [:]

    /// Asked just now: (permission, no system prompt came up), for the row to say what to do next.
    @Published private(set) var asked: (Permission, prompted: Bool)?

    /// The app whose launch agent runs dinod, when that's another dino (the one you use, beside
    /// "dino dev"): programs in the terminals then have that app's permissions, not this one's.
    @Published private(set) var dinodApp: String?

    private var checking = false

    /// Whether macOS allows each one now. Asked of a new `dino permissions` started by the app, so
    /// macOS answers for dino as it does for programs in its terminals: the app's own answer for
    /// Screen Recording stays what it was the first time it asked. In-process when that fails (a
    /// `dino` from before the command).
    func refresh() {
        guard !checking else { return }
        checking = true
        Task.detached(priority: .userInitiated) {
            let fresh = Self.askDino()
            let result = fresh ?? Self.inProcess()
            let owner = Self.dinodOwner()
            await MainActor.run {
                self.checking = false
                if self.allowed != result { self.allowed = result }
                if self.dinodApp != owner { self.dinodApp = owner }
            }
        }
    }

    nonisolated private static func askDino() -> [Permission: Bool]? {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: DinoEnvironment.dinoBinary)
        p.arguments = ["permissions", "--json"]
        let out = Pipe()
        p.standardOutput = out
        p.standardError = FileHandle.nullDevice
        guard (try? p.run()) != nil else { return nil }
        let data = out.fileHandleForReading.readDataToEndOfFile()
        p.waitUntilExit()
        guard p.terminationStatus == 0,
              let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return nil }
        var result: [Permission: Bool] = [:]
        for permission in Permission.allCases {
            if let ok = json[permission.rawValue] as? Bool { result[permission] = ok }
        }
        return result.isEmpty ? nil : result
    }

    /// dinod's launch agent is named for its app's bundle id (`<id>.dinod…`, scripts/bundle.sh and
    /// crates/dino/src/launchd.rs); that app's name, when it isn't this one.
    nonisolated private static func dinodOwner() -> String? {
        guard let label = (try? DinoConnection(path: DinoEnvironment.socketPath).request(["type": "version"]))?.launchd,
              let range = label.range(of: ".dinod") else { return nil }
        let bundle = String(label[..<range.lowerBound])
        guard bundle != Bundle.main.bundleIdentifier else { return nil }
        guard let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: bundle) else { return bundle }
        return FileManager.default.displayName(atPath: url.path).replacingOccurrences(of: ".app", with: "")
    }

    nonisolated private static func inProcess() -> [Permission: Bool] {
        var result: [Permission: Bool] = [
            .screenRecording: CGPreflightScreenCaptureAccess(),
            .accessibility: AXIsProcessTrusted(),
        ]
        // As `dino permissions` (crates/dino/src/permissions.rs): the privacy database opens only
        // with Full Disk Access.
        for db in ["/Library/Application Support/com.apple.TCC/TCC.db", NSString(string: "~/Library/Application Support/com.apple.TCC/TCC.db").expandingTildeInPath] {
            let fd = open(db, O_RDONLY)
            if fd >= 0 {
                close(fd)
                result[.fullDiskAccess] = true
                break
            } else if errno == EPERM || errno == EACCES {
                result[.fullDiskAccess] = false
                break
            }
        }
        return result
    }

    /// The system's own prompt, which also lists dino in System Settings. macOS prompts once: if
    /// nothing comes up (dino was asked about before), its list in System Settings opens instead.
    func request(_ permission: Permission) {
        switch permission {
        case .screenRecording:
            _ = CGRequestScreenCaptureAccess()
        case .accessibility:
            let prompt = kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String
            _ = AXIsProcessTrustedWithOptions([prompt: true] as CFDictionary)
        case .fullDiskAccess:
            openSettings(permission)
            return
        }
        let wasActive = NSApp.isActive
        Task { @MainActor in
            try? await Task.sleep(for: .milliseconds(1200))
            let prompted = Self.promptShowing() || (wasActive && !NSApp.isActive)
            asked = (permission, prompted)
            if !prompted { openSettings(permission) }
        }
    }

    /// macOS's prompt is up: a window of its universalAccessAuthWarn (it shows both of these;
    /// whose window it is reads without Screen Recording). Not knowing, the prompt is taken not to
    /// have come, and System Settings opens beside it.
    private static func promptShowing() -> Bool {
        let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] ?? []
        return windows.contains { ($0[kCGWindowOwnerName as String] as? String) == "universalAccessAuthWarn" }
    }

    func openSettings(_ permission: Permission) {
        NSWorkspace.shared.open(permission.pane)
    }
}

/// Settings → General: each permission, whether dino has it, and the way to give it.
struct PermissionsSection: View {
    @ObservedObject private var permissions = Permissions.shared

    var body: some View {
        Section {
            ForEach(Permission.allCases) { permission in
                row(permission)
            }
        } header: {
            Text("Permissions")
        } footer: {
            if let other = permissions.dinodApp {
                Footnote("Programs and agents in dino's terminals use the permissions of the app running dinod, not their own. Here that's \(other), not \(Permissions.appName), so they need allowing for \(other) too.")
            } else {
                Footnote("Programs and agents in dino's terminals use the permissions you give \(Permissions.appName) here, not their own. Allowed, they apply to what starts next; a program that already asked may need starting again.")
            }
        }
        .onAppear { permissions.refresh() }
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
            permissions.refresh()
        }
    }

    private func row(_ permission: Permission) -> some View {
        let ok = permissions.allowed[permission]
        return LabeledContent {
            HStack(spacing: 8) {
                status(ok)
                if ok != true, permission.requestable {
                    Button("Request…") { permissions.request(permission) }
                        .help("Shows macOS's own prompt, which also puts \(Permissions.appName) in the list in System Settings")
                }
                Button("Open System Settings") { permissions.openSettings(permission) }
                    .help("Privacy & Security → \(permission.title), where this app is \(Permissions.appName) (\(Bundle.main.bundleIdentifier ?? ""))")
            }
        } label: {
            Text(permission.title)
            Text(note(permission, ok))
        }
    }

    private func note(_ permission: Permission, _ ok: Bool?) -> String {
        if ok != true, let asked = permissions.asked, asked.0 == permission {
            return asked.prompted
                ? "Choose Open System Settings in macOS's prompt, then turn on \(Permissions.appName)"
                : "macOS asks only once: turn on \(Permissions.appName) in System Settings"
        }
        if ok != true, !permission.requestable { return permission.use + ". Turn on \(Permissions.appName) in System Settings, or add it with +" }
        return permission.use
    }

    @ViewBuilder private func status(_ ok: Bool?) -> some View {
        switch ok {
        case true?:
            Label("Allowed", systemImage: "checkmark.circle.fill")
                .foregroundStyle(Color(nsColor: .systemGreen))
        case false?:
            Text("Not allowed").foregroundStyle(.secondary)
        case nil:
            EmptyView()
        }
    }
}
