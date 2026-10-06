import AppKit
import DinoGhostty
import SwiftUI

/// What programs in a pane say besides their text, done as Ghostty 1.3 does on the Mac: a progress
/// bar (OSC 9;4), desktop notifications (OSC 9, OSC 777), a notice when a long command finishes
/// (`notify-on-command-finish`), and the bell (`bell-features`). Each session has its own
/// `PaneSignal`, so a progress report redraws that session's bar, tab and row, not the window.
@MainActor
final class PaneSignal: ObservableObject {
    /// A program's progress, until it removes it or 15 s pass without a report, as in Ghostty.
    @Published fileprivate(set) var progress: PaneProgress?
    /// The bell rang and the pane hasn't been focused or typed in since (`bell-features` title, border).
    @Published fileprivate(set) var rang = false
    fileprivate var reported = Date.distantPast
    fileprivate var expiry: Timer?
}

struct PaneProgress: Equatable {
    var state: TerminalProgressState
    var percent: Int?

    /// Ghostty's colors: red for an error, orange paused, the accent otherwise.
    var color: NSColor {
        switch state {
        case .error: .systemRed
        case .pause: .systemOrange
        default: .controlAccentColor
        }
    }

    /// How far, or nil to show it moving: a pause with no percent shows full, as in Ghostty.
    var fraction: Double? {
        if let percent { return Double(percent) / 100 }
        return state == .pause ? 1 : nil
    }

    var label: String {
        let what: String = switch state {
        case .error: "Failed"
        case .pause: "Paused"
        default: "In progress"
        }
        return percent.map { "\(what), \($0)%" } ?? what
    }
}

/// The Ghostty settings these follow, read each time the config is applied.
struct PaneConfig {
    var progress = true
    var desktopNotifications = true
    /// "never", "unfocused" or "always".
    var notifyOnFinish = "never"
    /// `notify-on-command-finish-action`: bit 0 bell, bit 1 notify.
    var notifyActions: UInt32 = 1
    var notifyAfterMs: UInt64 = 5000
    /// `bell-features`: system, audio, attention, title, border, from bit 0.
    var bell: UInt32 = 1 << 2 | 1 << 3
    var bellAudio: String?
    var bellVolume = 0.5
    var autoSecureInput = true
    var secureInputIndication = true
    var undoMs: UInt64 = 5000

    static let system: UInt32 = 1 << 0, audio: UInt32 = 1 << 1, attention: UInt32 = 1 << 2, title: UInt32 = 1 << 3, border: UInt32 = 1 << 4

    init() {}

    @MainActor init(_ c: TerminalController) {
        progress = c.configFlag("progress-style") ?? true
        desktopNotifications = c.configFlag("desktop-notifications") ?? true
        notifyOnFinish = c.configText("notify-on-command-finish") ?? "never"
        notifyActions = c.configBits("notify-on-command-finish-action") ?? 1
        notifyAfterMs = c.configMilliseconds("notify-on-command-finish-after") ?? 5000
        bell = c.configBits("bell-features") ?? (Self.attention | Self.title)
        bellAudio = c.configPath("bell-audio-path")
        bellVolume = c.configNumber("bell-audio-volume") ?? 0.5
        autoSecureInput = c.configFlag("macos-auto-secure-input") ?? true
        secureInputIndication = c.configFlag("macos-secure-input-indication") ?? true
        undoMs = c.configMilliseconds("undo-timeout") ?? 5000
    }
}

@MainActor
enum PaneSignals {
    private(set) static var config = PaneConfig()
    private static var signals: [String: PaneSignal] = [:]
    /// Sessions whose bell shows (`rang`), so a key typed elsewhere costs one look at an empty set.
    private static var rung: Set<String> = []
    private static var sound: (path: String, sound: NSSound)?

    static func readConfig(_ c: TerminalController) {
        config = PaneConfig(c)
        if !config.progress { for s in signals.values where s.progress != nil { clearProgress(s) } }
    }

    static func of(_ id: String) -> PaneSignal {
        if let s = signals[id] { return s }
        let s = PaneSignal()
        signals[id] = s
        return s
    }

