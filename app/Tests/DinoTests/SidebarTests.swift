import AppKit
import Foundation
import XCTest
@testable import Dino

/// A session as dinod sends it, with what a test sets on top.
private func session(_ id: String, _ name: String, cwd: String = "/code/acme-api", _ extra: [String: Any] = [:]) -> SessionInfo {
    var json: [String: Any] = [
        "id": id, "name": name, "agent_id": "claude", "exited": false, "bells": 0, "requests": 0,
        "in_flight": 0, "input_tokens": 0, "output_tokens": 0, "cwd": cwd,
    ]
    json.merge(extra) { $1 }
    let data = try! JSONSerialization.data(withJSONObject: json)
    return try! JSONDecoder().decode(SessionInfo.self, from: data)
}

private func pr(_ number: UInt32, failed: UInt32 = 0, pending: UInt32 = 0, passed: UInt32 = 0, state: String = "open") -> PrInfo {
    PrInfo(number: number, url: "https://github.com/acme/infra/pull/\(number)", title: "Faster CI", state: state, draft: false,
           checks: Checks(passed: passed, failed: failed, pending: pending, failing: []), review: nil, head: nil)
}

final class RowFactsTests: XCTestCase {
    /// The design's tooltip: agent · model · sub-state, then branch and PR, then context and tokens.
    func testTooltipCarriesWhatTheRowLeavesOut() {
        var i = RowFacts.Input(name: "Faster CI, smaller image", status: .thinking, agent: "Claude Code", model: "sonnet-5-5")
        i.branch = "faster-ci"
        i.pr = pr(42, pending: 2)
        i.contextUsed = 104_000
        i.contextLimit = 200_000
        i.tokensIn = 1_200_000
        i.tokensOut = 38_000
        i.requests = 12
        i.splitWith = "Retry with backoff"
        let f = RowFacts(i)
        XCTAssertEqual(f.tooltip[0], "Claude Code · sonnet-5-5 · Thinking")
        XCTAssertEqual(f.tooltip[1], "Branch faster-ci · PR #42, checks running")
        XCTAssertEqual(f.tooltip[2], "In a split with Retry with backoff")
        XCTAssertEqual(f.tooltip[3], "Context 52% · ↑1.2M in · ↓38.0k out · 12 requests")
        XCTAssertTrue(f.help.hasSuffix("Double-click to rename"))
    }

    /// VoiceOver hears the name, the status word and everything the tooltip says.
    func testAccessibilityLabelSaysEverythingMoved() {
        var i = RowFacts.Input(name: "Cache npm in CI", status: .needsYou, agent: "Claude Code", model: "sonnet-5-5")
        i.needs = "Edit ci.yml?"
        i.branch = "cache-npm"
        i.pr = pr(7, failed: 1)
        i.auto = AutoPr(fix: true, merge: true, fixes: 0, note: nil)
        i.error = "overloaded"
        i.automation = "Fix CI on my PRs"
        i.forkedFrom = "Speed up CI"
        i.peers = ["Started by Docs's agent"]
        i.pinned = true
        i.fallback = "On Claude account 2"
        i.serving = nil
        i.project = "infra"
        let a = RowFacts(i).accessibility
        for part in ["Cache npm in CI", "Needs you", "Asks: Edit ci.yml?", "sonnet-5-5", "Claude Code", "In infra", "Branch cache-npm",
                     "PR #7, 1 failing", "auto fixes failing checks and merges when they pass", "Error: overloaded",
                     "Started by the automation “Fix CI on my PRs”", "Forked from “Speed up CI”", "Started by Docs's agent",
                     "Pinned", "On Claude account 2"] {
            XCTAssertTrue(a.contains(part), "missing \(part) in \(a)")
        }
        XCTAssertFalse(a.contains("Double-click"))
    }

