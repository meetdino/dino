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

/// Output tokens per second across every agent, over dinod's last 10 s, for the usage panel, and the
/// new input: what agents sent that the prompt cache didn't hold. dinod answers once either differs
/// from the one shown, so while no agent writes nothing is asked, woken or redrawn; while one does,
/// at most once a second. Watched only while the panel shows it.
@MainActor
final class OutputRate: ObservableObject {
    static let shared = OutputRate()

    @Published private(set) var perSecond: UInt32 = 0
    /// Nil from a dinod older than the input rate.
    @Published private(set) var inputPerSecond: UInt32?
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

    /// Shows `rate` and `input`; whether it's still wanted. Ends the watch if not, or if dinod
    /// `refused` it.
    private func shown(_ rate: UInt32, _ input: UInt32?, refused: Bool) -> Bool {
        if perSecond != rate { perSecond = rate }
        if inputPerSecond != input { inputPerSecond = input }
        if watchers == 0 || refused { running = false }
        return running
    }

    private struct Answer: Decodable { var tokens_per_second: UInt32?, input_tokens_per_second: UInt32? }

    nonisolated private static func follow() {
        var connection: DinoConnection?
        var seen: UInt32?, seenInput: UInt32?
        while true {
            var rate: UInt32 = 0
            var input: UInt32?
            var refused = false
            do {
                if connection == nil { connection = try DinoConnection(path: DinoEnvironment.socketPath) }
                var body: [String: Any] = ["type": "output_rate"]
                body["seen"] = seen
                body["seen_input"] = seenInput
                let answer = try JSONDecoder().decode(Answer.self, from: connection!.send(body))
                rate = answer.tokens_per_second ?? 0
                input = answer.input_tokens_per_second
                seen = rate
                seenInput = input
            } catch DinoError.daemon {
                // A dinod older than this app: nothing shown until the panel opens again.
                refused = true
            } catch {
                // dinod went: asked again once it's back.
                connection = nil
                seen = nil
                seenInput = nil
                Thread.sleep(forTimeInterval: 5)
            }
            let wanted = DispatchQueue.main.sync { MainActor.assumeIsolated { shared.shown(rate, input, refused: refused) } }
            if !wanted { return }
        }
    }
}

/// The usage panel's live rate, beside Usage Stats and under the sessions' tokens, drawn as they
/// are: only while agents write, where "0 tok/s" would sit in the sidebar for good saying nothing.
struct OutputRateText: View {
    @ObservedObject private var rate = OutputRate.shared

    var body: some View {
        if let text = Self.text(output: rate.perSecond, input: rate.inputPerSecond) {
            // On one line, ahead of Usage Stats beside it: in the narrowest sidebar the label gives way.
            Text(text)
                .font(.caption.monospacedDigit())
                .lineLimit(1)
                .layoutPriority(1)
                .help("Tokens per second across every agent, over the last 10 seconds: what they write (out), and what they send that the prompt cache didn't already hold (in). Each call sends the whole conversation again, mostly read back from the cache; those reads aren't counted. Only traffic routed through dino is counted.")
                .accessibilityLabel("\(rate.perSecond) output tokens per second" + (rate.inputPerSecond.map { ", \($0) new input tokens per second" } ?? ""))
        }
    }

    /// "2.0k out · 16.4k in tok/s", what's not 0 of the two; "2.0k tok/s" from a dinod older than the
    /// input rate; nil while neither moves.
    static func text(output: UInt32, input: UInt32?) -> String? {
        guard let input else { return output > 0 ? "\(tokens(UInt64(output))) tok/s" : nil }
        let parts = [(output, "out"), (input, "in")].filter { $0.0 > 0 }.map { "\(tokens(UInt64($0.0))) \($0.1)" }
        return parts.isEmpty ? nil : parts.joined(separator: " · ") + " tok/s"
    }
}