    /// Only sessions still there keep theirs.
    static func keep(_ live: Set<String>) {
        guard signals.keys.contains(where: { !live.contains($0) }) else { return }
        for (id, s) in signals where !live.contains(id) { s.expiry?.invalidate() }
        signals = signals.filter { live.contains($0.key) }
        rung.formIntersection(live)
    }

    /// Ghostty's progress, notification, command-finished and bell actions: true when taken (the
    /// wrapper then doesn't publish them to the pane's state, which would redraw it for nothing).
    /// Nil for any other action.
    static func handle(_ action: TerminalHostAction, _ state: TerminalViewState?) -> Bool? {
        switch action {
        case .progressReport(let p, let percent):
            guard config.progress, let id = session(of: state) else { return true }
            // A turn later: this can come while SwiftUI is drawing.
            DispatchQueue.main.async { report(id, PaneProgress(state: p, percent: percent)) }
            return true
        case .desktopNotification(let title, let body):
            guard config.desktopNotifications, let id = session(of: state) else { return true }
            DispatchQueue.main.async { notify(id, state: state, title: title, body: body) }
            return true
        case .commandFinished(let exit, let nanos):
            guard config.notifyOnFinish != "never", let id = session(of: state) else { return true }
            DispatchQueue.main.async { finished(id, state: state, exit: exit, nanos: nanos) }
            return true
        case .ringBell:
            guard let id = session(of: state) else { return true }
            DispatchQueue.main.async { ring(id) }
            return true
        default:
            return nil
        }
    }

    /// The session pane `state` shows: a tab's or a split's, or the quick terminal's own shell.
    static func session(of state: TerminalViewState?) -> String? {
        guard let state else { return nil }
        if state === QuickTerminal.shared.surfaceState { return QuickTerminal.shared.sessionID }
        return GhosttyActions.model?.session(showing: state)
    }

    /// The pane is in front of you: its window is key, it has the keyboard and dino is frontmost.
    static func focused(_ state: TerminalViewState?) -> Bool {
        guard NSApp.isActive, let state, state.isFocused, let view = state.attachedPlatformView else { return false }
        return view.window?.isKeyWindow == true
    }

    // MARK: Progress

    private static func report(_ id: String, _ p: PaneProgress) {
        let s = of(id)
        guard p.state != .remove else { return clearProgress(s) }
        if s.progress != p { s.progress = p }
        s.reported = Date()
        // One timer while a bar shows, moved on rather than made again for every report.
        if s.expiry == nil { expire(s, after: 15) }
    }

    private static func expire(_ s: PaneSignal, after seconds: TimeInterval) {
        s.expiry = Timer.scheduledTimer(withTimeInterval: seconds, repeats: false) { [weak s] _ in
            MainActor.assumeIsolated {
                guard let s else { return }
                s.expiry = nil
                let left = 15 - Date().timeIntervalSince(s.reported)
                if left > 0.05, s.progress != nil { expire(s, after: left) } else { clearProgress(s) }
            }
        }
    }

    private static func clearProgress(_ s: PaneSignal) {
        s.expiry?.invalidate()
        s.expiry = nil
        if s.progress != nil { s.progress = nil }
    }

    // MARK: Notifications

    private static func notify(_ id: String, state: TerminalViewState?, title: String, body: String) {
        // In front of you already, as Ghostty leaves it.
        guard !focused(state) else { return }
        let model = GhosttyActions.model
        let s = model?.sessions.first { $0.id == id }
        // An agent that tells dino how it's doing: dino already says when it needs you or is done,
        // and its own notification would say it twice.
        if let s, s.agent != "shell", s.reportsStatus { return }
        let name = s.flatMap { s in model?.tabName(s) } ?? "Quick terminal"
        Notifier.post(key: "program-\(UUID().uuidString)", title: title.isEmpty ? name : title, body: body, session: id)
    }

