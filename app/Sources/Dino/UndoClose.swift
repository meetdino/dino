import AppKit

/// Undo Close Tab and Undo Close Pane (⌘Z, ⇧⌘T: Ghostty's `undo`, and Edit › Undo), within
/// Ghostty's `undo-timeout` (5 s unless set). An agent's tab only hid it, so undoing shows it
/// again. A shell's process would be gone: dinod keeps a closed shell running, hidden, until the
/// time to undo is up, so ⌘Z brings back the same shell with its scrollback; then it really ends.
/// Each close is undone on its own and expires on its own, as in Ghostty.
@MainActor
final class ClosedLayout: NSObject {
    /// The tabs it took away, with where each was.
    let tabs: [(id: String, at: Int)]
    /// The splits they were in.
    let splits: [Split]
    /// The shells dinod keeps for now, to bring back.
    let shells: [String]
    let selected: String?
    let name: String
    /// Close it all again (Redo).
    let again: () -> Void
    /// What undoing (or redoing) it does. Registered by target and selector, not with a block: the
    /// undo manager only finds a block's actions by target to drop them once their time is up.
    var perform: (() -> Void)?

    @objc func act(_: Any?) { perform?() }

    init(tabs: [(id: String, at: Int)], splits: [Split], shells: [String], selected: String?, name: String, again: @escaping () -> Void) {
        self.tabs = tabs
        self.splits = splits
        self.shells = shells
        self.selected = selected
        self.name = name
        self.again = again
    }
}

extension DinoModel {
    /// The main window's undo manager: Edit › Undo, and Ghostty's `undo` from a pane.
    var undoManager: UndoManager? {
        NSApp.windows.first { $0.identifier?.rawValue.hasPrefix("main") == true }?.undoManager
    }

    /// The time a close can be undone in, from `undo-timeout`; zero: closes can't be undone.
    private var undoTimeout: TimeInterval { TimeInterval(PaneSignals.config.undoMs) / 1000 }

    /// Shell `id` ends: at once when closes can't be undone, else once the time to undo is up
    /// (dinod keeps it hidden till then). A dinod from before undo ends it at once.
    func endShell(_ id: String) {
        let ms = PaneSignals.config.undoMs
        guard ms > 0, let conn = connection else { return kill(id) }
        Task.detached {
            do { try conn.closeLater(session: id, undoMs: ms) } catch { _ = try? conn.request(["type": "kill", "id": id]) }
        }
    }

    /// What a close is about to take away, to undo it: call before, then `closed(_:)` after.
    func layoutBefore() -> (tabs: [String], splits: [Split], selected: String?) {
        (tabs, splits, selected)
    }

    /// A close happened since `before`: it can be undone for `undo-timeout`.
    func closed(since before: (tabs: [String], splits: [Split], selected: String?), members: [String], shells: [String], name: String, again: @escaping () -> Void) {
        guard undoTimeout > 0, let manager = undoManager else { return }
        let gone = before.tabs.enumerated().filter { !tabs.contains($0.element) }.map { (id: $0.element, at: $0.offset) }
        let splits = before.splits.filter { s in members.contains { s.contains($0) } }
        guard !gone.isEmpty || !splits.isEmpty else { return }
        let record = ClosedLayout(tabs: gone, splits: splits, shells: shells,
                                  selected: before.selected.flatMap { members.contains($0) ? $0 : nil } ?? members.first,
                                  name: name, again: again)
        undoRecords.append(record)
        record.perform = { [weak self, weak record] in
            guard let self, let record else { return }
            self.undo(record)
        }
        manager.registerUndo(withTarget: record, selector: #selector(ClosedLayout.act(_:)), object: nil)
        manager.setActionName(name)
        expire(record, in: manager)
    }

    /// Each close's undo goes once its time is up; its shells end in dinod at the same moment.
    private func expire(_ record: ClosedLayout, in manager: UndoManager) {
        DispatchQueue.main.asyncAfter(deadline: .now() + undoTimeout) { [weak self, weak manager] in
            manager?.removeAllActions(withTarget: record)
            self?.undoRecords.removeAll { $0 === record }
        }
    }

    private func undo(_ r: ClosedLayout) {
        undoRecords.removeAll { $0 === r }
        // Redo closes it again, for as long as an undo would have lasted.
        if let manager = undoManager {
            let redo = ClosedLayout(tabs: r.tabs, splits: r.splits, shells: r.shells, selected: r.selected, name: r.name, again: r.again)
            undoRecords.append(redo)
            redo.perform = { [weak self, weak redo] in
                guard let self, let redo else { return }
                self.undoRecords.removeAll { $0 === redo }
                redo.again()
            }
            manager.registerUndo(withTarget: redo, selector: #selector(ClosedLayout.act(_:)), object: nil)
            manager.setActionName(r.name)
            expire(redo, in: manager)
        }
        guard !r.shells.isEmpty else { return restore(r) }
        guard let conn = connection else { return NSSound.beep() }
        // Back in dinod first: the tabs come back once a state lists the shells again, or the next
        // poll would drop them as gone.
        reopening = r
        Task.detached {
            let back = r.shells.allSatisfy { id in (try? conn.reopenClosed(session: id)) != nil }
            await MainActor.run {
                // Its time ran out as you pressed it: a shell that did come back gets a tab as any
                // shell does.
                if !back {
                    if self.reopening === r { self.reopening = nil }
                    NSSound.beep()
                }
            }
        }
    }

    /// Called with each state: a close being undone whose shells are listed again is put back.
    func restoreReopened(_ live: Set<String>) {
        guard let r = reopening, r.shells.allSatisfy({ live.contains($0) }) else { return }
        reopening = nil
        restore(r)
    }

    /// The tabs and splits a close took, where they were, and the one you were on.
    private func restore(_ r: ClosedLayout) {
        let live = Set(sessions.map(\.id))
        var t = tabs
        // A shell back in dinod may already have a tab at the end (every shell gets one): it goes
        // back where it was.
        for (id, at) in r.tabs where live.contains(id) {
            t.removeAll { $0 == id }
            t.insert(id, at: min(at, t.count))
        }
        knownTabless.subtract(r.tabs.map(\.id))
        if t != tabs { tabs = t }
        for s in r.splits where !splits.contains(s) && live.contains(s.first) && live.contains(s.second) {
            splits.removeAll { $0.contains(s.first) || $0.contains(s.second) }
            splits.append(s)
        }
        if let id = r.selected, live.contains(id) { select(id) }
    }
}
