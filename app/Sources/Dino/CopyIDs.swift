import AppKit
import SwiftUI

/// What can be copied to name a session elsewhere, each for what takes it: dino's id (`dino
/// attach`, `dino kill`, `dino resume`), the agent's own conversation id (to resume or look it
/// up), and the name the agent's own messaging reaches it by (Claude Code's `SendMessage`).
/// Only what's known is offered.
struct SessionCopy: Equatable {
    let title: String
    let value: String
    let help: String

    static func of(_ s: SessionInfo) -> [SessionCopy] {
        var out = [SessionCopy(title: "Copy Session ID", value: s.id,
                               help: "dino's id for it: dino attach, dino kill and dino resume take it")]
        if let c = conversation(of: s) {
            let agent = AgentNames.of(s.agent)
            let resume = switch s.agent.replacingOccurrences(of: "-free", with: "") {
            case "claude": "; claude --resume takes it"
            case "codex": "; codex resume takes it, and codex queue --thread to message it"
            default: ""
            }
            out.append(SessionCopy(title: "Copy Conversation ID", value: c, help: "\(agent)'s own id for the conversation\(resume)"))
        }
        if let name = s.peer_name.flatMap(nonEmpty) {
            out.append(SessionCopy(title: "Copy Agent Name", value: name,
                                   help: "The name your other \(AgentNames.of(s.agent)) sessions message it by: tell an agent to message “\(name)”"))
        }
        return out
    }

    /// The agent's conversation: its own session's, or the one typed into the shell.
    static func conversation(of s: SessionInfo) -> String? {
        s.conversation.flatMap(nonEmpty) ?? s.inside.map(\.session_id).flatMap(nonEmpty)
    }

    private static func nonEmpty(_ v: String) -> String? {
        let t = v.trimmingCharacters(in: .whitespacesAndNewlines)
        return t.isEmpty ? nil : t
    }

    /// Puts it on `pasteboard` as plain text, in place of what was there.
    func copy(to pasteboard: NSPasteboard = .general) {
        pasteboard.clearContents()
        pasteboard.setString(value, forType: .string)
    }
}

/// A session's Copy items: in its sidebar and tab menus, and in the Session menu (so ⇧⌘P has them).
struct CopyItems: View {
    let session: SessionInfo

    var body: some View {
        ForEach(SessionCopy.of(session), id: \.title) { item in
            Button(item.title) { item.copy() }
                .help(item.help)
        }
    }
}
