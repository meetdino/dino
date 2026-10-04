import AppKit
import Combine
import DinoGhostty
import SwiftUI

/// What a pane's find bar shows: the text searched for and Ghostty's count.
@MainActor
final class FindState: ObservableObject {
    @Published var needle: String
    @Published var total: Int?
    @Published var selected: Int?
    /// Bumped to put the keyboard in the field again (⌘F with the bar open).
    @Published var focusRequests = 0

    init(needle: String) {
        self.needle = needle
    }
}

/// Ghostty's find bar over one pane (⌘F; its `start_search`): Ghostty searches the scrollback and
/// highlights every match in the `search-*` colors; the bar shows "selected/total", Return and
/// ⌘G go to the next match, ⇧Return and ⇧⌘G to the previous, Esc goes back to the terminal (and
/// closes the bar when the field is empty, or from the terminal). It sits in a corner, the top
/// right at first, and can be dragged to another. The text searched for is the Mac's find text
/// too, so ⌘F in another app picks it up, and the bar starts with that text.
///
/// A view of the pane's own, not of the SwiftUI pane around it: it only exists while it's open,
/// and Ghostty's count changes nothing but the bar.
@MainActor
final class FindBar: NSView {
    let state: FindState
    private weak var terminal: LinkTerminalView?
    private var hosting: NSHostingView<FindBarView>!
    private var corner = Corner.topRight
    private var dragStart: NSPoint?
    private var pending: DispatchWorkItem?
    private var sent: String?

    /// Around the bar: room for its shadow. The bar sits `inset` from the pane's edges.
    private static let margin: CGFloat = 6
    private static let inset: CGFloat = 8

    enum Corner { case topLeft, topRight, bottomLeft, bottomRight }

    init(terminal: LinkTerminalView, needle: String) {
        state = FindState(needle: needle)
        self.terminal = terminal
        super.init(frame: .zero)
        let view = FindBarView(
            state: state,
            margin: Self.margin,
            navigate: { [weak self] next in self?.navigate(next: next) },
            escape: { [weak self] in self?.escape() },
            close: { [weak self] in self?.close() },
            drag: { [weak self] translation in self?.drag(translation) }
        )
        hosting = NSHostingView(rootView: view)
        hosting.sizingOptions = [.intrinsicContentSize]
        let size = hosting.fittingSize
        hosting.frame = NSRect(origin: .zero, size: size)
        frame.size = size
        addSubview(hosting)
        place(in: terminal.bounds)
        observe()
    }

    @available(*, unavailable)
    required init?(coder _: NSCoder) { fatalError() }

    /// Sends the text to Ghostty as you type: at once from three characters (and when emptied),
    /// a moment after the last key below that, as Ghostty does to keep a short needle from
    /// searching the whole scrollback at every key.
    private var needleWatch: AnyCancellable?
    private func observe() {
        needleWatch = state.$needle.removeDuplicates().sink { [weak self] needle in
            guard let self else { return }
            pending?.cancel()
            let work = DispatchWorkItem { [weak self] in self?.search(needle) }
            pending = work
            if needle.isEmpty || needle.count >= 3 {
                DispatchQueue.main.async(execute: work)
            } else {
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.3, execute: work)
            }
        }
    }

    private func search(_ needle: String) {
        guard needle != sent else { return }
        sent = needle
        terminal?.terminalState?.performBindingAction("search:\(needle)")
        if !needle.isEmpty { FindBar.findText = needle }
    }

    /// The Mac's find text (the find pasteboard), shared by every app's find.
    static var findText: String? {
        get { NSPasteboard(name: .find).string(forType: .string).flatMap { $0.isEmpty ? nil : $0 } }
        set {
            let board = NSPasteboard(name: .find)
            guard let newValue, board.string(forType: .string) != newValue else { return }
            board.clearContents()
            board.setString(newValue, forType: .string)
        }
    }

    func focusField() {
        state.focusRequests += 1
    }

    /// Ghostty's `navigate_search`: a match up (next) or down (previous) the scrollback.
    func navigate(next: Bool) {
        // A needle still waiting for its pause goes first, so ⏎ finds what's typed.
        if let pending, !pending.isCancelled { pending.perform(); pending.cancel() }
        terminal?.terminalState?.performBindingAction(next ? "navigate_search:next" : "navigate_search:previous")
    }

    /// Esc in the field: back to the terminal, matches still lit; with nothing typed, closes.
    private func escape() {
        if state.needle.isEmpty { close() } else { terminal?.focusTerminal() }
    }

    func close() {
        pending?.cancel()
        terminal?.endSearch(tellGhostty: true)
    }

    /// The field's own keys for what the terminal does with them: ⌘G and ⇧⌘G go through the
    /// field (the terminal isn't first responder while you type here), ⌘F keeps the keyboard here.
    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        guard let responder = window?.firstResponder as? NSView, responder.isDescendant(of: self) else {
            return super.performKeyEquivalent(with: event)
        }
        let mods = event.modifierFlags.intersection([.command, .shift, .option, .control])
        switch (mods, event.charactersIgnoringModifiers?.lowercased() ?? "") {
        case ([.command], "g"): navigate(next: true)
        case ([.command, .shift], "g"): navigate(next: false)
        case ([.command], "f"): focusField()
        default: return super.performKeyEquivalent(with: event)
        }
        return true
    }

    // MARK: Where it sits

    /// In its corner of `bounds`, kept there as the pane resizes.
    private func place(in bounds: NSRect) {
        let left = corner == .topLeft || corner == .bottomLeft
        let top = corner == .topLeft || corner == .topRight
        let edge = Self.inset - Self.margin
        frame.origin = NSPoint(
            x: left ? edge : bounds.width - frame.width - edge,
            y: top ? bounds.height - frame.height - edge : edge
        )
        autoresizingMask = [left ? .maxXMargin : .minXMargin, top ? .minYMargin : .maxYMargin]
    }

    /// Dragged (window points, y down): follows the pointer; let go, it goes to the nearest corner.
    private func drag(_ translation: CGSize?) {
        guard let terminal else { return }
        guard let translation else {
            dragStart = nil
            let center = NSPoint(x: frame.midX, y: frame.midY)
            let left = center.x < terminal.bounds.midX
            let top = center.y > terminal.bounds.midY
            corner = top ? (left ? .topLeft : .topRight) : (left ? .bottomLeft : .bottomRight)
            let from = frame.origin
            place(in: terminal.bounds)
            let to = frame.origin
            frame.origin = from
            NSAnimationContext.runAnimationGroup { context in
                context.duration = 0.2
                context.timingFunction = CAMediaTimingFunction(name: .easeOut)
                animator().setFrameOrigin(to)
            }
            return
        }
        let start = dragStart ?? frame.origin
        dragStart = start
        setFrameOrigin(NSPoint(x: start.x + translation.width, y: start.y - translation.height))
    }
}

