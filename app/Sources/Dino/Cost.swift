import AppKit
import SwiftUI

/// What a session's processes cost the Mac (its program and everything under it), as dinod
/// measures them for the hover card: memory is the physical footprint, CPU is per core.
struct SessionCost: Codable, Equatable {
    var mem_bytes: UInt64?
    var cpu_pct: Double?
    var cpu_avg_pct: Double?
    var avg_secs: UInt32?
    var processes: UInt32?
    var top_child: ProcessCost?
}

struct ProcessCost: Codable, Equatable {
    var name: String?
    var pid: UInt32?
    var mem_bytes: UInt64?
    var cpu_pct: Double?
}

private struct SessionCostResponse: Decodable {
    var cost: SessionCost
}

extension DinoConnection {
    /// dinod measures only when asked: ask while the answer shows, and stop after.
    func sessionCost(session: String) throws -> SessionCost {
        try JSONDecoder().decode(SessionCostResponse.self, from: send(["type": "session_cost", "id": session])).cost
    }
}

/// Sizes and CPU as Activity Monitor writes them: "479.2 MB", "1.25 GB", "85.3%".
enum CostFormat {
    static func memory(_ bytes: UInt64?) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(clamping: bytes ?? 0), countStyle: .memory)
    }

    static func cpu(_ pct: Double?) -> String {
        String(format: "%.1f%%", max(0, pct ?? 0))
    }
}

/// The sidebar row's hover card: after the pointer rests on a row, a small panel beside it shows
/// what that session costs, and stays current while it's up. Nothing is measured otherwise.
@MainActor
final class CostCard {
    static let shared = CostCard()

    /// How long the pointer rests on a row before the card shows, as a tooltip waits.
    private static let delay: Duration = .milliseconds(800)
    /// How often it asks again while it shows (Activity Monitor's "Often").
    private static let every: Duration = .seconds(2)

    private var panel: NSPanel?
    private let feed = CostFeed()
    private var task: Task<Void, Never>?
    private var session: String?

    /// The pointer entered (`on`) or left session `id`'s row; `anchor` is the row's view.
    func hover(_ id: String, anchor: NSView?, on: Bool) {
        if !on {
            // Left a row that isn't the one shown (moving between rows can say so late).
            guard session == id else { return }
            hide()
            return
        }
        hide()
        session = id
        feed.reset()
        task = Task { [weak self, feed] in
            try? await Task.sleep(for: Self.delay)
            guard !Task.isCancelled, let self, let anchor, anchor.window?.isVisible == true else { return }
            // One connection while it shows: no reconnect per look, and the app's main one stays free.
            let conn = await Task.detached { try? DinoConnection(path: DinoEnvironment.socketPath) }.value
            guard !Task.isCancelled else { return }
            var shown = false
            while !Task.isCancelled {
                // The pointer moved off without the row hearing (a row removed under it, the app
                // in the background): stop looking.
                if shown, !Self.pointer(over: anchor) || !NSApp.isActive {
                    self.hide()
                    return
                }
                let result = await Task.detached { () -> Result<SessionCost, Error> in
                    guard let conn else { return .failure(DinoError.socket("can't reach dinod")) }
                    return Result { try conn.sessionCost(session: id) }
                }.value
                guard !Task.isCancelled else { return }
                switch result {
                case let .success(cost): feed.set(cost)
                case .failure:
                    // Exited, remote, or dinod predates it: nothing worth a card.
                    if !shown { return }
                    feed.stale()
                }
                if !shown {
                    shown = true
                    // Laid out with the first numbers; the card keeps its size after.
                    await Task.yield()
                    self.show(beside: anchor)
                }
                try? await Task.sleep(for: Self.every)
            }
        }
    }

    private static func pointer(over view: NSView) -> Bool {
        guard let window = view.window, window.isVisible else { return false }
        let row = window.convertToScreen(view.convert(view.bounds, to: nil))
        return NSMouseInRect(NSEvent.mouseLocation, row, false)
    }

    func hide() {
        task?.cancel()
        task = nil
        session = nil
        panel?.orderOut(nil)
    }

