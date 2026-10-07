import AppKit
import SwiftUI
import UserNotifications

/// Tells the user when a background agent needs them or finishes: a macOS notification when
/// allowed, and always a Dock bounce while dino isn't frontmost.
enum Notifier {
    static let center = UNUserNotificationCenter.current()
    /// Click → focus that session.
    @MainActor static var onOpenSession: ((String) -> Void)?

    @MainActor static func setUp() {
        center.delegate = Delegate.shared
        NotificationAccess.shared.request()
    }

    /// Settings → General and the Welcome card: "Notify me when an agent needs me". On unless
    /// turned off; the sidebar's Needs you and the Dock badge say it either way.
    static let needsYouKey = "notifyNeedsYou"
    static var needsYouOn: Bool { UserDefaults.standard.object(forKey: needsYouKey) as? Bool ?? true }

    /// An agent asking for something: a permission, an answer. Its own key, so it can go once
    /// answered (`answered`) without taking another notification about the session with it.
    @MainActor static func needsYou(key: String, title: String, body: String, session: String? = nil) {
        guard needsYouOn else {
            if !NSApp.isActive { NSApp.requestUserAttention(.informationalRequest) }
            return
        }
        post(key: key, title: title, body: body, session: session)
    }

    /// What it asked was answered (here or in its terminal): its notification would only be stale.
    static func answered(key: String) {
        center.removeDeliveredNotifications(withIdentifiers: [key])
        center.removePendingNotificationRequests(withIdentifiers: [key])
    }

    @MainActor static func post(session: SessionInfo, title: String, body: String) {
        // One notification per session; a newer event replaces the older one.
        post(key: "session-\(session.id)", title: title, body: body, session: session.id)
    }

    /// `key` names what the notification is about; a newer one with the same key replaces it.
    @MainActor static func post(key: String, title: String, body: String, session: String? = nil) {
        if !NSApp.isActive { NSApp.requestUserAttention(.informationalRequest) }
        let content = UNMutableNotificationContent()
        content.title = title
        content.body = body
        content.sound = .default
        if let session { content.userInfo = ["session": session] }
        let request = UNNotificationRequest(identifier: key, content: content, trigger: nil)
        center.add(request) { error in
            if let error { NSLog("dino notification failed: \(error)") } else { NSLog("dino notification posted: \(key)") }
        }
    }

    final class Delegate: NSObject, UNUserNotificationCenterDelegate {
        static let shared = Delegate()

        // Show banners even while dino is frontmost (the session may be in the background).
        func userNotificationCenter(_: UNUserNotificationCenter, willPresent _: UNNotification) async -> UNNotificationPresentationOptions {
            [.banner, .sound]
        }

        func userNotificationCenter(_: UNUserNotificationCenter, didReceive response: UNNotificationResponse) async {
            guard let id = response.notification.request.content.userInfo["session"] as? String else { return }
            await MainActor.run {
                NSApp.activate(ignoringOtherApps: true)
                Notifier.onOpenSession?(id)
            }
        }
    }
}

/// Whether macOS lets dino notify, and how: asked when dino opens and again when you turn on
/// "Notify me when an agent needs me" (macOS shows its prompt only the first time).
@MainActor final class NotificationAccess: ObservableObject {
    static let shared = NotificationAccess()

    /// nil until asked.
    @Published private(set) var status: UNAuthorizationStatus?
    /// Allowed, but set to None in System Settings: they go to Notification Center without a banner.
    @Published private(set) var quiet = false

    var allowed: Bool { status == .authorized || status == .provisional }

    private init() {
        NotificationCenter.default.addObserver(forName: NSApplication.didBecomeActiveNotification, object: nil, queue: .main) { _ in
            MainActor.assumeIsolated { NotificationAccess.shared.refresh() }
        }
    }

    func refresh() {
        Notifier.center.getNotificationSettings { settings in
            let status = settings.authorizationStatus
            let quiet = settings.alertStyle == .none
            Task { @MainActor in
                let access = NotificationAccess.shared
                if access.status != status { access.status = status }
                if access.quiet != quiet { access.quiet = quiet }
            }
        }
    }

    /// macOS's prompt the first time; after that its answer stands until changed in System Settings.
    func request() {
        Notifier.center.requestAuthorization(options: [.alert, .sound, .badge]) { granted, error in
            NSLog("dino notifications authorized=\(granted) error=\(String(describing: error))")
            Task { @MainActor in NotificationAccess.shared.refresh() }
        }
    }

    /// System Settings → Notifications → dino.
    func openSettings() {
        let id = Bundle.main.bundleIdentifier ?? "dev.dino.app"
        NSWorkspace.shared.open(URL(string: "x-apple.systempreferences:com.apple.Notifications-Settings.extension?id=\(id)")!)
    }

    /// What to say under the switch, when anything: why it can't notify as it is.
    var note: String? {
        switch status {
        case .denied?: "Notifications are off for \(Permissions.appName) in System Settings"
        case _ where allowed && quiet: "\(Permissions.appName)'s notifications go to Notification Center without a banner. Choose Banners or Alerts in System Settings to see them"
        default: nil
        }
    }
}

/// "Notify me when an agent needs me": on the Welcome card and in Settings → General. Turning it
/// on asks macOS, or, once macOS was told no, opens System Settings where that's changed.
struct NeedsYouNotifyToggle: View {
    /// In a Form (Settings) rather than on the card.
    var form = false
    @AppStorage(Notifier.needsYouKey) private var on = true
    @ObservedObject private var access = NotificationAccess.shared

    private var isOn: Binding<Bool> {
        Binding(get: { on && access.status != .denied }, set: { want in
            on = want
            guard want else { return }
            if access.status == .denied { access.openSettings() } else { access.request() }
        })
    }

    var body: some View {
        Group {
            if form {
                Toggle(isOn: isOn) {
                    Text("Notify me when an agent needs me")
                    Text(access.note ?? "A notification names the session and what it asks; click it to go there. None for the session you're looking at.")
                }
                if access.note != nil {
                    HStack {
                        Spacer()
                        Button("Open System Settings") { access.openSettings() }
                    }
                }
            } else {
                VStack(alignment: .leading, spacing: 3) {
                    Toggle("Notify me when an agent needs me", isOn: isOn)
                        .toggleStyle(.checkbox)
                        .font(.callout)
                        .help("A macOS notification names the session and what it asks; click it to go there")
                    if let note = access.note {
                        HStack(alignment: .firstTextBaseline, spacing: 6) {
                            Text(note).font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                            Button("Open System Settings") { access.openSettings() }
                                .buttonStyle(.link)
                                .font(.caption)
                        }
                        .padding(.leading, 20)
                    }
                }
            }
        }
        .onAppear { access.refresh() }
    }
}
