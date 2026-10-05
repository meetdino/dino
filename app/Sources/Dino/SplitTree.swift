import CoreGraphics
import Foundation

/// A tab's panes, as Ghostty's `SplitTree`: nested splits, side by side or stacked, any number of
/// panes. Each pane is a session, which keeps running in dinod whatever happens to the layout.
/// Pure values, so the layout outlives the app (see `saved()`).
struct SplitTree: Equatable {
    indirect enum Node: Equatable {
        case pane(String)
        case split(Branch)
    }

    struct Branch: Equatable, Codable {
        /// Stacked (Ghostty's `vertical`): `first` above `second`. Else `first` is on the left.
        var vertical: Bool
        /// How much of the branch `first` takes, 0…1.
        var ratio: Double
        var first: Node
        var second: Node
    }

    /// Where a node is: each step goes to a branch's `first` (false) or `second` (true).
    typealias Path = [Bool]

    /// Ghostty's `new_split` directions.
    enum NewDirection { case right, down, left, up }

    /// Ghostty's spatial `goto_split` and `resize_split` directions.
    enum Spatial { case left, right, up, down }

    /// Ghostty's `goto_split`.
    enum Focus: Equatable { case previous, next, spatial(Spatial) }

    var root: Node
    /// The pane taking the whole tab for now (Ghostty's `toggle_split_zoom`).
    var zoomed: String?
    /// Shells dino started for this split (⌘D): each goes when its pane closes.
    var helpers: [String] = []

    init(root: Node, zoomed: String? = nil, helpers: [String] = []) {
        self.root = root
        self.zoomed = zoomed
        self.helpers = helpers
    }

    /// Two panes: `first` left of `second`, or above it.
    init(_ first: String, _ second: String, vertical: Bool, ratio: Double = 0.5, helpers: [String] = []) {
        self.init(root: .split(Branch(vertical: vertical, ratio: ratio, first: .pane(first), second: .pane(second))), helpers: helpers)
    }

    /// The panes, left to right and top to bottom (Ghostty's leaf order).
    var panes: [String] { root.panes }

    func contains(_ id: String?) -> Bool {
        guard let id else { return false }
        return root.path(to: id) != nil
    }

    /// The panes on screen: just the zoomed one while one is.
    var shownPanes: [String] {
        if let zoomed, contains(zoomed) { return [zoomed] }
        return panes
    }

    func isHelper(_ id: String) -> Bool { helpers.contains(id) }

    // MARK: Changing it

    /// Pane `new` next to pane `at`, half of `at`'s space (Ghostty's `new_split`). Unzooms.
    func inserting(_ new: String, at: String, _ direction: NewDirection) -> SplitTree? {
        guard let path = root.path(to: at) else { return nil }
        let vertical = direction == .down || direction == .up
        let before = direction == .left || direction == .up
        let branch = Branch(vertical: vertical, ratio: 0.5, first: .pane(before ? new : at), second: .pane(before ? at : new))
        return SplitTree(root: root.replacing(at: path, with: .split(branch)), zoomed: nil, helpers: helpers)
    }

    /// Without pane `id`; nil once one pane or none is left (it's no split then).
    func removing(_ id: String) -> SplitTree? {
        guard contains(id), let rest = root.removing(id), case .split = rest else { return nil }
        return SplitTree(root: rest, zoomed: zoomed == id ? nil : zoomed, helpers: helpers.filter { $0 != id })
    }

    /// Only the panes `keep` says; nil once fewer than two are left.
    func pruned(_ keep: (String) -> Bool) -> SplitTree? {
        var t: SplitTree? = self
        for id in panes where !keep(id) { t = t?.removing(id) }
        return t
    }

    /// Every branch at its share again (Ghostty's `equalize_splits`): each side by how many panes
    /// it has across that branch's direction.
    func equalized() -> SplitTree {
        var t = self
        t.root = root.equalized()
        return t
    }

    /// The branch at `path` with `first` taking `ratio` of it (dragging its divider).
    func setting(ratio: Double, at path: Path) -> SplitTree? {
        guard case .split(var b)? = root.node(at: path) else { return nil }
        b.ratio = ratio
        var t = self
        t.root = root.replacing(at: path, with: .split(b))
        return t
    }