    func testSubStates() {
        func state(_ s: SessionStatus, waiting: String? = nil) -> String {
            var i = RowFacts.Input(name: "x", status: s, agent: "Codex")
            i.waitingOn = waiting
            return RowFacts.state(i)
        }
        XCTAssertEqual(state(.thinking), "Thinking")
        XCTAssertEqual(state(.waiting, waiting: "1 agent"), "Waiting on 1 agent")
        XCTAssertEqual(state(.ended), "Exited, Enter resumes it")
        XCTAssertEqual(state(.exited), "Exited with an error")
        // No model known yet: no empty part.
        XCTAssertEqual(RowFacts(RowFacts.Input(name: "x", status: .idle, agent: "Codex")).tooltip[0], "Codex · Idle")
    }

    func testShellPlaceAndExit() {
        var i = RowFacts.Input(name: "zsh", status: .idle, agent: "Shell")
        i.shellPlace = "src/app"
        i.lastExit = 2
        XCTAssertTrue(RowFacts(i).tooltip.contains("src/app · exit 2"))
        i.shellPlace = nil
        XCTAssertTrue(RowFacts(i).tooltip.contains("Last command: exit 2"))
    }

    /// The row keeps one PR mark: a red ✕ while an open PR's checks fail.
    func testFailingPRMark() {
        XCTAssertTrue(RowFacts.failing(pr(1, failed: 1)))
        XCTAssertFalse(RowFacts.failing(pr(1, pending: 1)))
        XCTAssertFalse(RowFacts.failing(pr(1, failed: 1, state: "merged")))
        XCTAssertFalse(RowFacts.failing(nil))
    }

    func testRoutesListedWhenMoreThanOne() {
        var i = RowFacts.Input(name: "x", status: .working, agent: "Claude Code")
        i.routes = [RouteUsage(route: "a", name: "Claude", input_tokens: 10, output_tokens: 2),
                    RouteUsage(route: "b", name: "GLM", input_tokens: 5, output_tokens: 1)]
        let t = RowFacts(i).tooltip
        XCTAssertTrue(t.contains("Claude: ↑10 in · ↓2 out"))
        XCTAssertTrue(t.contains("GLM: ↑5 in · ↓1 out"))
    }
}

final class StateSectionTests: XCTestCase {
    func testSectionsFollowTheStatusFilters() {
        XCTAssertEqual(StateSection.of(.needsYou), .needsYou)
        XCTAssertEqual(StateSection.of(.thinking), .working)
        XCTAssertEqual(StateSection.of(.waiting), .working)
        XCTAssertEqual(StateSection.of(.done), .done)
        XCTAssertEqual(StateSection.of(.ended), .idle)
        XCTAssertEqual(StateSection.of(.exited), .idle)
        XCTAssertEqual(StateSection.order, [.needsYou, .working, .done, .idle])
    }

    func testSplitKeepsOrderWithPinnedFirst() {
        let a = session("a", "A"), b = session("b", "B", ["pinned": true]), c = session("c", "C"), d = session("d", "D")
        let status: [String: SessionStatus] = ["a": .working, "b": .working, "c": .needsYou, "d": .idle]
        let split = StateSection.split([a, b, c, d]) { status[$0.id]! }
        XCTAssertEqual(split.map(\.0), [.needsYou, .working, .done, .idle])
        XCTAssertEqual(split.map { $0.1.map(\.id) }, [["c"], ["b", "a"], [], ["d"]])
    }

    func testPlacesNameTheRepoWorktreeHostOrFolder() {
        let repo = RepoInfo(path: "/code/acme-api", name: "acme-api", worktrees: [
            Worktree(path: "/code/acme-api", branch: "main", dino: false),
            Worktree(path: "/code/acme-api/.dino/wt/rate-limit", branch: "dino/rate-limit", dino: true),
        ], defaultBranch: "main")
        let main = session("m", "Main")
        let wt = session("w", "In worktree", cwd: "/code/acme-api/.dino/wt/rate-limit")
        let remote = session("r", "Remote", cwd: "/srv/app", ["host": "devbox"])
        let loose = session("l", "Loose", cwd: "/tmp/scratch")
        let places = SessionPlace.index(SessionTree.build(repos: [repo], sessions: [main, wt, remote, loose]))
        XCTAssertEqual(places["m"]?.name, "acme-api")
        XCTAssertNil(places["m"]?.worktree)
        XCTAssertEqual(places["w"]?.name, "acme-api")
        XCTAssertEqual(places["w"]?.worktree?.path, "/code/acme-api/.dino/wt/rate-limit")
        XCTAssertEqual(places["r"]?.name, "devbox")
        XCTAssertEqual(places["l"]?.name, "scratch")
    }
}

