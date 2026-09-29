import AppKit
import UserNotifications

/// Tells the user when a background agent needs them or finishes: a macOS notification when
/// allowed, and always a Dock bounce while dino isn't frontmost.
enum Notifier {
    static let center = UNUserNotificationCenter.current()
    /// Click → focus that session.
    @MainActor static var onOpenSession: ((String) -> Void)?

    static func setUp() {
        center.delegate = Delegate.shared
        center.requestAuthorization(options: [.alert, .sound, .badge]) { granted, error in
            NSLog("dino notifications authorized=\(granted) error=\(String(describing: error))")
        }
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
            if let error { NSLog("dino notification failed: \(error)") }
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
