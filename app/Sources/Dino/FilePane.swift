import AppKit
import PDFKit
import SwiftUI
import UniformTypeIdentifiers

/// What sits beside the terminals: a file, the selected session's web preview, or its tasks.
enum SidePane: Equatable {
    case file(OpenFile)
    case preview
    case tasks

    static func == (a: SidePane, b: SidePane) -> Bool {
        switch (a, b) {
        case let (.file(x), .file(y)): x === y
        case (.preview, .preview): true
        case (.tasks, .tasks): true
        default: false
        }
    }
}

/// A file open in the file pane, watched for changes on disk (agents edit while you read).
@MainActor
final class OpenFile: ObservableObject {
    enum Kind { case text, image, pdf, unreadable(String) }
    enum Conflict: Equatable { case changed, deleted }

    let path: String
    /// The session it was opened from.
    let session: String?
    @Published private(set) var kind: Kind = .text
    /// The text as on disk when last loaded or saved.
    @Published private(set) var saved = ""
    /// Bumped when the text is replaced from disk, so the editor takes it.
    @Published private(set) var revision = 0
    @Published var dirty = false
    @Published var conflict: Conflict?
    /// "Updated from disk" for a moment after a quiet reload.
    @Published var reloadedNote = false
    /// A line to show; the token repeats a jump to the same line.
    @Published private(set) var jump: (line: Int, token: Int)?
    /// Reads the editor's text (set by the editor).
    var current: (() -> String)?

    private var encoding: String.Encoding = .utf8
    private var stamp: (Date?, Int?)
    private var watcher: Timer?

    static let maxTextBytes = 8 << 20

    init(path: String, line: Int?, session: String?) {
        self.path = path
        self.session = session
        load()
        if let line { show(line: line) }
        watcher = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.checkDisk() }
        }
    }

    func stop() {
        watcher?.invalidate()
        watcher = nil
    }

    var url: URL { URL(fileURLWithPath: path) }
    var name: String { (path as NSString).lastPathComponent }
    var isText: Bool { if case .text = kind { true } else { false } }

    func show(line: Int) {
        jump = (line, (jump?.token ?? 0) + 1)
    }

    private static func stamp(_ path: String) -> (Date?, Int?) {
        let a = try? FileManager.default.attributesOfItem(atPath: path)
        return (a?[.modificationDate] as? Date, (a?[.size] as? NSNumber)?.intValue)
    }

    private func load() {
        stamp = Self.stamp(path)
        let type = UTType(filenameExtension: url.pathExtension)
        if type?.conforms(to: .pdf) == true {
            kind = .pdf
        } else if type?.conforms(to: .image) == true, NSImage(contentsOf: url) != nil {
            kind = .image
        } else if (stamp.1 ?? 0) > Self.maxTextBytes {
            kind = .unreadable("Too large to edit here (\(ByteCountFormatter.string(fromByteCount: Int64(stamp.1 ?? 0), countStyle: .file)))")
        } else if let (text, enc) = Self.readText(path) {
            kind = .text
            saved = text
            encoding = enc
        } else {
            kind = .unreadable("Not a text file")
        }
        revision += 1
        dirty = false
        conflict = nil
    }

    private static func readText(_ path: String) -> (String, String.Encoding)? {
        guard let data = FileManager.default.contents(atPath: path) else { return nil }
        // A NUL early on means binary, whatever it decodes as.
        if data.prefix(8192).contains(0) { return nil }
        if let s = String(data: data, encoding: .utf8) { return (s, .utf8) }
        if let s = String(data: data, encoding: .isoLatin1) { return (s, .isoLatin1) }
        return nil
    }

    private func checkDisk() {
        let now = Self.stamp(path)
        guard now.0 != stamp.0 || now.1 != stamp.1 else { return }
        if now.0 == nil {
            if conflict != .deleted { conflict = .deleted }
            return
        }
        guard isText else {
            load()
            return
        }
        // Saved again with the same text (formatters, `touch`): nothing to do.
        if let (text, _) = Self.readText(path), text == saved {
            stamp = now
            if conflict == .deleted { conflict = nil }
            return
        }
        if dirty {
            if conflict != .changed { conflict = .changed }
        } else {
            reload()
            reloadedNote = true
            DispatchQueue.main.asyncAfter(deadline: .now() + 2.5) { [weak self] in self?.reloadedNote = false }
        }
    }

    /// Take the version on disk, dropping unsaved edits.
    func reload() {
        load()
    }

    /// Keep the edits; the next save replaces what's on disk.
    func keepMine() {
        stamp = Self.stamp(path)
        conflict = nil
    }

    /// Returns false (and says why) when it couldn't.
    @discardableResult
    func save() -> Bool {
        guard isText, let text = current?() else { return true }
        // A change on disk the user hasn't decided about: saving would silently drop it.
        guard conflict != .changed else {
            NSSound.beep()
            return false
        }
        do {
            try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            try text.write(to: url, atomically: true, encoding: encoding)
            saved = text
            dirty = false
            conflict = nil
            stamp = Self.stamp(path)
            return true
        } catch {
            let alert = NSAlert(error: error)
            alert.messageText = "Couldn't save \(name)"
            alert.runModal()
            return false
        }
    }

    /// Before the file goes away: offers to save unsaved edits. False means stay.
    func confirmClose() -> Bool {
        guard dirty else { return true }
        let alert = NSAlert()
        alert.messageText = "Save your changes to \(name)?"
        alert.informativeText = "Your changes will be lost if you don't save them."
        alert.addButton(withTitle: "Save")
        alert.addButton(withTitle: "Don't Save")
        alert.addButton(withTitle: "Cancel")
        switch alert.runModal() {
        case .alertFirstButtonReturn: return save()
        case .alertSecondButtonReturn: return true
        default: return false
        }
    }
}