    private func show(beside anchor: NSView) {
        guard let window = anchor.window else { return }
        let panel = panel ?? makePanel()
        let row = window.convertToScreen(anchor.convert(anchor.bounds, to: nil))
        let size = panel.contentView?.fittingSize ?? NSSize(width: 240, height: 120)
        // Beside the row, over the terminal; kept on the row's screen.
        var origin = NSPoint(x: row.maxX + 6, y: row.maxY - size.height)
        if let area = window.screen?.visibleFrame {
            if origin.x + size.width > area.maxX { origin.x = row.minX - size.width - 6 }
            origin.y = min(max(origin.y, area.minY), area.maxY - size.height)
        }
        panel.setFrame(NSRect(origin: origin, size: size), display: true)
        panel.orderFront(nil)
    }

    private func makePanel() -> NSPanel {
        // Never key, never in the way: the terminal keeps the keyboard and the pointer passes through.
        let panel = NSPanel(contentRect: .zero, styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: true)
        panel.isFloatingPanel = true
        panel.level = .floating
        panel.hidesOnDeactivate = true
        panel.ignoresMouseEvents = true
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = true
        panel.isReleasedWhenClosed = false
        panel.collectionBehavior = [.transient, .ignoresCycle, .fullScreenAuxiliary]
        let host = NSHostingView(rootView: CostCardView(feed: feed))
        host.sizingOptions = [.intrinsicContentSize]
        panel.contentView = host
        self.panel = panel
        return panel
    }
}

/// What the card shows; written only when it changes.
@MainActor
final class CostFeed: ObservableObject {
    @Published private(set) var cost: SessionCost?
    /// The last ask failed: the numbers are from before.
    @Published private(set) var isStale = false

    func set(_ cost: SessionCost) {
        if self.cost != cost { self.cost = cost }
        if isStale { isStale = false }
    }

    func stale() {
        if !isStale { isStale = true }
    }

    func reset() {
        if cost != nil { cost = nil }
        if isStale { isStale = false }
    }
}

struct CostCardView: View {
    @ObservedObject var feed: CostFeed

    var body: some View {
        let c = feed.cost
        VStack(alignment: .leading, spacing: 6) {
            // Every line always there, so the card keeps its size as numbers come and go.
            Grid(alignment: .leading, horizontalSpacing: 12, verticalSpacing: 3) {
                GridRow {
                    Text("Memory").foregroundStyle(.secondary)
                    Text(CostFormat.memory(c?.mem_bytes)).gridColumnAlignment(.trailing)
                }
                GridRow {
                    Text("CPU").foregroundStyle(.secondary)
                    Text(CostFormat.cpu(c?.cpu_pct))
                }
                GridRow {
                    if let secs = c?.avg_secs, secs >= 4 {
                        Text("Last \(Self.span(secs))").foregroundStyle(.secondary)
                        Text(CostFormat.cpu(c?.cpu_avg_pct))
                    } else {
                        Text("Average").foregroundStyle(.secondary)
                        Text("—").foregroundStyle(.tertiary)
                    }
                }
            }
            .font(.callout.monospacedDigit())
            Divider()
            VStack(alignment: .leading, spacing: 1) {
                Text("Biggest under it").font(.caption).foregroundStyle(.secondary)
                if let top = c?.top_child, let name = top.name {
                    Text("\(name)  \(CostFormat.memory(top.mem_bytes)) · \(CostFormat.cpu(top.cpu_pct))")
                        .font(.callout.monospacedDigit()).lineLimit(1).truncationMode(.middle)
                } else {
                    Text("Nothing running").font(.callout).foregroundStyle(.tertiary)
                }
            }
            Text(Self.count(c?.processes)).font(.caption).foregroundStyle(.tertiary)
        }
        .opacity(feed.isStale ? 0.5 : 1)
        .padding(10)
        .frame(width: 220, alignment: .leading)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 8))
        .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(.separator))
        .accessibilityElement(children: .combine)
    }

    private static func count(_ n: UInt32?) -> String {
        switch n ?? 0 {
        case 0, 1: "Its own process only"
        case let n: "\(n) processes together"
        }
    }

    private static func span(_ secs: UInt32) -> String {
        secs < 90 ? "\(secs) s" : "\((secs + 30) / 60) min"
    }
}

/// The row's view, for placing the card beside it. A plain holder: setting it redraws nothing.
final class CostAnchorView {
    weak var view: NSView?
}

struct CostAnchor: NSViewRepresentable {
    let holder: CostAnchorView

    func makeNSView(context: Context) -> NSView {
        let v = NSView()
        holder.view = v
        return v
    }

    func updateNSView(_ nsView: NSView, context: Context) {
        holder.view = nsView
    }
}