    /// Pane `id`'s own branch turned: side by side becomes stacked and back.
    func turning(_ id: String) -> SplitTree? {
        guard let path = root.path(to: id), !path.isEmpty,
              case .split(var b)? = root.node(at: Array(path.dropLast())) else { return nil }
        b.vertical.toggle()
        var t = self
        t.root = root.replacing(at: Array(path.dropLast()), with: .split(b))
        return t
    }

    /// Whether pane `id`'s own branch is stacked.
    func stacked(_ id: String) -> Bool? {
        guard let path = root.path(to: id), !path.isEmpty, case .split(let b)? = root.node(at: Array(path.dropLast())) else { return nil }
        return b.vertical
    }

    /// Pane `id` zoomed, or unzoomed if it is (Ghostty's `toggle_split_zoom`).
    func togglingZoom(_ id: String) -> SplitTree {
        var t = self
        t.zoomed = zoomed == id ? nil : id
        return t
    }

    /// Ghostty's `resize_split`: the divider of the nearest branch around pane `id` that runs the
    /// right way moves `points` towards `direction`, in a tab of `size`. Unzooms. Nil when no
    /// branch runs that way.
    func resizing(_ id: String, by points: Double, _ direction: Spatial, in size: CGSize) -> SplitTree? {
        guard let path = root.path(to: id) else { return nil }
        let vertical = direction == .up || direction == .down
        // The innermost branch around the pane split the right way.
        var found: (path: Path, branch: Branch)?
        for i in stride(from: path.count - 1, through: 0, by: -1) {
            let p = Array(path.prefix(i))
            if case .split(let b)? = root.node(at: p), b.vertical == vertical {
                found = (p, b)
                break
            }
        }
        guard let (bp, b) = found,
              let slot = root.spatialSlots(in: CGRect(origin: .zero, size: size)).first(where: { $0.path == bp })
        else { return nil }
        let length = vertical ? slot.bounds.height : slot.bounds.width
        guard length > 0 else { return nil }
        let sign: Double = direction == .right || direction == .down ? 1 : -1
        var next = b
        next.ratio = min(max(b.ratio + sign * points / length, 0.1), 0.9)
        var t = self
        t.root = root.replacing(at: bp, with: .split(next))
        t.zoomed = nil
        return t
    }

    // MARK: Moving between panes

    /// The pane Ghostty's `goto_split` goes to from pane `id`; nil when there's none that way.
    func focusTarget(_ focus: Focus, from id: String) -> String? {
        let all = panes
        guard let at = all.firstIndex(of: id) else { return nil }
        switch focus {
        case .previous:
            guard all.count > 1 else { return nil }
            return all[(at - 1 + all.count) % all.count]
        case .next:
            guard all.count > 1 else { return nil }
            return all[(at + 1) % all.count]
        case .spatial(let direction):
            // As Ghostty: laid out by pane counts rather than points, the slots (branches too)
            // wholly on that side, nearest first by their top-left corners; a pane among them
            // first, else the nearer edge of the nearest branch.
            let slots = root.spatial()
            guard let ref = slots.first(where: { $0.node == .pane(id) })?.bounds else { return nil }
            let e = 1e-9
            let side = slots.filter { slot in
                guard slot.node != .pane(id) else { return false }
                let b = slot.bounds
                return switch direction {
                case .left: b.maxX <= ref.minX + e
                case .right: b.minX >= ref.maxX - e
                case .up: b.maxY <= ref.minY + e
                case .down: b.minY >= ref.maxY - e
                }
            }
            func distance(_ b: CGRect) -> Double {
                let dx = b.minX - ref.minX, dy = b.minY - ref.minY
                return (dx * dx + dy * dy).squareRoot()
            }
            let sorted = side.sorted { distance($0.bounds) < distance($1.bounds) }
            guard let best = sorted.first(where: { if case .pane = $0.node { true } else { false } }) ?? sorted.first else { return nil }
            switch best.node {
            case .pane(let p): return p
            case .split: return direction == .up || direction == .left ? best.node.leftmost : best.node.rightmost
            }
        }
    }

    /// Where focus goes when pane `id` closes, as in Ghostty: the pane before it, or after it for
    /// the first one.
    func afterClosing(_ id: String) -> String? {
        panes.first == id ? focusTarget(.next, from: id) : focusTarget(.previous, from: id)
    }

    // MARK: Layout

    /// A divider between a branch's two sides: where it is, and the branch it divides.
    struct Divider: Equatable {
        var path: Path
        var vertical: Bool
        var ratio: Double
        /// The whole branch, for dragging.
        var branch: CGRect
        /// The line itself, `gap` wide.
        var line: CGRect