final class SidebarSettingsTests: XCTestCase {
    func testGroupingIsStoredLikeTheFilter() {
        XCTAssertEqual(SidebarGrouping.key, "sidebar.groupBy")
        XCTAssertEqual(SidebarGrouping(rawValue: "state"), .state)
        XCTAssertEqual(SidebarGrouping(rawValue: "project"), .project)
        XCTAssertEqual(SidebarGrouping.allCases.map(\.label), ["Project", "State"])
    }

    func testFilterMenuSaysWhatItShows() {
        XCTAssertEqual(ScopeMenu.summary(scope: nil, filter: .all, grouping: .project), "Filter and group: by project")
        XCTAssertEqual(ScopeMenu.summary(scope: "acme-api", filter: .needsYou, grouping: .state), "Filter and group: by state, Needs you only, in acme-api")
    }

    func testPeerLinks() {
        let lead = session("lead", "Lead")
        let child = session("child", "Child", ["started_by": "lead"])
        let other = session("other", "Other", ["messaged_by": "lead"])
        XCTAssertEqual(PeerLinks.of(child, among: [lead, child, other]).map(\.help), ["Started by Lead's agent"])
        XCTAssertEqual(PeerLinks.of(lead, among: [lead, child, other]).map(\.help), ["Its agent started Child", "Its agent last messaged Other"])
    }
}

/// An agent's own scheduled prompts under Automations: one line, the rest in the tooltip.
final class AgentCronTests: XCTestCase {
    func testDecodesTheAgentsScheduledPrompts() {
        let s = session("s1", "deploy", ["tasks": ["todos": [], "subagents": [], "background": [], "crons": [
            ["id": "efc5ae94", "schedule": "23 * * * *", "recurring": true, "prompt": "Post-merge production watch: follow up",
             "human": "Every hour at :23", "next_due": 1_791_400_980],
        ]]])
        XCTAssertEqual(s.tasks?.crons?.first?.id, "efc5ae94")
        XCTAssertEqual(s.tasks?.crons?.first?.next_due, 1_791_400_980)
        // An older dinod says nothing of them.
        let old = session("s2", "old", ["tasks": ["todos": [], "subagents": [], "background": []]])
        XCTAssertNil(old.tasks?.crons)
    }

    func testRowSaysThePromptAndTheRestIsInTheTooltip() {
        let f = CronFacts(prompt: "Post-merge production watch: follow the deploy\nthen report", agent: "Claude Code",
                          session: "deploy", schedule: "23 * * * *", human: "Every hour at :23", recurring: true, next: "today 12:23 PM")
        XCTAssertEqual(f.title, "Post-merge production watch: follow the deploy")
        XCTAssertEqual(f.when, "Every hour at :23 (23 * * * *)")
        XCTAssertEqual(f.tooltip, [
            "Post-merge production watch: follow the deploy\nthen report",
            "Scheduled by Claude Code in “deploy”",
            "Every hour at :23 (23 * * * *)",
            "Next due about today 12:23 PM",
            "It's the session's own: it goes when Claude Code deletes it or the session ends.",
        ])
        XCTAssertEqual(f.accessibility,
                       "Scheduled prompt: Post-merge production watch: follow the deploy, Claude Code in deploy, Every hour at :23 (23 * * * *), next due about today 12:23 PM")
    }

