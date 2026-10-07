import AppKit
import SwiftUI

/// ⇧⌘P: every action by name. Built from the menu bar itself, so whatever a menu can do the
/// palette can, with the same words and keys, plus a jump to each session.
struct CommandPalette: View {
    @EnvironmentObject var model: DinoModel
    @Environment(\.dismiss) private var dismiss
    @State private var query = ""
    @State private var picked = 0
    @State private var actions: [Action] = []
    @FocusState private var focused: Bool

    struct Action: Identifiable {
        var title: String
        var path: String
        var keys: String
        var run: () -> Void
        var id: String { path + title }
    }

    private var shown: [Action] {
        let q = query.trimmingCharacters(in: .whitespaces)
        guard !q.isEmpty else { return actions }
        let words = q.lowercased().split(separator: " ")
        return actions.filter { a in
            let text = (a.path + a.title).lowercased()
            return words.allSatisfy { text.contains($0) }
        }
    }

    var body: some View {
        let shown = shown
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass").foregroundStyle(.secondary).accessibilityHidden(true)
                TextField("Search actions and sessions", text: $query)
                    .textFieldStyle(.plain)
                    .font(.title3)
                    .focused($focused)
                    .onSubmit { run(shown) }
                    .onKeyPress(.downArrow) { picked = min(picked + 1, max(shown.count - 1, 0)); return .handled }
                    .onKeyPress(.upArrow) { picked = max(picked - 1, 0); return .handled }
                    .accessibilityLabel("Search actions")
            }
            .padding(14)
            Divider()
            ScrollViewReader { proxy in
                List {
                    ForEach(Array(shown.enumerated()), id: \.element.id) { i, a in
                        HStack(spacing: 8) {
                            Text(a.title).lineLimit(1)
                            if !a.path.isEmpty {
                                Text(a.path).foregroundStyle(.secondary).lineLimit(1)
                            }
                            Spacer(minLength: 8)
                            Text(a.keys).font(.callout.monospaced()).foregroundStyle(.secondary)
                        }
                        .padding(.vertical, 3)
                        .padding(.horizontal, 6)
                        .background(RoundedRectangle(cornerRadius: 5).fill(i == picked ? Color.accentColor.opacity(0.22) : .clear))
                        .contentShape(Rectangle())
                        .onTapGesture { picked = i; run(shown) }
                        .listRowSeparator(.hidden)
                        .id(i)
                        .accessibilityElement(children: .combine)
                        .accessibilityAddTraits(.isButton)
                    }
                    if shown.isEmpty {
                        Text("Nothing matches “\(query)”").foregroundStyle(.tertiary)
                    }
                }
                .listStyle(.plain)
                .onChange(of: picked) { _, i in proxy.scrollTo(i) }
            }
        }
        .frame(width: 560, height: 420)
        .onChange(of: query) { _, _ in picked = 0 }
        .onAppear {
            actions = Self.menuActions() + sessionActions
            focused = true
        }
    }

    private var sessionActions: [Action] {
        model.sessions.filter { $0.id != model.selected }.map { s in
            Action(title: s.display, path: "Go to session", keys: "") { [weak model] in model?.select(s.id) }
        }
    }

    private func run(_ shown: [Action]) {
        guard shown.indices.contains(picked) else { return }
        let action = shown[picked]
        dismiss()
        model.showPalette = false
        // After the sheet is gone, so what the action opens isn't stacked on it.
        DispatchQueue.main.async { action.run() }
    }

    /// Every enabled item in the menu bar, as "Title  Menu › Submenu  ⇧⌘K".
    static func menuActions() -> [Action] {
        guard let bar = NSApp.mainMenu else { return [] }
        // File and Session first (what you come here for: File's New Session is the one picked on
        // Return), the app menu (About, Hide, Quit) last.
        let menus = bar.items.dropFirst()
        let first = ["File", "Session"]
        let ordered = first.flatMap { t in menus.filter { $0.title == t } } + menus.filter { !first.contains($0.title) } + bar.items.prefix(1)
        return ordered.flatMap { item -> [Action] in
            guard let menu = item.submenu else { return [] }
            // Window: dino's own (sessions, tabs, splits), not macOS's Minimize, Zoom and tiling.
            return actions(in: menu, path: item.title, ownOnly: menu === NSApp.windowsMenu)
        }
    }

    /// `ownOnly`: only what SwiftUI put there (an item with a target); AppKit's own go to the
    /// first responder.
    private static func actions(in menu: NSMenu, path: String, ownOnly: Bool = false) -> [Action] {
        // SwiftUI fills its menus when they're about to open, through the delegate: without this
        // the palette saw them as they were at launch (no agents yet, so no New Tab).
        menu.delegate?.menuNeedsUpdate?(menu)
        menu.update()
        return menu.items.flatMap { item -> [Action] in
            if ownOnly, item.target == nil { return [] }
            if let sub = item.submenu {
                guard sub !== NSApp.servicesMenu, item.isEnabled else { return [] }
                return actions(in: sub, path: path + " › " + item.title, ownOnly: ownOnly)
            }
            guard !item.isHidden, !item.isSeparatorItem, item.isEnabled, !item.title.isEmpty, item.action != nil,
                  item.title != paletteTitle else { return [] }
            if let action = item.action, ShortcutSheet.systemActions.contains(action) { return [] }
            // ⌘W (Close Tab, Pane or Window): one key away already, and a palette Return on it would
            // close what you were looking at.
            if item.keyEquivalent == "w", item.keyEquivalentModifierMask == .command { return [] }
            let keys = item.keyEquivalent.isEmpty ? "" : ShortcutSheet.keys(item)
            return [Action(title: item.title, path: path, keys: keys) { [weak item] in
                guard let item, let menu = item.menu else { return }
                let i = menu.index(of: item)
                if i >= 0 { menu.performActionForItem(at: i) }
            }]
        }
    }

    static let paletteTitle = "Command Palette…"
}
