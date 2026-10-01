import AppKit
import GhosttyTerminal
import UniformTypeIdentifiers

/// What a drop on a session's terminal becomes: text pasted through the terminal's paste path,
/// as Ghostty's own macOS app does it. Files are their shell-escaped paths; an image with no file
/// behind it (dragged out of a browser, a screenshot thumbnail) is written to a file first. An
/// agent sees a pasted image path as an attachment, the same as a drop on Terminal or Ghostty.
@MainActor
enum TerminalDrop {
    static let types: [NSPasteboard.PasteboardType] =
        [.fileURL, .URL, .string, .png, .tiff, NSPasteboard.PasteboardType(UTType.jpeg.identifier)]
        + NSFilePromiseReceiver.readableDraggedTypes.map { NSPasteboard.PasteboardType($0) }

    /// Image data dino writes to a file, best first.
    private static let images: [(NSPasteboard.PasteboardType, UTType)] = [
        (.png, .png),
        (NSPasteboard.PasteboardType(UTType.jpeg.identifier), .jpeg),
        (NSPasteboard.PasteboardType(UTType.heic.identifier), .heic),
        (NSPasteboard.PasteboardType(UTType.gif.identifier), .gif),
        (.tiff, .png),
    ]

    static func accepts(_ pasteboard: NSPasteboard) -> Bool {
        pasteboard.availableType(from: types) != nil
    }

    /// Reads the drop and hands the text to paste to `paste`, later when files have to be
    /// written first; nothing when the drop held nothing usable.
    static func text(from pasteboard: NSPasteboard, paste: @escaping @MainActor (String) -> Void) {
        let files = pasteboard.readObjects(forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]) as? [URL] ?? []
        if !files.isEmpty {
            paste(files.map { escape($0.path) }.joined(separator: " "))
            return
        }
        if let (data, type) = image(on: pasteboard), let path = store(data, name: "image", type: type) {
            paste(escape(path))
            return
        }
        if let promises = pasteboard.readObjects(forClasses: [NSFilePromiseReceiver.self]) as? [NSFilePromiseReceiver], !promises.isEmpty {
            receive(promises, paste: paste)
            return
        }
        let urls = pasteboard.readObjects(forClasses: [NSURL.self]) as? [URL] ?? []
        if !urls.isEmpty {
            paste(urls.map(\.absoluteString).joined(separator: " "))
            return
        }
        if let s = pasteboard.string(forType: .string), !s.isEmpty {
            paste(s)
        }
    }

    /// ⌘V with only an image on the clipboard (a screenshot copied with ⌃⇧⌘4, an image copied in a
    /// browser): the terminal's own paste reads text only, so the image is written to a file and
    /// its path pasted, which an agent attaches. Nil when there's text, a file or a URL to paste
    /// the usual way.
    static func clipboardImage(_ pasteboard: NSPasteboard = .general) -> String? {
        guard pasteboard.string(forType: .string)?.isEmpty ?? true,
              pasteboard.availableType(from: [.fileURL, .URL]) == nil,
              let (data, type) = image(on: pasteboard),
              let path = store(data, name: "pasted", type: type) else { return nil }
        return escape(path)
    }

    private static func image(on pasteboard: NSPasteboard) -> (Data, UTType)? {
        for (pbType, type) in images {
            guard let data = pasteboard.data(forType: pbType) else { continue }
            // TIFF is how AppKit carries an image around, not a file anyone wants.
            if pbType == .tiff {
                guard let png = NSBitmapImageRep(data: data)?.representation(using: .png, properties: [:]) else { continue }
                return (png, .png)
            }
            return (data, type)
        }
        return nil
    }

    /// Files another app hands over only on request (Photos, Mail attachments, some browsers).
    private static func receive(_ promises: [NSFilePromiseReceiver], paste: @escaping @MainActor (String) -> Void) {
        guard let dir = directory() else { return }
        let group = DispatchGroup()
        let lock = NSLock()
        var paths: [Int: String] = [:]
        for (i, promise) in promises.enumerated() {
            group.enter()
            promise.receivePromisedFiles(atDestination: dir, options: [:], operationQueue: .main) { url, error in
                if error == nil {
                    lock.lock()
                    paths[i] = url.path
                    lock.unlock()
                }
                group.leave()
            }
        }
        group.notify(queue: .main) {
            let text = paths.keys.sorted().compactMap { paths[$0] }.map(escape).joined(separator: " ")
            if !text.isEmpty {
                MainActor.assumeIsolated { paste(text) }
            }
        }
    }

    /// Where dropped data goes: the terminal package's staging folder, which it sweeps of files
    /// older than a day.
    private static func directory() -> URL? {
        let dir = TerminalFileStaging.directory
        TerminalFileStaging.removeStaleFiles()
        do {
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            return dir
        } catch {
            return nil
        }
    }

    private static func store(_ data: Data, name: String, type: UTType) -> String? {
        guard let dir = directory() else { return nil }
        let stamp = Int(Date().timeIntervalSince1970)
        let ext = type.preferredFilenameExtension ?? "bin"
        var url = dir.appendingPathComponent("\(name)-\(stamp).\(ext)")
        var n = 1
        while FileManager.default.fileExists(atPath: url.path) {
            url = dir.appendingPathComponent("\(name)-\(stamp)-\(n).\(ext)")
            n += 1
        }
        do {
            try data.write(to: url, options: .atomic)
            return url.path
        } catch {
            return nil
        }
    }

    /// Backslashes before what a shell would read as syntax: the form a path takes typed at a
    /// prompt. The same set as Ghostty's `Shell.escape`. A name with a newline or other control
    /// character in it is ANSI-C quoted (`$'a\nb'`) instead: pasted as is, a newline would run
    /// what follows it in a shell without bracketed paste.
    static func escape(_ s: String) -> String {
        if s.unicodeScalars.contains(where: { $0.properties.generalCategory == .control }) {
            var out = "$'"
            for u in s.unicodeScalars {
                switch u {
                case "\n": out += "\\n"
                case "\r": out += "\\r"
                case "\t": out += "\\t"
                case "\\": out += "\\\\"
                case "'": out += "\\'"
                default:
                    if u.properties.generalCategory == .control {
                        for b in String(u).utf8 { out += String(format: "\\x%02X", b) }
                    } else {
                        out.unicodeScalars.append(u)
                    }
                }
            }
            return out + "'"
        }
        let special: Set<Character> = ["\\", " ", "(", ")", "[", "]", "{", "}", "<", ">", "\"", "'", "`", "!", "#", "$", "&", ";", "|", "*", "?", "\t"]
        var out = ""
        for c in s {
            if special.contains(c) { out.append("\\") }
            out.append(c)
        }
        return out
    }
}

/// The outline a terminal shows while something is dragged over it.
final class DropHighlight: NSView {
    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        layer?.borderWidth = 2
        layer?.cornerRadius = 4
        layer?.borderColor = NSColor(Brand.green).cgColor
        layer?.backgroundColor = NSColor(Brand.green).withAlphaComponent(0.12).cgColor
        autoresizingMask = [.width, .height]
        isHidden = true
    }

    @available(*, unavailable)
    required init?(coder _: NSCoder) { fatalError() }

    /// Clicks and drags go to the terminal underneath.
    override func hitTest(_: NSPoint) -> NSView? { nil }
}