extension DinoModel {
    var openFile: OpenFile? {
        if case let .file(f) = sidePane { f } else { nil }
    }

    /// Show `path` beside the terminals, at `line`. HTML shows rendered, in the preview, unless
    /// it's the source that's wanted (`source`, or a line to go to).
    func openFile(_ path: String, line: Int? = nil, session: String? = nil, source: Bool = false) {
        let session = session ?? selected
        let ext = (path as NSString).pathExtension.lowercased()
        if ext == "html" || ext == "htm", line == nil, !source {
            openPreview(session: session, url: URL(fileURLWithPath: path))
            return
        }
        if let f = openFile, f.path == path {
            if let line { f.show(line: line) }
            return
        }
        guard openFile?.confirmClose() ?? true else { return }
        openFile?.stop()
        sidePane = .file(OpenFile(path: path, line: line, session: session))
    }

    /// ⌘W and Esc. False when the user chose to keep an unsaved file open.
    @discardableResult
    func closeSidePane() -> Bool {
        if let f = openFile {
            guard f.confirmClose() else { return false }
            f.stop()
        }
        sidePane = nil
        if let id = selected { terminals[id]?.requestFocus() }
        return true
    }

    /// File → Open File…: starts in the selected session's folder.
    func chooseFile() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = false
        panel.directoryURL = selectedSession.flatMap { $0.host == nil ? $0.here : nil }.map { URL(fileURLWithPath: $0) } ?? folder
        guard panel.runModal() == .OK, let url = panel.url else { return }
        openFile(url.path)
    }
}

// MARK: - Views

/// The header every side pane shares: what it is, what it's about, its own buttons, and close.
struct SidePaneHeader<Icon: View, Trailing: View>: View {
    let title: String
    var subtitle: String?
    let closeHelp: String
    let close: () -> Void
    @ViewBuilder var icon: Icon
    @ViewBuilder var trailing: Trailing