        /// The ratio putting the line at `point` (dragging it), each side kept `least` points or
        /// more where there's room.
        func ratio(at point: CGPoint, least: CGFloat, gap: CGFloat) -> Double? {
            let length = (vertical ? branch.height : branch.width) - gap
            guard length > 0 else { return nil }
            let at = vertical ? point.y - branch.minY : point.x - branch.minX
            let least = min(least, length / 2)
            return Double(min(max(at, least), length - least) / length)
        }
    }

    /// Each shown pane's rect in a tab of `size`, `gap` points between panes, and the dividers. A
    /// zoomed pane takes it all, with no dividers.
    func layout(in size: CGSize, gap: CGFloat) -> (panes: [String: CGRect], dividers: [Divider]) {
        let all = CGRect(origin: .zero, size: size)
        if let zoomed, contains(zoomed) { return ([zoomed: all], []) }
        var panes: [String: CGRect] = [:]
        var dividers: [Divider] = []
        func place(_ node: Node, _ r: CGRect, _ path: Path) {
            switch node {
            case .pane(let id):
                panes[id] = r
            case .split(let b):
                let (a, c, line) = Self.cut(r, b, gap: gap)
                dividers.append(Divider(path: path, vertical: b.vertical, ratio: b.ratio, branch: r, line: line))
                place(b.first, a, path + [false])
                place(b.second, c, path + [true])
            }
        }
        place(root, all, [])
        return (panes, dividers)
    }

    /// A branch's two sides in `r` and the line between them, on whole points.
    static func cut(_ r: CGRect, _ b: Branch, gap: CGFloat) -> (CGRect, CGRect, CGRect) {
        if b.vertical {
            let h = max(((r.height - gap) * b.ratio).rounded(), 0)
            return (CGRect(x: r.minX, y: r.minY, width: r.width, height: h),
                    CGRect(x: r.minX, y: r.minY + h + gap, width: r.width, height: max(r.height - h - gap, 0)),
                    CGRect(x: r.minX, y: r.minY + h, width: r.width, height: gap))
        }
        let w = max(((r.width - gap) * b.ratio).rounded(), 0)
        return (CGRect(x: r.minX, y: r.minY, width: w, height: r.height),
                CGRect(x: r.minX + w + gap, y: r.minY, width: max(r.width - w - gap, 0), height: r.height),
                CGRect(x: r.minX + w, y: r.minY, width: gap, height: r.height))
    }
}

// MARK: - Nodes

extension SplitTree.Node {
    typealias Path = SplitTree.Path

    var panes: [String] {
        switch self {
        case .pane(let id): [id]
        case .split(let b): b.first.panes + b.second.panes
        }
    }

    var leftmost: String {
        switch self {
        case .pane(let id): id
        case .split(let b): b.first.leftmost
        }
    }

    var rightmost: String {
        switch self {
        case .pane(let id): id
        case .split(let b): b.second.rightmost
        }
    }

    func path(to id: String) -> Path? {
        switch self {
        case .pane(let p): return p == id ? [] : nil
        case .split(let b):
            if let p = b.first.path(to: id) { return [false] + p }
            if let p = b.second.path(to: id) { return [true] + p }
            return nil
        }
    }

    func node(at path: Path) -> Self? {
        guard let step = path.first else { return self }
        guard case .split(let b) = self else { return nil }
        return (step ? b.second : b.first).node(at: Array(path.dropFirst()))
    }

    func replacing(at path: Path, with node: Self) -> Self {
        guard let step = path.first else { return node }
        guard case .split(var b) = self else { return self }
        let rest = Array(path.dropFirst())
        if step { b.second = b.second.replacing(at: rest, with: node) } else { b.first = b.first.replacing(at: rest, with: node) }
        return .split(b)
    }

    /// Without pane `id`: its branch gives way to the other side.
    func removing(_ id: String) -> Self? {
        switch self {
        case .pane(let p): return p == id ? nil : self
        case .split(var b):
            let first = b.first.removing(id), second = b.second.removing(id)
            guard let first else { return second }
            guard let second else { return first }
            b.first = first
            b.second = second
            return .split(b)
        }
    }

    func equalized() -> Self {
        guard case .split(var b) = self else { return self }
        let l = b.first.weight(b.vertical), r = b.second.weight(b.vertical)
        b.ratio = Double(l) / Double(l + r)
        b.first = b.first.equalized()
        b.second = b.second.equalized()
        return .split(b)
    }

