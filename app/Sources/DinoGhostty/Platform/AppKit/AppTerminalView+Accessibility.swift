//
//  AppTerminalView+Accessibility.swift
//  dino: the terminal's text for VoiceOver, as Ghostty 1.2+ gives it (SurfaceView_AppKit).
//

#if !canImport(UIKit) && canImport(AppKit)
    import AppKit

    extension AppTerminalView {
        /// The terminal's text, read at most every half second: VoiceOver asks for it piece by
        /// piece, many times in a row. Nothing is read until something asks.
        struct AccessibilityTextCache {
            var text = ""
            var read: TimeInterval = -.infinity
        }

        /// All of the terminal's text, scrollback included, as Ghostty's app gives VoiceOver.
        var accessibilityContents: String {
            let now = ProcessInfo.processInfo.systemUptime
            if now - accessibilityText.read > 0.5 {
                accessibilityText = AccessibilityTextCache(text: surface?.readScreenText() ?? "", read: now)
            }
            return accessibilityText.text
        }

        override open func isAccessibilityElement() -> Bool {
            true
        }

        /// A text area: the terminal is text the user reads and types into.
        override open func accessibilityRole() -> NSAccessibility.Role? {
            .textArea
        }

        override open func accessibilityHelp() -> String? {
            "Terminal content area"
        }

        override open func accessibilityValue() -> Any? {
            accessibilityContents
        }

        // Ranges and lengths in UTF-16 units, as NSRange counts them.

        override open func accessibilityNumberOfCharacters() -> Int {
            (accessibilityContents as NSString).length
        }

        override open func accessibilityVisibleCharacterRange() -> NSRange {
            NSRange(location: 0, length: accessibilityNumberOfCharacters())
        }

        /// The selection, where it is in the text: where Ghostty says it starts, as long as the
        /// selected text, kept inside the text.
        override open func accessibilitySelectedTextRange() -> NSRange {
            guard let s = surface?.readSelectionResult(), !s.text.isEmpty else { return NSRange(location: 0, length: 0) }
            let total = accessibilityNumberOfCharacters()
            let length = min((s.text as NSString).length, total)
            return NSRange(location: min(Int(s.offsetStart), total - length), length: length)
        }

        override open func accessibilitySelectedText() -> String? {
            guard let text = surface?.readSelection(), !text.isEmpty else { return nil }
            return text
        }

        /// The line `index` is on, counting from 0 at the top of the scrollback.
        override open func accessibilityLine(for index: Int) -> Int {
            let text = accessibilityContents as NSString
            let end = min(max(index, 0), text.length)
            var lines = 0
            var at = 0
            while at < end {
                let found = text.range(of: "\n", options: .literal, range: NSRange(location: at, length: end - at))
                guard found.location != NSNotFound else { break }
                lines += 1
                at = NSMaxRange(found)
            }
            return lines
        }

        /// The range of line `line`, its line break included.
        override open func accessibilityRange(forLine line: Int) -> NSRange {
            let text = accessibilityContents as NSString
            var n = 0
            var found = NSRange(location: NSNotFound, length: 0)
            text.enumerateSubstrings(in: NSRange(location: 0, length: text.length), options: [.byLines, .substringNotRequired]) { _, _, enclosing, stop in
                if n == line {
                    found = enclosing
                    stop.pointee = true
                }
                n += 1
            }
            return found
        }

        override open func accessibilityString(for range: NSRange) -> String? {
            let text = accessibilityContents as NSString
            guard range.location != NSNotFound, NSMaxRange(range) <= text.length else { return nil }
            return text.substring(with: range)
        }

        /// The text in the terminal's font (Ghostty gives no more of its styling yet).
        override open func accessibilityAttributedString(for range: NSRange) -> NSAttributedString? {
            guard let plain = accessibilityString(for: range) else { return nil }
            var attributes: [NSAttributedString.Key: Any] = [:]
            if let font = surface?.quicklookFont() { attributes[.font] = font }
            return NSAttributedString(string: plain, attributes: attributes)
        }
    }
#endif