    private static func finished(_ id: String, state: TerminalViewState?, exit: Int?, nanos: UInt64) {
        let ms = nanos / 1_000_000
        guard ms >= config.notifyAfterMs else { return }
        if config.notifyOnFinish == "unfocused", focused(state) { return }
        if config.notifyActions & 1 != 0 { ring(id) }
        guard config.notifyActions & 2 != 0 else { return }
        let model = GhosttyActions.model
        let name = model?.sessions.first { $0.id == id }.flatMap { s in model?.tabName(s) } ?? "Quick terminal"
        let failed = (exit ?? 0) != 0
        let took = Duration.milliseconds(Int64(ms)).formatted(.units(allowed: [.hours, .minutes, .seconds], width: .abbreviated, maximumUnitCount: 2))
        Notifier.post(key: "finished-\(id)", title: failed ? "Command failed" : "Command finished",
                      body: "\(name) · took \(took)\(exit.map { failed ? ", exit code \($0)" : "" } ?? "")", session: id)
    }

    // MARK: Bell

    /// The bell, with the features `bell-features` turns on. dino's own mark for a session that
    /// needs you comes from dinod, as before.
    static func ring(_ id: String) {
        let f = config.bell
        if f & PaneConfig.system != 0 { NSSound.beep() }
        if f & PaneConfig.audio != 0, let path = config.bellAudio { play(path) }
        if f & PaneConfig.attention != 0, !NSApp.isActive { NSApp.requestUserAttention(.informationalRequest) }
        if f & (PaneConfig.title | PaneConfig.border) != 0 {
            let s = of(id)
            if !s.rang { s.rang = true }
            rung.insert(id)
        }
    }

    private static func play(_ path: String) {
        let full = NSString(string: path).expandingTildeInPath
        if sound?.path != full {
            guard let s = NSSound(contentsOfFile: full, byReference: true) else { return }
            sound = (full, s)
        }
        guard let s = sound?.sound else { return }
        s.stop()
        s.volume = Float(min(max(config.bellVolume, 0), 1))
        s.play()
    }

    /// The pane was focused or typed in: its bell has been seen.
    static func seen(_ id: String) {
        guard rung.remove(id) != nil else { return }
        if let s = signals[id], s.rang { s.rang = false }
    }

    /// A key typed in the pane showing `state`.
    static func typed(in state: TerminalViewState?) {
        guard !rung.isEmpty, let id = session(of: state) else { return }
        seen(id)
    }
}

// MARK: - Views

/// A pane's progress along its top edge, as Ghostty draws it: 2 pt, the bar or a bouncing piece.
struct PaneProgressBar: View {
    @ObservedObject var signal: PaneSignal

    var body: some View {
        if let p = signal.progress {
            ProgressMark(progress: p, ring: false)
                .frame(height: 2)
                .allowsHitTesting(false)
                .accessibilityElement()
                .accessibilityLabel("Progress")
                .accessibilityValue(p.label)
        }
    }
}

/// A tab's progress: a thin bar along its bottom edge, over the tab, so nothing moves.
struct TabProgress: View {
    @ObservedObject var signal: PaneSignal

    var body: some View {
        if let p = signal.progress {
            ProgressMark(progress: p, ring: false).frame(height: 2).allowsHitTesting(false).help(p.label)
        }
    }
}

/// A sidebar row's progress: a ring around its status dot.
struct RowProgress: View {
    @ObservedObject var signal: PaneSignal

    var body: some View {
        if let p = signal.progress {
            ProgressMark(progress: p, ring: true).frame(width: 14, height: 14).allowsHitTesting(false).help(p.label)
        }
    }
}

/// The bell's marks on a pane, as `bell-features` says: a border around it until it's focused or
/// typed in.
struct PaneBellBorder: View {
    @ObservedObject var signal: PaneSignal

    var body: some View {
        if signal.rang, PaneSignals.config.bell & PaneConfig.border != 0 {
            Rectangle().strokeBorder(Color(nsColor: .systemYellow).opacity(0.6), lineWidth: 2).allowsHitTesting(false)
        }
    }
}

/// 🔔 before a tab's name while its bell shows (`bell-features` title).
struct BellTitleMark: View {
    @ObservedObject var signal: PaneSignal