/// The bar itself, as Ghostty 1.3 draws it: a plain field with the count in it, up and down, ×.
struct FindBarView: View {
    @ObservedObject var state: FindState
    let margin: CGFloat
    let navigate: (Bool) -> Void
    let escape: () -> Void
    let close: () -> Void
    let drag: (CGSize?) -> Void
    @FocusState private var focused: Bool

    var body: some View {
        HStack(spacing: 4) {
            TextField("Search", text: $state.needle)
                .textFieldStyle(.plain)
                .frame(width: 180)
                .padding(.leading, 8)
                .padding(.trailing, 50)
                .padding(.vertical, 6)
                .background(Color.primary.opacity(0.1))
                .cornerRadius(6)
                .focused($focused)
                .overlay(alignment: .trailing) { count }
                .onExitCommand(perform: escape)
                .onKeyPress(keys: [.return]) { press in
                    navigate(!press.modifiers.contains(.shift))
                    return .handled
                }
                .accessibilityLabel("Find in terminal")
            Button { navigate(true) } label: { Image(systemName: "chevron.up") }
                .buttonStyle(FindButtonStyle())
                .help("Next match (⌘G, ↩)")
                .accessibilityLabel("Next match")
            Button { navigate(false) } label: { Image(systemName: "chevron.down") }
                .buttonStyle(FindButtonStyle())
                .help("Previous match (⇧⌘G, ⇧↩)")
                .accessibilityLabel("Previous match")
            Button(action: close) { Image(systemName: "xmark") }
                .buttonStyle(FindButtonStyle())
                .help("Close (Esc)")
                .accessibilityLabel("Close find bar")
        }
        .padding(8)
        .background(.background)
        .clipShape(RoundedRectangle(cornerRadius: 8))
        .shadow(radius: 4)
        .gesture(
            DragGesture(coordinateSpace: .global)
                .onChanged { drag($0.translation) }
                .onEnded { _ in drag(nil) }
        )
        .padding(margin)
        .onAppear { focused = true }
        .onChange(of: state.focusRequests) { _, _ in focused = true }
    }

    @ViewBuilder private var count: some View {
        if let selected = state.selected {
            Text("\(selected + 1)/\(state.total.map(String.init) ?? "?")")
                .font(.caption).foregroundStyle(.secondary).monospacedDigit().padding(.trailing, 8)
        } else if let total = state.total {
            Text("-/\(total)")
                .font(.caption).foregroundStyle(.secondary).monospacedDigit().padding(.trailing, 8)
        }
    }
}

private struct FindButtonStyle: ButtonStyle {
    @State private var hovered = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .foregroundStyle(hovered || configuration.isPressed ? .primary : .secondary)
            .padding(.horizontal, 2)
            .frame(height: 26)
            .background(
                RoundedRectangle(cornerRadius: 6)
                    .fill(Color.primary.opacity(configuration.isPressed ? 0.2 : hovered ? 0.1 : 0))
            )
            .onHover { hovered = $0 }
    }
}

/// Edit's items for the pane with the keyboard, or the selected session's: Clear, Reset Terminal
/// and Ghostty's Find menu. Ghostty's ⌘K clears; dino's ⌘K is Continue a Session, so Clear is ⌥⌘K
/// (Terminal's key for clearing the scrollback). ⌘J and ⇧⌘F are dino's too (Jump to Session
/// Needing You, Find Sessions), so Hide Find Bar and Jump to Selection have no key: Esc hides.
struct TerminalEditItems: View {
    /// Not observed: the items look up the pane when chosen, and change with nothing.
    let model: DinoModel

    var body: some View {
        Divider()
        Button("Clear") { LinkTerminalView.current(model)?.clearScreen(nil) }
            .keyboardShortcut("k", modifiers: [.command, .option])
        Button("Reset Terminal") { LinkTerminalView.current(model)?.resetTerminal(nil) }
        Divider()
        Menu("Find") {
            Button("Find…") { LinkTerminalView.current(model)?.find(.find) }
                .keyboardShortcut("f")
            Button("Find Next") { LinkTerminalView.current(model)?.find(.next) }
                .keyboardShortcut("g")
            Button("Find Previous") { LinkTerminalView.current(model)?.find(.previous) }
                .keyboardShortcut("g", modifiers: [.command, .shift])
            Button("Hide Find Bar") { LinkTerminalView.current(model)?.find(.hide) }
            Divider()
            Button("Use Selection for Find") { LinkTerminalView.current(model)?.find(.useSelection) }
                .keyboardShortcut("e")
            Button("Jump to Selection") { LinkTerminalView.current(model)?.find(.jumpToSelection) }
        }
    }
}
