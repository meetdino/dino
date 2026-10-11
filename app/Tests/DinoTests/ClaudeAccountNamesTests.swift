import Foundation
import XCTest
@testable import Dino

/// Claude accounts by the names the user gave them: account 1 by Claude Code's email until then,
/// the others by number ("Account 2") until named, and from an older dinod, by number as before.
final class ClaudeAccountNamesTests: XCTestCase {
    private func account(_ json: String) -> ClaudeAccountInfo {
        try! JSONDecoder().decode(ClaudeAccountInfo.self, from: Data(json.utf8))
    }

    func testAccountsAreCalledByTheirNames() {
        let own = account(#"{"number":1,"answering":true,"spent":false,"name":"me@example.com","email":"me@example.com","plan":"max"}"#)
        XCTAssertEqual(own.label, "me@example.com")
        XCTAssertEqual(own.short, "me@example.com")
        XCTAssertEqual(own.planName, "Max")
        let named = account(#"{"number":2,"answering":false,"spent":true,"name":"Work"}"#)
        XCTAssertEqual(named.label, "Work")
        XCTAssertEqual(named.short, "Work", "as the proxy names a session on it")
        // No name, or an older dinod: by number, as before.
        let plain = account(#"{"number":3,"answering":false,"spent":false}"#)
        XCTAssertEqual(plain.label, "Account 3")
        XCTAssertEqual(plain.short, "Claude account 3")
        XCTAssertNil(plain.email)
    }

    /// A session on another account says both by name: the one answering and Claude Code's own.
    func testASessionOnAnotherAccountSaysTheirNames() {
        let before = ClaudeAccountInfo.ownShort
        defer { ClaudeAccountInfo.ownShort = before }
        ClaudeAccountInfo.ownShort = "me@example.com"
        let f = FallbackInfo(provider: "anthropic", name: "Work", model: "claude-opus-5-5", from: "Claude", reason: "limit", said: "spent", resets_at: nil, retry_at: nil, since: 0)
        XCTAssertTrue(f.isAccount)
        XCTAssertEqual(f.label, "On Work · me@example.com at its limit")
        XCTAssertEqual(f.rowLabel, "On Work")
        XCTAssertTrue(FallbackChip.detail(f, []).hasPrefix("me@example.com said: spent"), "Claude Code's own account, by its name")
    }

    /// The account chosen with Use This Account says so; account 1, which dinod marks chosen while
    /// no other is, isn't "chosen by you"; an older dinod says nothing. A session that the chosen
    /// one's limit moved to Claude Code's own sign-in ("Claude", as dinod calls it) says both by
    /// the names the list uses.
    func testAChosenAccountAndASessionMovedOffIt() {
        let before = ClaudeAccountInfo.ownShort
        defer { ClaudeAccountInfo.ownShort = before }
        ClaudeAccountInfo.ownShort = "me@example.com"
        let work = account(#"{"number":3,"answering":true,"chosen":true,"spent":false,"name":"Work"}"#)
        XCTAssertTrue(work.isChosen && work.chosenByYou)
        let own = account(#"{"number":1,"answering":true,"chosen":true,"spent":false}"#)
        XCTAssertTrue(own.isChosen && !own.chosenByYou)
        let older = account(#"{"number":2,"answering":false,"spent":false}"#)
        XCTAssertNil(older.chosen)
        XCTAssertFalse(older.isChosen)

        let f = FallbackInfo(provider: "anthropic", name: "Claude", model: "claude-opus-5-5", from: "Work", reason: "limit", said: "spent", resets_at: nil, retry_at: nil, since: 0)
        XCTAssertEqual(f.accountName, "me@example.com")
        XCTAssertEqual(f.accountFrom, "Work")
        XCTAssertEqual(f.label, "On me@example.com · Work at its limit")
        XCTAssertEqual(f.rowLabel, "On me@example.com")
        XCTAssertTrue(FallbackChip.detail(f, []).hasPrefix("Work said: spent\nAnswering: me@example.com · claude-opus-5-5"))
    }
}
