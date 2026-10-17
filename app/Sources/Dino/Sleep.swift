import AppKit
import SwiftUI

/// Asleep to save memory (dinod's `sleep`): an idle agent's process stopped, its session kept as
/// it was. The app tells dinod which sessions are on screen, so one coming into view wakes and the
/// ones in view stay awake; its pane keeps the last screen, and a moon marks it in the sidebar and
/// its tab.
extension DinoModel {
    /// Tell dinod which sessions are on screen, when that changed: the selected tab's panes while a
    /// window of the app shows, else none. In order, on a queue of their own: a quick run of tab
    /// switches ends on the last one.
    func reportViewing() {
        guard let conn = connection else { return }
        let ids = Self.mainWindowShown ? shownSessions.filter { id in sessions.contains { $0.id == id } } : []
        guard ids != viewingSent else { return }
        viewingSent = ids
        // An older dinod doesn't know the request: its sessions never sleep, so there's nothing to say.
        Self.viewingQueue.async { try? conn.viewing(ids) }
    }

    private static let viewingQueue = DispatchQueue(label: "dino.viewing")

    /// A window showing the selected tab is on screen: visible, not minimized and not wholly
    /// covered. Settings, Usage Stats and the quick terminal don't show it (`isShown` counts them).
    static var mainWindowShown: Bool {
        NSApp.windows.contains { w in
            w.isVisible && !w.isMiniaturized && w.occlusionState.contains(.visible) && w.canBecomeMain && !(w is NSPanel)
                && ![SettingsView.windowID, StatsView.windowID].contains { w.identifier?.rawValue.hasPrefix($0) == true }
        }
    }

    /// An agent session dinod could put to sleep now, as far as the app can tell: dinod says why
    /// when it can't (working, asking, background work, scheduled prompts).
    func canSleep(_ s: SessionInfo) -> Bool {
        !s.plainShell && s.agent_id != "shell" && s.host == nil && !s.exited && s.asleep != true && s.waking != true
    }

    /// Sleep Now: its agent stopped now to free its memory, pinned or not. On a connection of its
    /// own: dinod answers once the agent is gone, which takes a few seconds, and every other request
    /// would wait on the shared one meanwhile.
    func sleepNow(_ id: String) {
        Task.detached {
            do {
                try DinoConnection(path: DinoEnvironment.socketPath).sleep(session: id)
            } catch {
                await MainActor.run { self.error = error.localizedDescription }
            }
        }
    }

    /// Wake: its agent started again on its conversation, in its place (one whose wake failed is
    /// tried again). On a connection of its own, as `sleepNow`: dinod answers once the agent is up.
    func wake(_ id: String) {
        Task.detached {
            do {
                try DinoConnection(path: DinoEnvironment.socketPath).wake(session: id)
            } catch {
                await MainActor.run { self.error = error.localizedDescription }
            }
        }
    }
}

/// Over the terminals while a session in view is asleep or waking: one quiet line. Its last screen
/// stays up below it, and what you type wakes it and reaches its agent once that's up.
struct SleepBanner: View {
    @EnvironmentObject var model: DinoModel

    var body: some View {
        let ids = model.shownSessions
        if let s = ids.lazy.compactMap({ id in model.sessions.first { $0.id == id && ($0.asleep == true || $0.waking == true) } }).first {
            let waking = s.waking == true
            let words = waking ? "Waking…" : s.wake_error.map { "Couldn't wake: \($0) · Wake in its menu tries again" } ?? "Asleep to save memory · wakes when you type"
            HStack(spacing: 6) {
                Image(systemName: "moon.zzz.fill")
                    .accessibilityHidden(true)
                Text(ids.count > 1 ? "\(words) (\(s.display))" : words)
                    .lineLimit(1)
                    .truncationMode(.tail)
                Spacer(minLength: 0)
            }
            .font(.callout)
            .foregroundStyle(.secondary)
            .padding(.horizontal, 10)
            .padding(.vertical, 3)
            .background(.bar)
            .overlay(alignment: .bottom) { Divider() }
            .help(waking ? "\(s.display) is starting again on its conversation. What you type goes in once it's up."
                : "\(s.display)'s agent was stopped\(s.slept_at.map { " at \(Clock.short($0))" } ?? "") to free its memory, its session kept as it was. Typing into it, or Wake in its menu, starts it again on its conversation.")
            .accessibilityElement(children: .combine)
        }
    }
}

/// On a session's row and tab beside the Done or Needs-you mark it still has while it's asleep.
struct SleepMark: View {
    let session: SessionInfo

    var body: some View {
        Image(systemName: "moon.zzz.fill")
            .font(.system(size: 9))
            .foregroundStyle(.secondary)
            .help(session.slept_at.map { "Asleep since \(Clock.short($0)) to save memory" } ?? "Asleep to save memory")
            .accessibilityLabel("Asleep")
    }
}

/// In a session's menu: Sleep Now for an agent that could sleep, Wake for one asleep.
struct SleepMenuItems: View {
    @EnvironmentObject var model: DinoModel
    let session: SessionInfo

    var body: some View {
        if session.asleep == true {
            Button("Wake") { model.wake(session.id) }
                .help("Start its agent again on its conversation, in its place")
        } else if model.canSleep(session) {
            Button("Sleep Now") { model.sleepNow(session.id) }
                .help("Stop its agent now to free its memory; it wakes when you open it or type into it")
        }
    }
}
