import AppKit

/// "Install Command Line Tool…": a link to the `dino` inside the app, in a folder on the PATH.
/// ~/.local/bin needs no password; /usr/local/bin is offered only when it's writable already.
/// Nothing is replaced without asking, and nothing runs as root.
enum CommandLineTool {
    static func install() {
        guard let source = DinoEnvironment.bundledDino else { return }
        let fm = FileManager.default
        let home = NSString(string: "~/.local/bin").expandingTildeInPath
        let shared = "/usr/local/bin"
        let sharedWritable = fm.isWritableFile(atPath: shared)

        let ask = NSAlert()
        ask.messageText = "Install the dino command?"
        ask.informativeText = "This links \(home)/dino to the dino command inside this app, so it stays up to date with the app. No password is needed."
            + (sharedWritable ? "\n\nYou can also install the dino command in \(shared)." : "")
        ask.addButton(withTitle: "Install in ~/.local/bin")
        if sharedWritable { ask.addButton(withTitle: "Install in /usr/local/bin") }
        ask.addButton(withTitle: "Cancel")
        let dir: String
        switch ask.runModal() {
        case .alertFirstButtonReturn: dir = home
        case .alertSecondButtonReturn where sharedWritable: dir = shared
        default: return
        }
        let target = "\(dir)/dino"

        if let existing = try? fm.destinationOfSymbolicLink(atPath: target), existing == source {
            tell("The dino command is already installed", "\(target) points at this app.")
            return
        }
        let present = (try? fm.attributesOfItem(atPath: target)) != nil
        if present {
            let replace = NSAlert()
            replace.messageText = "Replace \(target)?"
            replace.informativeText = "Something already exists there. Replacing it makes it a link to the dino command inside this app."
            replace.addButton(withTitle: "Replace")
            replace.addButton(withTitle: "Cancel")
            guard replace.runModal() == .alertFirstButtonReturn else { return }
        }
        do {
            try fm.createDirectory(atPath: dir, withIntermediateDirectories: true)
            if present { try fm.removeItem(atPath: target) }
            try fm.createSymbolicLink(atPath: target, withDestinationPath: source)
        } catch {
            tell("Couldn't install the dino command", error.localizedDescription)
            return
        }
        let onPath = DinoEnvironment.loginPath.split(separator: ":").contains { $0 == dir }
        tell("Installed \(target)",
             onPath ? "Open a new shell and run dino." : "\(dir) isn't on your PATH yet. Add this to your shell's startup file:\n\nexport PATH=\"\(dir):$PATH\"")
    }

    private static func tell(_ title: String, _ text: String) {
        let a = NSAlert()
        a.messageText = title
        a.informativeText = text
        a.runModal()
    }
}
