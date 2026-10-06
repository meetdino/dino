import SwiftUI

/// One session as dinod last said, observed apart from `DinoModel`, as `PaneSignal` is for its
/// progress: what changes all the time about a session (a shell's title, whether an agent is
/// thinking, what it asks or waits on, its last output) isn't announced by the model (see
/// `DinoModel.sessions`), so a change redraws the views showing that session, not the window, the
/// menu bar and every pane.
@MainActor
final class LiveSession: ObservableObject {
    @Published fileprivate(set) var info: SessionInfo

    fileprivate init(_ info: SessionInfo) { self.info = info }
}

@MainActor
enum LiveSessions {
    private static var lives: [String: LiveSession] = [:]

    /// Session `s`'s, made the first time a view asks.
    static func of(_ s: SessionInfo) -> LiveSession {
        if let l = lives[s.id] { return l }
        let l = LiveSession(s)
        lives[s.id] = l
        return l
    }

    /// Each one follows the model's sessions; a gone session's goes.
    static func follow(_ sessions: [SessionInfo]) {
        guard !lives.isEmpty else { return }
        var gone = Set(lives.keys)
        for s in sessions {
            gone.remove(s.id)
            if let l = lives[s.id], l.info != s { l.info = s }
        }
        for id in gone { lives[id] = nil }
    }
}

/// `content` drawn from session `live` as it is now, and again each time it changes: for what shows
/// a session but is drawn by a view that doesn't watch it (the toolbar, a menu, an automation's run).
struct Live<Content: View>: View {
    @ObservedObject var live: LiveSession
    @ViewBuilder let content: (SessionInfo) -> Content

    init(_ s: SessionInfo, @ViewBuilder content: @escaping (SessionInfo) -> Content) {
        _live = ObservedObject(wrappedValue: LiveSessions.of(s))
        self.content = content
    }

    var body: some View { content(live.info) }
}
