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

/// The tokens the sidebar's sessions have worked through, for the usage panel's totals. They grow
/// with every model call a working agent makes, which `DinoModel` doesn't announce (see
/// `DinoModel.sessions`): a change redraws those totals, not the window.
@MainActor
final class SessionTokens: ObservableObject {
    static let shared = SessionTokens()

    struct Totals: Equatable {
        /// In and out, not counting the context read again from the prompt cache.
        var used: UInt64 = 0
        /// That context read again: summed over a long session, billions that say little.
        var cached: UInt64 = 0
        /// Worked through on a free model (dino's free tier).
        var free: UInt64 = 0
    }

    @Published private(set) var totals = Totals()

    func follow(_ sessions: [SessionInfo]) {
        let cached = sessions.reduce(UInt64(0)) { $0 + ($1.cache_read_tokens ?? 0) }
        let used = sessions.reduce(UInt64(0)) { $0 + $1.input_tokens + $1.output_tokens } - cached
        let free = sessions.filter { $0.tier != nil }.reduce(UInt64(0)) { $0 + $1.input_tokens + $1.output_tokens - ($1.cache_read_tokens ?? 0) }
        let now = Totals(used: used, cached: cached, free: free)
        if now != totals { totals = now }
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
