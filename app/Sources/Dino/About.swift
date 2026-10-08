import AppKit

/// dino → About dino: the standard About window, with the third-party notices as its credits.
/// release.sh and build.sh put THIRD_PARTY_NOTICES.md (scripts/third-party.py) in
/// Contents/Resources; every license text dino ships is in it, so the credits scroll through all of
/// them.
enum About {
    static let notices = "THIRD_PARTY_NOTICES"
    /// As AppKit's own item says it: "About dino", "About dino dev".
    static let title = "About " + (Bundle.main.object(forInfoDictionaryKey: "CFBundleName") as? String ?? "dino")

    static func show() {
        var options: [NSApplication.AboutPanelOptionKey: Any] = [:]
        if let url = Bundle.main.url(forResource: notices, withExtension: "md"),
           let text = try? String(contentsOf: url, encoding: .utf8) {
            options[.credits] = credits(text)
        }
        NSApp.activate(ignoringOtherApps: true)
        NSApp.orderFrontStandardAboutPanel(options: options)
    }

    /// The notices as the credits show them: headings in bold, everything else as written (license
    /// texts keep their line breaks, and a # in one is just text), and the fences around them left out.
    static func credits(_ markdown: String) -> NSAttributedString {
        let body: [NSAttributedString.Key: Any] = [
            .font: NSFont.systemFont(ofSize: 10),
            .foregroundColor: NSColor.labelColor,
        ]
        let heading: [NSAttributedString.Key: Any] = [
            .font: NSFont.boldSystemFont(ofSize: 11),
            .foregroundColor: NSColor.labelColor,
        ]
        let out = NSMutableAttributedString()
        var inText = false
        for line in markdown.split(separator: "\n", omittingEmptySubsequences: false) {
            if line.hasPrefix("```") {
                inText.toggle()
                continue
            }
            if !inText, line.hasPrefix("#") {
                let title = line.drop(while: { $0 == "#" }).trimmingCharacters(in: .whitespaces)
                out.append(NSAttributedString(string: title + "\n", attributes: heading))
            } else {
                out.append(NSAttributedString(string: line + "\n", attributes: body))
            }
        }
        return out
    }
}