    var body: some View {
        HStack(spacing: 8) {
            icon.frame(width: 18, height: 18).foregroundStyle(.secondary).accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 1) {
                Text(title).font(.headline).lineLimit(1)
                if let subtitle {
                    Text(subtitle).font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.head)
                }
            }
            .accessibilityElement(children: .combine)
            .accessibilityAddTraits(.isHeader)
            Spacer(minLength: 8)
            trailing
            Button(action: close) { Image(systemName: "xmark") }
                .buttonStyle(.borderless)
                .help(closeHelp)
                .accessibilityLabel("Close \(title)")
        }
        .padding(.horizontal, 12)
        .frame(height: 44)
        .background(Color(nsColor: .windowBackgroundColor))
    }
}

struct SidePaneView: View {
    @EnvironmentObject var model: DinoModel
    let pane: SidePane

    var body: some View {
        Group {
            switch pane {
            case let .file(f): FilePane(file: f)
            case .preview: PreviewPane(session: model.selectedSession)
            case .tasks: TasksPane(session: model.tasksSession)
            }
        }
        .onExitCommand { model.closeSidePane() }
    }
}

struct FilePane: View {
    @EnvironmentObject var model: DinoModel
    @ObservedObject var file: OpenFile

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            if let c = file.conflict {
                conflictBanner(c)
                Divider()
            }
            content.frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .background(Color(nsColor: .textBackgroundColor))
        .onDisappear { if model.openFile !== file { file.stop() } }
    }

    private var header: some View {
        SidePaneHeader(
            title: file.name,
            subtitle: shortPath((file.path as NSString).deletingLastPathComponent),
            closeHelp: "Close (⌘W or Esc)",
            close: { model.closeSidePane() }
        ) {
            Image(nsImage: NSWorkspace.shared.icon(forFile: file.path)).resizable()
        } trailing: {
            Group {
                if file.reloadedNote {
                    Text("Updated from disk").font(.caption).foregroundStyle(Brand.green).transition(.opacity)
                }
                if file.dirty {
                    Circle().fill(.secondary).frame(width: 7, height: 7).help("Unsaved changes (⌘S saves)")
                        .accessibilityLabel("Unsaved changes")
                    Button("Save") { file.save() }.controlSize(.small).help("Save (⌘S)")
                }
            }
            .animation(.default, value: file.reloadedNote)
            OpenInEditorButton(path: file.path, line: file.jump?.line)
        }
        .help(file.path)
    }

    private func conflictBanner(_ c: OpenFile.Conflict) -> some View {
        HStack(spacing: 8) {
            Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(SessionStatus.needsYou.color)
            Text(c == .changed ? "Changed on disk since you started editing." : "Deleted on disk.")
                .font(.callout).lineLimit(2)
            Spacer(minLength: 4)
            if c == .changed {
                Button("Reload") { file.reload() }.help("Load the version on disk and discard your edits")
                Button("Keep Mine") { file.keepMine() }.help("Keep your edits. Saving replaces the version on disk.")
            } else {
                Button("Close") { model.closeSidePane() }
                Button("Keep Mine") { file.keepMine() }.help("Keep the file open. Saving creates it again.")
            }
        }
        .controlSize(.small)
        .padding(.horizontal, 12)
        .padding(.vertical, 7)
        .background(SessionStatus.needsYou.color.opacity(0.12))
    }

    @ViewBuilder
    private var content: some View {
        switch file.kind {
        case .text:
            CodeEditor(file: file, onEscape: { model.closeSidePane() })
        case .image:
            ImagePreview(url: file.url, revision: file.revision)
        case .pdf:
            PDFPreview(url: file.url, revision: file.revision)
        case let .unreadable(why):
            VStack(spacing: 12) {
                Image(nsImage: NSWorkspace.shared.icon(forFile: file.path)).resizable().frame(width: 64, height: 64)
                Text(why).foregroundStyle(.secondary)
                Button("Open with Default App") { NSWorkspace.shared.open(file.url) }
            }
        }
    }
}

private struct ImagePreview: View {
    let url: URL
    let revision: Int

    var body: some View {
        if let image = NSImage(contentsOf: url) {
            VStack(spacing: 8) {
                Image(nsImage: image).resizable().interpolation(.high).aspectRatio(contentMode: .fit)
                    .frame(maxWidth: image.size.width, maxHeight: image.size.height)
                    .background(Checkerboard().opacity(0.5))
                    .shadow(color: .black.opacity(0.15), radius: 3)
                Text("\(Int(image.size.width)) × \(Int(image.size.height))").font(.caption).foregroundStyle(.secondary)
            }
            .padding(20)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .id(revision)
        }
    }
}