    var body: some View {
        if signal.rang, PaneSignals.config.bell & PaneConfig.title != 0 {
            Text("🔔").font(.caption2).accessibilityLabel("Bell")
        }
    }
}

/// The bar or ring, drawn by Core Animation: a SwiftUI repeating animation would redraw the window
/// on the main thread every frame while a program reports progress it can't measure.
private struct ProgressMark: NSViewRepresentable {
    let progress: PaneProgress
    let ring: Bool

    func makeNSView(context _: Context) -> ProgressMarkView { ProgressMarkView(ring: ring) }

    func updateNSView(_ view: ProgressMarkView, context _: Context) {
        view.show(progress)
    }
}

private final class ProgressMarkView: NSView {
    private let ring: Bool
    private let track = CAShapeLayer()
    private let fill = CAShapeLayer()
    private var shown: PaneProgress?

    init(ring: Bool) {
        self.ring = ring
        super.init(frame: .zero)
        wantsLayer = true
        layer?.masksToBounds = !ring
        for l in [track, fill] {
            l.fillColor = nil
            l.lineCap = .round
            layer?.addSublayer(l)
        }
        track.isHidden = !ring
    }

    @available(*, unavailable)
    required init?(coder _: NSCoder) { fatalError() }

    override var isFlipped: Bool { true }

    func show(_ p: PaneProgress) {
        guard p != shown else { return }
        shown = p
        place()
    }

    override func layout() {
        super.layout()
        place()
    }

    // SwiftUI sizes it after it's first shown, without asking for a layout.
    override func setFrameSize(_ size: NSSize) {
        super.setFrameSize(size)
        place()
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        colors()
    }

    private func colors() {
        guard let p = shown else { return }
        effectiveAppearance.performAsCurrentDrawingAppearance {
            if ring {
                fill.strokeColor = p.color.cgColor
                track.strokeColor = p.color.withAlphaComponent(0.2).cgColor
            } else {
                fill.fillColor = p.color.cgColor
            }
        }
    }

    private func place() {
        guard let p = shown else { return }
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        colors()
        let b = bounds
        if ring {
            let r = min(b.width, b.height) / 2 - 0.75
            // From the top, clockwise (the view is flipped).
            let path = CGMutablePath()
            path.addArc(center: CGPoint(x: b.midX, y: b.midY), radius: r, startAngle: -.pi / 2, endAngle: 1.5 * .pi, clockwise: false)
            for l in [track, fill] {
                l.frame = b
                l.path = path
                l.lineWidth = 1.5
            }
        } else {
            fill.frame = b
        }
        CATransaction.commit()
        if let f = p.fraction {
            fill.removeAnimation(forKey: "moving")
            if ring {
                fill.strokeStart = 0
                fill.strokeEnd = f
            } else {
                // Grows with Core Animation's own short animation, as Ghostty's does.
                fill.path = CGPath(rect: CGRect(x: 0, y: 0, width: b.width * f, height: b.height), transform: nil)
            }
        } else if fill.animation(forKey: "moving") == nil || b != lastBounds {
            fill.removeAnimation(forKey: "moving")
            if ring {
                fill.strokeStart = 0
                fill.strokeEnd = 0.25
                let spin = CABasicAnimation(keyPath: "transform.rotation.z")
                spin.fromValue = 0
                spin.toValue = 2 * Double.pi
                spin.duration = 1
                spin.repeatCount = .infinity
                // Around the ring's middle.
                fill.anchorPoint = CGPoint(x: 0.5, y: 0.5)
                fill.add(spin, forKey: "moving")
            } else {
                let piece = b.width * 0.25
                fill.path = CGPath(rect: CGRect(x: 0, y: 0, width: piece, height: b.height), transform: nil)
                let move = CABasicAnimation(keyPath: "transform.translation.x")
                move.fromValue = 0
                move.toValue = b.width - piece
                move.duration = 1.2
                move.autoreverses = true
                move.repeatCount = .infinity
                move.timingFunction = CAMediaTimingFunction(name: .easeInEaseOut)
                fill.add(move, forKey: "moving")
            }
        }
        lastBounds = b
    }

    private var lastBounds = CGRect.zero
}
