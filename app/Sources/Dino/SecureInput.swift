import AppKit
import Carbon.HIToolbox
import SwiftUI

/// Secure Keyboard Entry, as in Ghostty: other apps can't read the keys you type into dino. On for
/// good from the app menu (or Ghostty's `toggle_secure_input`), and by itself while the pane you're
/// typing in reads a password (`macos-auto-secure-input`): dinod sees the shell's terminal turn
/// echo off in line mode, as `sudo`, `ssh` and `read -s` do. Only while dino is frontmost: macOS
/// would otherwise keep every app from reading keys.
@MainActor
final class SecureInput: ObservableObject {
    static let shared = SecureInput()
    private static let key = "SecureInput"

    /// The menu's toggle, kept across launches as Ghostty keeps it.
    @Published var global = UserDefaults.standard.bool(forKey: SecureInput.key) {
        didSet {
            guard global != oldValue else { return }
            UserDefaults.standard.set(global, forKey: Self.key)
            update()
        }
    }

    /// The session whose password prompt has it on, while its pane has the keyboard.
    @Published private(set) var prompt: String?
    /// It's on now.
    @Published private(set) var active = false
    /// What this app asked macOS for: the calls are counted, so each is made once.
    private var enabled = false
    private var observers: [Any] = []

    private init() {
        let center = NotificationCenter.default
        for name in [NSApplication.didBecomeActiveNotification, NSApplication.didResignActiveNotification, NSWindow.didBecomeKeyNotification] {
            observers.append(center.addObserver(forName: name, object: nil, queue: .main) { _ in
                MainActor.assumeIsolated { SecureInput.shared.update() }
            })
        }
    }

    /// Look again: the focus moved, the app came to the front or went, a shell's prompt changed.
    func update() {
        let prompting = PaneSignals.config.autoSecureInput ? passwordPane() : nil
        if prompt != prompting { prompt = prompting }
        let want = (global || prompting != nil) && NSApp.isActive
        if want != enabled {
            enabled = want
            if want { EnableSecureEventInput() } else { DisableSecureEventInput() }
        }
        if active != want { active = want }
    }

    /// The session reading a password in the pane that has the keyboard: a tab's, a split's or the
    /// quick terminal's.
    private func passwordPane() -> String? {
        guard NSApp.isActive, let model = GhosttyActions.model else { return nil }
        let quick = QuickTerminal.shared
        if quick.isShown, NSApp.keyWindow is QuickPanel {
            guard let id = quick.sessionID, quick.surfaceState?.isFocused == true else { return nil }
            return quick.passwordPrompt ? id : nil
        }
        guard let id = model.focusedTerminal, NSApp.keyWindow?.firstResponder is LinkTerminalView,
              model.sessions.first(where: { $0.id == id })?.password == true else { return nil }
        return id
    }
}

/// The app menu's Secure Keyboard Entry, checked while it's on for good.
struct SecureInputCommand: View {
    @ObservedObject var secure = SecureInput.shared

    var body: some View {
        Toggle("Secure Keyboard Entry", isOn: $secure.global)
            .help("Keep other apps from reading what you type in dino")
    }
}

/// A lock in the corner of the pane you're typing in while Secure Keyboard Entry is on
/// (`macos-secure-input-indication`), as Ghostty shows one.
struct SecureInputMark: View {
    @ObservedObject var secure = SecureInput.shared
    let id: String
    let focused: Bool

    var body: some View {
        if focused, secure.active, PaneSignals.config.secureInputIndication {
            Image(systemName: "lock.shield.fill")
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(.white)
                .padding(5)
                .background(Circle().fill(Color(nsColor: .systemBlue).opacity(0.85)))
                .padding(8)
                .help(secure.prompt == id
                    ? "Secure Keyboard Entry is on while this prompt reads a password: other apps can't read your keys"
                    : "Secure Keyboard Entry is on: other apps can't read your keys (dino menu)")
                .accessibilityLabel("Secure Keyboard Entry is on")
                .transition(.opacity)
        }
    }
}