/// Shows through transparent images.
private struct Checkerboard: View {
    var body: some View {
        Canvas { ctx, size in
            let s: CGFloat = 8
            for y in stride(from: 0, to: size.height, by: s) {
                for x in stride(from: 0, to: size.width, by: s) where (Int(x / s) + Int(y / s)).isMultiple(of: 2) {
                    ctx.fill(Path(CGRect(x: x, y: y, width: s, height: s)), with: .color(.gray.opacity(0.3)))
                }
            }
        }
    }
}

private struct PDFPreview: NSViewRepresentable {
    let url: URL
    let revision: Int

    func makeNSView(context: Context) -> PDFView {
        let v = PDFView()
        v.autoScales = true
        v.backgroundColor = .textBackgroundColor
        v.delegate = context.coordinator
        v.document = PDFDocument(url: url)
        return v
    }

    func updateNSView(_ v: PDFView, context: Context) {
        if context.coordinator.revision != revision {
            context.coordinator.revision = revision
            v.document = PDFDocument(url: url)
        }
    }

    func makeCoordinator() -> Box { Box(revision: revision) }

    final class Box: NSObject, PDFViewDelegate {
        var revision: Int
        init(revision: Int) { self.revision = revision }

        /// A link in the document: as one in agent text, not straight to whatever app claims it.
        func pdfViewWillClick(onLink _: PDFView, with url: URL) {
            MainActor.assumeIsolated { LinkPolicy.openElsewhere(url) }
        }
    }
}

// MARK: - Editor

/// A plain monospaced editor with line numbers.
struct CodeEditor: NSViewRepresentable {
    @ObservedObject var file: OpenFile
    let onEscape: () -> Void

    func makeCoordinator() -> Coordinator { Coordinator(file: file) }

    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = true
        scroll.autohidesScrollers = true
        // The text view paints the page; the scroll view stays out of the way on macOS 26.
        scroll.drawsBackground = false
        scroll.automaticallyAdjustsContentInsets = false
        scroll.contentInsets = NSEdgeInsets()

        let text = EditorTextView(usingTextLayoutManager: false)
        text.onEscape = onEscape
        text.font = Self.font
        text.isRichText = false
        text.importsGraphics = false
        text.allowsUndo = true
        text.usesFindBar = true
        text.isIncrementalSearchingEnabled = true
        text.isAutomaticQuoteSubstitutionEnabled = false
        text.isAutomaticDashSubstitutionEnabled = false
        text.isAutomaticTextReplacementEnabled = false
        text.isAutomaticSpellingCorrectionEnabled = false
        text.isContinuousSpellCheckingEnabled = false
        text.isGrammarCheckingEnabled = false
        text.smartInsertDeleteEnabled = false
        text.textContainerInset = NSSize(width: 4, height: 6)
        text.drawsBackground = true
        text.backgroundColor = .textBackgroundColor
        text.textColor = .textColor
        // Code doesn't wrap: it scrolls sideways, and line numbers stay one per line.
        text.isHorizontallyResizable = true
        text.isVerticallyResizable = true
        text.autoresizingMask = []
        text.maxSize = NSSize(width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)
        text.textContainer?.widthTracksTextView = false
        text.textContainer?.containerSize = NSSize(width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)
        text.string = file.saved
        text.delegate = context.coordinator
        scroll.documentView = text

        let ruler = LineNumberRuler(textView: text)
        scroll.verticalRulerView = ruler
        scroll.hasVerticalRuler = true
        scroll.rulersVisible = true