    func testOnlyWhatTheAgentSaidAndTheExpressionReads() {
        // Listed at a turn's end: no wording of the agent's, and an expression dinod can't read.
        var f = CronFacts(prompt: "", agent: "Claude Code", session: "s", schedule: "59 23 31 12 *", human: nil, recurring: false, next: nil)
        XCTAssertEqual(f.title, "Scheduled prompt")
        XCTAssertEqual(f.when, "Once, at 59 23 31 12 *")
        XCTAssertFalse(f.tooltip.contains { $0.hasPrefix("Next due") })
        // The agent's wording that's just the expression isn't said twice.
        f.human = "59 23 31 12 *"
        XCTAssertEqual(f.when, "Once, at 59 23 31 12 *")
    }

    func testNextDueSaysTheDateBeyondTheWeek() {
        let now = Date(timeIntervalSince1970: 1_791_400_000)
        let soon = UInt64(now.timeIntervalSince1970) + 3600
        XCTAssertEqual(cronWhen(soon, now: now), whenText(soon))
        let far = UInt64(now.timeIntervalSince1970) + 80 * 86400
        let date = Date(timeIntervalSince1970: TimeInterval(far))
        XCTAssertEqual(cronWhen(far, now: now),
                       "\(date.formatted(.dateTime.month(.abbreviated).day())) \(date.formatted(date: .omitted, time: .shortened))")
    }
}

/// A session row's Copy items: dino's id always, the agent's conversation and messaging name only
/// when dinod knows them, each copied as plain text.
final class SessionCopyTests: XCTestCase {
    private func titles(_ s: SessionInfo) -> [String] { SessionCopy.of(s).map(\.title) }
    private func value(_ title: String, _ s: SessionInfo) -> String? { SessionCopy.of(s).first { $0.title == title }?.value }

    func testDinoIDAlwaysTheRestOnlyWhenKnown() {
        let bare = session("7", "Faster CI")
        XCTAssertEqual(titles(bare), ["Copy Session ID"])
        XCTAssertEqual(value("Copy Session ID", bare), "7", "what dino attach takes")

        let claude = session("7", "Faster CI", ["conversation": "0b5c-uuid", "peer_name": "acme-api-3f"])
        XCTAssertEqual(titles(claude), ["Copy Session ID", "Copy Conversation ID", "Copy Agent Name"])
        XCTAssertEqual(value("Copy Conversation ID", claude), "0b5c-uuid")
        XCTAssertEqual(value("Copy Agent Name", claude), "acme-api-3f")
        XCTAssertTrue(SessionCopy.of(claude)[1].help.contains("claude --resume"))

        let blank = session("7", "Faster CI", ["conversation": "", "peer_name": " "])
        XCTAssertEqual(titles(blank), ["Copy Session ID"], "nothing empty is offered")
    }

    /// A shell with an agent typed into it: that agent's conversation.
    func testAShellsTypedAgentsConversation() {
        let inside: [String: Any] = ["source": "running", "agent": "codex", "session_id": "019a-thread", "title": "", "updated_at": 0, "args": [String]()]
        let shell = session("9", "zsh", ["agent_id": "shell", "inside": inside])
        XCTAssertEqual(value("Copy Conversation ID", shell), "019a-thread")
        XCTAssertTrue(SessionCopy.of(shell)[1].help.contains("codex queue --thread"))
        XCTAssertEqual(titles(session("9", "zsh", ["agent_id": "shell"])), ["Copy Session ID"])
    }

    /// Copying replaces what the pasteboard held with the value as plain text (a private
    /// pasteboard here, never the user's clipboard).
    func testCopyPutsThePlainValueOnThePasteboard() {
        let board = NSPasteboard(name: NSPasteboard.Name("dino-test-\(UUID().uuidString)"))
        defer { board.releaseGlobally() }
        board.clearContents()
        board.setString("something else", forType: .string)
        let s = session("42", "Faster CI", ["conversation": "0b5c-uuid", "peer_name": "api-worker"])
        for item in SessionCopy.of(s) {
            item.copy(to: board)
            XCTAssertEqual(board.string(forType: .string), item.value, item.title)
            XCTAssertEqual(board.types?.first, .string, "plain text only")
        }
    }
}
