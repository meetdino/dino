import Foundation
import XCTest
@testable import Dino

/// Every settings.toml key has a control Settings' search finds and can take you to.
final class SettingsIndexTests: XCTestCase {
    private static let repo = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()

    /// crates/dino-core/settings-keys.txt, which a dino-core test keeps up to date.
    private static var keys: [String] {
        let text = try! String(contentsOf: repo.appendingPathComponent("crates/dino-core/settings-keys.txt"), encoding: .utf8)
        return text.split(separator: "\n").map(String.init).filter { !$0.isEmpty }
    }

    /// Every Swift source of the app, as one text.
    private static var sources: String {
        let dir = repo.appendingPathComponent("app/Sources/Dino")
        let files = try! FileManager.default.contentsOfDirectory(at: dir, includingPropertiesForKeys: nil).filter { $0.pathExtension == "swift" }
        return files.map { try! String(contentsOf: $0, encoding: .utf8) }.joined(separator: "\n")
    }

    func testEveryKeyHasASearchEntry() {
        let indexed = Set(SettingsEntry.all.flatMap(\.keys))
        let missing = Self.keys.filter { !indexed.contains($0) && SettingsEntry.notControls[$0] == nil }
        XCTAssertEqual(missing, [], "settings.toml keys no Settings search entry names: add them to SettingsEntry.all")
        let stale = indexed.filter { !$0.hasPrefix("app:") && !Self.keys.contains($0) }
        XCTAssertEqual(stale.sorted(), [], "search entries naming keys settings.toml doesn't have")
    }

    func testEveryEntryIsFoundByItsTitleAndHasAControl() {
        let source = Self.sources
        // Anchors made from an id at run time: the experimental features'.
        let computed = Set(ExperimentalFeature.all.map { $0.id.replacingOccurrences(of: "_", with: "-") })
        for e in SettingsEntry.all {
            XCTAssertTrue(SettingsSearch.results(e.title, managed: true).contains(e), "searching “\(e.title)” doesn't find it")
            for s in e.synonyms {
                XCTAssertTrue(SettingsSearch.results(s, managed: true).contains(e), "searching “\(s)” doesn't find \(e.id)")
            }
            let anchored = source.contains("settingAnchor(\"\(e.id)\")") || source.contains("? \"\(e.id)\" :") || computed.contains(e.id)
                || e.pane == .account || e.pane == .managed
            XCTAssertTrue(anchored, "\(e.id) has no settingAnchor on its page, so a result can't take you to it")
        }
        XCTAssertEqual(Set(SettingsEntry.all.map(\.id)).count, SettingsEntry.all.count, "two entries share an id")
    }

    func testRankingAndOtherWords() {
        XCTAssertEqual(SettingsSearch.results("yolo").first?.id, "allow-bypass")
        XCTAssertEqual(SettingsSearch.results("sleep").first?.pane, .power)
        XCTAssertEqual(SettingsSearch.results("lid").first?.id, "lid")
        XCTAssertEqual(SettingsSearch.results("⌘I").first?.id, "ask-agent")
        XCTAssertEqual(SettingsSearch.results("sccache").first?.id, "build-cache")
        XCTAssertEqual(SettingsSearch.results("branch prefix").first?.id, "branch-prefix", "every word must match")
        XCTAssertEqual(SettingsSearch.results("  "), [])
        XCTAssertEqual(SettingsSearch.results("zzqx"), [])
        XCTAssertFalse(SettingsSearch.results("organization").contains { $0.pane == .managed }, "the Managed page only while something is managed")
    }

    /// One way to sign in to a Claude account: Accounts → Add Account… → Sign in with Claude, which
    /// adds an account. Settings once had a second browser sign-in (the subscription token's
    /// Create…) that looked the same but kept the token for SSH hosts and added no account.
    func testOneWayToSignInToAClaudeAccount() {
        XCTAssertEqual(SettingsPane.claude.title, "Accounts")
        XCTAssertEqual(SettingsPane(rawValue: "claude"), .claude, "the page's raw value stays, so links to it still open")
        let source = Self.sources
        XCTAssertEqual(source.components(separatedBy: "call(\"login\"").count - 1, 1, "one place starts a browser sign-in")
        XCTAssertFalse(source.contains("claudeToken(\"create\"") || source.contains("call(\"create\""), "no other sign-in that runs claude setup-token")
        XCTAssertFalse(source.contains("claudeToken(\"set\""), "no second place to paste a token")
        XCTAssertEqual(source.components(separatedBy: "Button(\"Sign in with Claude\")").count - 1, 1)
    }

    func testOldPageNamesStillOpen() {
        XCTAssertEqual(SettingsPane(rawValue: "terminal"), .shell)
        XCTAssertEqual(SettingsPane(rawValue: "workspaces"), .worktrees)
        XCTAssertEqual(SettingsPane(rawValue: "policies"), .agents)
        XCTAssertEqual(SettingsPane(rawValue: "models"), .models)
        // Every page is in exactly one sidebar group.
        let grouped = SettingsGroup.all.flatMap(\.panes)
        XCTAssertEqual(Set(grouped).count, grouped.count)
        XCTAssertEqual(Set(grouped).union([.account]), Set(SettingsPane.allCases))
    }
}
