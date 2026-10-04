import AppKit
import DinoGhostty

/// A pane's scrollbar, as Ghostty draws its own: the Mac's overlay scroller at the right edge,
/// shown while you scroll through the scrollback (or, with scroll bars set to always show in
/// System Settings, when the pointer comes to that edge) and faded out after, by AppKit. Dragging
/// its knob scrolls (Ghostty's `scroll_to_row`), as does a click in its track.
/// `scrollbar = never` in the Ghostty config leaves it out.
///
/// Ghostty's own app puts the whole surface in a scroll view; here a scroll view as narrow as the
/// scroller stands at the pane's edge, its document as tall as the scrollback, and only takes
/// clicks on the scroller while it shows. Ghostty reports the scrollbar with every change of the
/// screen: those are only kept while the scroller is out of sight (output under a view that
/// follows the bottom), and nothing in SwiftUI hears of them.
@MainActor
final class PaneScroller: NSScrollView {
    /// Rows: the whole scrollback and screen, the first one shown, how many are shown.
    private var bar: TerminalScrollbar?
    private var shownUntil = Date.distantPast
    private var lastRow: UInt64?
    private var live = false
    private var observers: [NSObjectProtocol] = []
    private let scrollTo: (UInt64) -> Void
    private weak var terminal: NSView?

    /// `scrollTo`: the row to show at the top. Wheel and trackpad scrolling over it go to `terminal`.
    init(terminal: NSView, scrollTo: @escaping (UInt64) -> Void) {
        self.terminal = terminal
        self.scrollTo = scrollTo
        super.init(frame: NSRect(x: 0, y: 0, width: Self.width, height: 100))
        drawsBackground = false
        hasVerticalScroller = true
        hasHorizontalScroller = false
        autohidesScrollers = false
        scrollerStyle = .overlay
        verticalScrollElasticity = .none
        autoresizingMask = [.minXMargin, .height]
        documentView = NSView(frame: bounds)
        let center = NotificationCenter.default
        observers = [
            center.addObserver(forName: NSScrollView.willStartLiveScrollNotification, object: self, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.live = true }
            },
            center.addObserver(forName: NSScrollView.didLiveScrollNotification, object: self, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.liveScrolled() }
            },
            center.addObserver(forName: NSScrollView.didEndLiveScrollNotification, object: self, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated {
                    self?.live = false
                    self?.apply()
                }
            },
            // Overlay always, as in Ghostty: a mouse's "always show" style would take a column.
            center.addObserver(forName: NSScroller.preferredScrollerStyleDidChangeNotification, object: nil, queue: nil) { [weak self] _ in
                MainActor.assumeIsolated { self?.scrollerStyle = .overlay }
            },
        ]
    }

    @available(*, unavailable)
    required init?(coder _: NSCoder) { fatalError() }

    deinit {
        observers.forEach { NotificationCenter.default.removeObserver($0) }
    }

    static var width: CGFloat { NSScroller.scrollerWidth(for: .regular, scrollerStyle: .overlay) }

    /// Ghostty's latest numbers. Shows the scroller when the view moved through the scrollback,
    /// not when output only added lines under a view that follows the bottom.
    func update(_ new: TerminalScrollbar) {
        let old = bar
        bar = new
        guard !live else { return }
        if let old, old.offset != new.offset, !(Self.atBottom(old) && Self.atBottom(new)) {
            flash()
        } else if Date() < shownUntil {
            apply()
        }
    }

    private static func atBottom(_ b: TerminalScrollbar) -> Bool {
        b.offset + b.len >= b.total
    }

    /// The pointer is at the pane's right edge: a shown scroller stays there to be grabbed (AppKit
    /// keeps it while the pointer is on it). With scroll bars that always show (a mouse), that's
    /// also how one reaches the scroller, as in Ghostty.
    func pointerAtEdge() {
        if Date() < shownUntil {
            shownUntil = Date().addingTimeInterval(1.5)
        } else if NSScroller.preferredScrollerStyle == .legacy {
            flash()
        }
    }

    func flash() {
        guard let bar, bar.total > bar.len else { return }
        apply()
        flashScrollers()
        // AppKit hides it about a second after; numbers are kept up while it may show.
        shownUntil = Date().addingTimeInterval(1.5)
    }

    /// The document as tall as the scrollback, scrolled to where the view is.
    private func apply() {
        guard let bar, let documentView else { return }
        let height = contentSize.height
        guard height > 0, bar.len > 0 else { return }
        let rowHeight = height / CGFloat(bar.len)
        let total = max(CGFloat(bar.total) * rowHeight, height)
        if documentView.frame.size != NSSize(width: contentSize.width, height: total) {
            documentView.frame.size = NSSize(width: contentSize.width, height: total)
        }
        // AppKit's y goes up: the rows below the view are what's under the visible rect.
        let y = CGFloat(bar.total - min(bar.total, bar.offset + bar.len)) * rowHeight
        if contentView.bounds.origin.y != y {
            contentView.scroll(to: NSPoint(x: 0, y: y))
            reflectScrolledClipView(contentView)
        }
        lastRow = bar.offset
    }

    /// The knob dragged, or the track clicked: the row now at the top, to Ghostty.
    private func liveScrolled() {
        guard let bar, bar.len > 0 else { return }
        let rowHeight = contentSize.height / CGFloat(bar.len)
        guard rowHeight > 0 else { return }
        let fromBottom = contentView.bounds.origin.y / rowHeight
        let top = Double(bar.total) - Double(bar.len) - Double(fromBottom)
        let row = UInt64(max(0, min(top.rounded(), Double(bar.total - min(bar.total, bar.len)))))
        guard row != lastRow else { return }
        lastRow = row
        scrollTo(row)
    }

    override func scrollWheel(with event: NSEvent) {
        terminal?.scrollWheel(with: event)
    }

    /// Only the scroller, and only while AppKit shows it; the rest of the strip is the terminal's.
    override func hitTest(_ point: NSPoint) -> NSView? {
        let hit = super.hitTest(point)
        guard let scroller = verticalScroller, let hit, hit === scroller || hit.isDescendant(of: scroller) else { return nil }
        return Date() < shownUntil || live ? hit : nil
    }
}

extension GhosttyConfig {
    /// `scrollbar` isn't `never`. Read with the config, not with each of Ghostty's reports.
    private(set) static var showsScrollbar = true
    /// Light or dark scrollers, as the terminal's background is, as in Ghostty.
    private(set) static var scrollerAppearance: NSAppearance?

    static func readPaneChrome(_ controller: TerminalController) {
        showsScrollbar = controller.configText("scrollbar") != "never"
        scrollerAppearance = controller.configColor("background").map { c in
            let light = 0.299 * Double(c.red) + 0.587 * Double(c.green) + 0.114 * Double(c.blue) > 127.5
            return NSAppearance(named: light ? .aqua : .darkAqua)
        } ?? nil
    }
}