        context.coordinator.text = text
        context.coordinator.ruler = ruler
        context.coordinator.revision = file.revision
        file.current = { [weak text] in text?.string ?? "" }
        DispatchQueue.main.async {
            text.window?.makeFirstResponder(text)
            context.coordinator.jumpIfNeeded()
        }
        return scroll
    }

    func updateNSView(_: NSScrollView, context: Context) {
        let c = context.coordinator
        c.text?.onEscape = onEscape
        if c.revision != file.revision, let text = c.text {
            // Replaced from disk: keep the reader's place.
            c.revision = file.revision
            let selection = text.selectedRange()
            let origin = text.enclosingScrollView?.contentView.bounds.origin
            text.string = file.saved
            text.undoManager?.removeAllActions()
            let n = (text.string as NSString).length
            text.setSelectedRange(NSRange(location: min(selection.location, n), length: 0))
            if let origin { text.scroll(origin) }
            c.ruler?.textChanged()
        }
        c.jumpIfNeeded()
    }

    static let font = NSFont.monospacedSystemFont(ofSize: 12, weight: .regular)

    @MainActor
    final class Coordinator: NSObject, NSTextViewDelegate {
        let file: OpenFile
        weak var text: EditorTextView?
        weak var ruler: LineNumberRuler?
        var revision = 0
        private var jumped = 0

        init(file: OpenFile) { self.file = file }

        func textDidChange(_: Notification) {
            guard let text else { return }
            let dirty = text.string != file.saved
            if dirty != file.dirty { file.dirty = dirty }
            ruler?.textChanged()
        }

        func jumpIfNeeded() {
            guard let jump = file.jump, jump.token != jumped, let text, text.window != nil else { return }
            jumped = jump.token
            let s = text.string as NSString
            var location = 0, line = 1
            while line < jump.line, location < s.length {
                let r = s.lineRange(for: NSRange(location: location, length: 0))
                location = NSMaxRange(r)
                line += 1
            }
            let range = s.lineRange(for: NSRange(location: min(location, s.length), length: 0))
            text.setSelectedRange(NSRange(location: range.location, length: 0))
            text.scrollRangeToVisible(range)
            // Centered, not at the edge, so the line has context.
            if let scroll = text.enclosingScrollView, let lm = text.layoutManager, let tc = text.textContainer {
                let glyphs = lm.glyphRange(forCharacterRange: range, actualCharacterRange: nil)
                let rect = lm.boundingRect(forGlyphRange: glyphs, in: tc)
                let y = max(0, rect.midY - scroll.contentView.bounds.height / 2)
                text.scroll(NSPoint(x: 0, y: y))
            }
            text.showFindIndicator(for: range.length > 0 ? NSRange(location: range.location, length: max(range.length - 1, 1)) : range)
        }
    }
}

final class EditorTextView: NSTextView {
    var onEscape: (() -> Void)?

    override func cancelOperation(_ sender: Any?) {
        if let onEscape { onEscape() } else { super.cancelOperation(sender) }
    }
}

/// Line numbers down the editor's left edge.
final class LineNumberRuler: NSRulerView {
    private weak var textView: NSTextView?
    /// Where each line starts, in UTF-16 offsets; rebuilt on edits.
    private var starts: [Int] = [0]