    /// How many panes it has across a branch running `vertical`: one, unless it runs that way too.
    private func weight(_ vertical: Bool) -> Int {
        guard case .split(let b) = self, b.vertical == vertical else { return 1 }
        return b.first.weight(vertical) + b.second.weight(vertical)
    }

    /// Ghostty's grid for moving between panes: each pane 1×1, side by side adding widths.
    private func dimensions() -> (w: Int, h: Int) {
        guard case .split(let b) = self else { return (1, 1) }
        let l = b.first.dimensions(), r = b.second.dimensions()
        return b.vertical ? (max(l.w, r.w), l.h + r.h) : (l.w + r.w, max(l.h, r.h))
    }

    struct Slot {
        var node: SplitTree.Node
        var path: Path
        var bounds: CGRect
    }

    func spatial() -> [Slot] {
        let d = dimensions()
        return spatialSlots(in: CGRect(x: 0, y: 0, width: d.w, height: d.h))
    }

    /// Every node's bounds in `r`, branches before their sides (Ghostty's `spatialSlots`).
    func spatialSlots(in r: CGRect, at path: Path = []) -> [Slot] {
        guard case .split(let b) = self else { return [Slot(node: self, path: path, bounds: r)] }
        let a: CGRect, c: CGRect
        if b.vertical {
            a = CGRect(x: r.minX, y: r.minY, width: r.width, height: r.height * b.ratio)
            c = CGRect(x: r.minX, y: r.minY + r.height * b.ratio, width: r.width, height: r.height * (1 - b.ratio))
        } else {
            a = CGRect(x: r.minX, y: r.minY, width: r.width * b.ratio, height: r.height)
            c = CGRect(x: r.minX + r.width * b.ratio, y: r.minY, width: r.width * (1 - b.ratio), height: r.height)
        }
        return [Slot(node: self, path: path, bounds: r)] + b.first.spatialSlots(in: a, at: path + [false])
            + b.second.spatialSlots(in: c, at: path + [true])
    }
}

// MARK: - Saving

extension SplitTree.Node: Codable {
    private enum Key: String, CodingKey { case pane, split }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Key.self)
        if let id = try c.decodeIfPresent(String.self, forKey: .pane) {
            self = .pane(id)
        } else {
            self = .split(try c.decode(SplitTree.Branch.self, forKey: .split))
        }
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: Key.self)
        switch self {
        case .pane(let id): try c.encode(id, forKey: .pane)
        case .split(let b): try c.encode(b, forKey: .split)
        }
    }
}

extension SplitTree: Codable {
    private enum Key: String, CodingKey { case root, zoomed, helpers }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Key.self)
        root = try c.decode(Node.self, forKey: .root)
        zoomed = try c.decodeIfPresent(String.self, forKey: .zoomed)
        helpers = try c.decodeIfPresent([String].self, forKey: .helpers) ?? []
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: Key.self)
        try c.encode(root, forKey: .root)
        try c.encodeIfPresent(zoomed, forKey: .zoomed)
        if !helpers.isEmpty { try c.encode(helpers, forKey: .helpers) }
    }

    /// A split as dino kept one before splits could hold more than two panes.
    private struct Pair: Decodable {
        var first: String
        var second: String
        var vertical: Bool?
        var fraction: Double?
        var helper: String?
    }

    private static let key = "splitTrees"
    private static let pairsKey = "splits"

    /// Splits outlive the app, like the sessions in them. The two-pane splits of an earlier dino
    /// come back as trees of two.
    static func saved(_ defaults: UserDefaults = .standard) -> [SplitTree] {
        if let data = defaults.data(forKey: key) {
            return (try? JSONDecoder().decode([SplitTree].self, from: data)) ?? []
        }
        guard let data = defaults.data(forKey: pairsKey),
              let pairs = try? JSONDecoder().decode([Pair].self, from: data) else { return [] }
        return pairs.map {
            SplitTree($0.first, $0.second, vertical: $0.vertical ?? false, ratio: $0.fraction ?? 0.5, helpers: $0.helper.map { [$0] } ?? [])
        }
    }

    static func save(_ trees: [SplitTree], _ defaults: UserDefaults = .standard) {
        defaults.set(try? JSONEncoder().encode(trees), forKey: key)
        defaults.removeObject(forKey: pairsKey)
    }
}
