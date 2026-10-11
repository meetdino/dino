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

/// Output tokens per second across every agent, over dinod's last 10 s, for the usage panel. dinod
/// answers once the rate differs from the one shown, so while no agent writes nothing is asked,
/// woken or redrawn; while one does, at most once a second. Watched only while the panel shows it.
@MainActor
final class OutputRate: ObservableObject {
    static let shared = OutputRate()

    @Published private(set) var perSecond: UInt32 = 0
    private var watchers = 0
    private var running = false

    func watch() {
        watchers += 1
        guard !running else { return }
        running = true
        // A thread of its own: the wait blocks, for as long as nothing writes.
        Thread.detachNewThread { Self.follow() }
    }

    func unwatch() { watchers = max(0, watchers - 1) }

    /// Shows `rate`; whether it's still wanted. Ends the watch if not, or if dinod `refused` it.
    private func shown(_ rate: UInt32, refused: Bool) -> Bool {
        if perSecond != rate { perSecond = rate }
        if watchers == 0 || refused { running = false }
        return running
    }

    private struct Answer: Decodable { var tokens_per_second: UInt32? }

    nonisolated private static func follow() {
        var connection: DinoConnection?
        var seen: UInt32?
        while true {
            var rate: UInt32 = 0
            var refused = false
            do {
                if connection == nil { connection = try DinoConnection(path: DinoEnvironment.socketPath) }
                var body: [String: Any] = ["type": "output_rate"]
                body["seen"] = seen
                let data = try connection!.send(body)
                rate = try JSONDecoder().decode(Answer.self, from: data).tokens_per_second ?? 0
                seen = rate
            } catch DinoError.daemon {
                // A dinod older than this app: nothing shown until the panel opens again.
                refused = true
            } catch {
                // dinod went: asked again once it's back.
                connection = nil
                seen = nil
                Thread.sleep(forTimeInterval: 5)
            }
            let wanted = DispatchQueue.main.sync { MainActor.assumeIsolated { shared.shown(rate, refused: refused) } }
            if !wanted { return }
        }
    }
}

/// The usage panel's live rate, beside Usage Stats and under the sessions' tokens, drawn as they
/// are: only while agents write, where "0 tok/s" would sit in the sidebar for good saying nothing.
struct OutputRateText: View {
    @ObservedObject private var rate = OutputRate.shared

    var body: some View {
        if rate.perSecond > 0 {
            Text("\(tokens(UInt64(rate.perSecond))) tok/s")
                .font(.caption.monospacedDigit())
                .help("Output tokens per second across every agent, over the last 10 seconds. Only traffic routed through dino is counted.")
                .accessibilityLabel("\(rate.perSecond) output tokens per second")
        }
    }
}