    init(textView: NSTextView) {
        self.textView = textView
        super.init(scrollView: textView.enclosingScrollView, orientation: .verticalRuler)
        clientView = textView
        // Views don't clip by default since macOS 14: the ruler's fill painted over the whole pane.
        clipsToBounds = true
        textChanged()
        let clip = textView.enclosingScrollView?.contentView
        clip?.postsBoundsChangedNotifications = true
        NotificationCenter.default.addObserver(self, selector: #selector(redraw), name: NSView.boundsDidChangeNotification, object: clip)
    }

    @available(*, unavailable)
    required init(coder _: NSCoder) { fatalError() }

    @objc private func redraw() { needsDisplay = true }

    func textChanged() {
        guard let textView else { return }
        let s = textView.string.utf16
        var starts = [0]
        var i = 0
        for c in s {
            i += 1
            if c == 10 { starts.append(i) }
        }
        self.starts = starts
        let digits = max(3, String(starts.count).count)
        let width = CGFloat(digits) * 7.5 + 16
        if abs(ruleThickness - width) > 0.5 { ruleThickness = width }
        needsDisplay = true
    }

    private func line(at offset: Int) -> Int {
        var lo = 0, hi = starts.count - 1
        while lo < hi {
            let mid = (lo + hi + 1) / 2
            if starts[mid] <= offset { lo = mid } else { hi = mid - 1 }
        }
        return lo
    }

    override func drawHashMarksAndLabels(in rect: NSRect) {
        guard let textView, let lm = textView.layoutManager, let tc = textView.textContainer else { return }
        NSColor.textBackgroundColor.setFill()
        rect.intersection(bounds).fill()
        let visible = textView.visibleRect
        let glyphs = lm.glyphRange(forBoundingRect: visible, in: tc)
        let chars = lm.characterRange(forGlyphRange: glyphs, actualGlyphRange: nil)
        let attrs: [NSAttributedString.Key: Any] = [
            .font: NSFont.monospacedDigitSystemFont(ofSize: 10.5, weight: .regular),
            .foregroundColor: NSColor.tertiaryLabelColor,
        ]
        let current: [NSAttributedString.Key: Any] = [
            .font: NSFont.monospacedDigitSystemFont(ofSize: 10.5, weight: .regular),
            .foregroundColor: NSColor.secondaryLabelColor,
        ]
        let caret = line(at: textView.selectedRange().location)
        let inset = textView.textContainerOrigin.y
        let length = (textView.string as NSString).length
        var index = line(at: chars.location)
        while index < starts.count, starts[index] <= NSMaxRange(chars) {
            let start = starts[index]
            let lineRect: NSRect
            if start >= length {
                // The empty last line after a final newline.
                lineRect = lm.extraLineFragmentRect
                if lineRect.height == 0 { break }
            } else {
                let g = lm.glyphIndexForCharacter(at: start)
                lineRect = lm.lineFragmentRect(forGlyphAt: g, effectiveRange: nil)
            }
            let y = convert(NSPoint(x: 0, y: lineRect.minY + inset), from: textView).y
            let label = NSAttributedString(string: "\(index + 1)", attributes: index == caret ? current : attrs)
            let size = label.size()
            label.draw(at: NSPoint(x: ruleThickness - size.width - 8, y: y + (lineRect.height - size.height) / 2))
            index += 1
        }
    }
}

// MARK: - External editors

struct OpenInEditorButton: View {
    let path: String
    let line: Int?
    @AppStorage(ExternalEditor.preferenceKey) private var chosen = ""

    var body: some View {
        let editors = ExternalEditor.installed
        let preferred = ExternalEditor.preferred(for: path)
        Menu {
            ForEach(editors) { e in
                Button(e.name) {
                    chosen = e.bundleID
                    e.open(path, line: line)
                }
            }
            if !editors.isEmpty { Divider() }
            Button("Open with Default App") { NSWorkspace.shared.open(URL(fileURLWithPath: path)) }
            Button("Reveal in Finder") { NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: path)]) }
            Button("Copy Path") {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(path, forType: .string)
            }
        } label: {
            Text(preferred.map { "Open in \($0.name)" } ?? "Open")
        } primaryAction: {
            if let preferred { preferred.open(path, line: line) } else { NSWorkspace.shared.open(URL(fileURLWithPath: path)) }
        }
        .menuStyle(.borderedButton)
        .controlSize(.small)
        .fixedSize()
        .id(chosen)
        .help(preferred.map { "Open in \($0.name). Use the arrow to choose another editor." } ?? "Open with the default app")
    }
}

/// ⌘S: saves the file pane's file.
struct SaveCommand: View {
    @EnvironmentObject var model: DinoModel

    var body: some View {
        if let f = model.openFile {
            SaveButton(file: f)
        } else {
            Button("Save") {}.keyboardShortcut("s").disabled(true)
        }
    }

    private struct SaveButton: View {
        @ObservedObject var file: OpenFile
        var body: some View {
            Button("Save") { file.save() }.keyboardShortcut("s").disabled(!file.dirty)
        }
    }
}
