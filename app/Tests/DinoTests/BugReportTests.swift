import Foundation
import XCTest
@testable import Dino

/// Help → Report a Bug…: GitHub's bug form with the versions filled in, and nothing private.
final class BugReportTests: XCTestCase {
    private let facts = BugReport.Facts(
        app: "0.1.7 (build 12, c1ddaea2f)", dinod: "0.1.7 (c1ddaea2f)",
        macos: "macOS 26.4 (25E246) on Apple M3 Pro",
        agents: [("Claude Code", "2.1.288"), ("Codex", "0.50.0")]
    )

    /// The value of `key` in `url`'s query, decoded as GitHub reads it.
    private func field(_ url: URL, _ key: String) -> String? {
        URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems?.first { $0.name == key }?.value
    }

    /// bug.yml's field ids, filled in.
    func testFillsTheBugForm() {
        let url = BugReport.url(facts)
        XCTAssertTrue(url.absoluteString.hasPrefix("https://github.com/meetdino/dino/issues/new?template=bug.yml&"))
        XCTAssertEqual(field(url, "version"), "0.1.7 (build 12, c1ddaea2f); dinod 0.1.7 (c1ddaea2f)")
        XCTAssertEqual(field(url, "macos"), "macOS 26.4 (25E246) on Apple M3 Pro")
        XCTAssertEqual(field(url, "agent"), "Claude Code 2.1.288, Codex 0.50.0")
    }

    /// The form's own field ids, so a renamed one in bug.yml fails here rather than leaving it empty.
    func testFieldIdsAreTheTemplates() throws {
        let template = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            .appendingPathComponent(".github/ISSUE_TEMPLATE/bug.yml")
        let yaml = try String(contentsOf: template, encoding: .utf8)
        for id in ["version", "macos", "agent"] {
            XCTAssertTrue(yaml.contains("id: \(id)\n"), "bug.yml has no field \(id)")
        }
    }

    /// dinod not answering, no agent found: the form still opens, saying so.
    func testWithoutDinodOrAgents() {
        let url = BugReport.url(.init(app: "0.1.7", dinod: nil, macos: "macOS 26.4", agents: []))
        XCTAssertEqual(field(url, "version"), "0.1.7; dinod not running")
        XCTAssertNil(field(url, "agent"))
    }

    /// `&`, `=`, `+`, `#`, spaces and non-ASCII stay inside their value.
    func testEscapes() {
        let url = BugReport.url(.init(app: "1.0 a&b=c+d #e ü…", dinod: "x?y", macos: "macOS", agents: [("A&B", "1+2")]))
        let text = url.absoluteString
        XCTAssertFalse(text.contains(" "))
        XCTAssertFalse(text.contains("+"), "a + would read as a space")
        XCTAssertFalse(text.contains("#"), "a # would end the query")
        XCTAssertEqual(text.components(separatedBy: "&").count, 4, "template, version, macos, agent: no & of their own")
        XCTAssertEqual(field(url, "version"), "1.0 a&b=c+d #e ü…; dinod x?y")
        XCTAssertEqual(field(url, "agent"), "A&B 1+2")
    }

    /// No path, so none with your name in it, and one line per field.
    func testNoPathsOrNewlines() {
        let url = BugReport.url(.init(
            app: "0.1.7\n/Users/someone/Applications/Dino.app", dinod: "0.1.7 (abc) ~/.config/dino",
            macos: "macOS\t26.4", agents: [("Claude Code", "2.1.288 /Users/someone/.local/bin/claude")]
        ))
        XCTAssertFalse(url.absoluteString.contains("someone"))
        XCTAssertFalse(url.absoluteString.contains("%0A"))
        XCTAssertEqual(field(url, "version"), "0.1.7; dinod 0.1.7 (abc)")
        XCTAssertEqual(field(url, "macos"), "macOS 26.4")
        XCTAssertEqual(field(url, "agent"), "Claude Code 2.1.288")
    }

    /// A long field is cut; too many agents and the last ones are left out, the versions kept.
    func testLengthLimits() {
        let long = String(repeating: "9", count: 5000)
        let url = BugReport.url(.init(app: long, dinod: long, macos: long, agents: []))
        XCTAssertEqual(field(url, "version")?.count, BugReport.fieldLimit)
        XCTAssertEqual(field(url, "version")?.last, "…")
        XCTAssertEqual(field(url, "macos")?.count, BugReport.fieldLimit)

        let many = (0..<400).map { (name: "Agent ü&\($0)", version: "1.\($0)") }
        let crowded = BugReport.url(.init(app: "0.1.7", dinod: "0.1.7", macos: "macOS 26.4", agents: many))
        XCTAssertLessThanOrEqual(crowded.absoluteString.count, BugReport.urlLimit)
        XCTAssertEqual(field(crowded, "version"), "0.1.7; dinod 0.1.7")
        XCTAssertEqual(field(crowded, "macos"), "macOS 26.4")
        XCTAssertTrue(field(crowded, "agent")?.hasPrefix("Agent ü&0 1.0, Agent ü&1 1.1") == true)

        // Every field at its limit, in the widest escaping: each cut, still under the limit.
        let wide = String(repeating: "ü", count: 5000)
        let widest = BugReport.url(.init(app: wide, dinod: wide, macos: wide, agents: [(wide, wide)]))
        XCTAssertLessThanOrEqual(widest.absoluteString.count, BugReport.urlLimit)
        XCTAssertEqual(field(widest, "macos")?.last, "…")
        XCTAssertLessThanOrEqual(BugReport.escape(field(widest, "macos") ?? "").count, BugReport.escapedLimit)
    }

    /// This Mac's versions, as filled in: no path and no name.
    func testThisMac() {
        XCTAssertTrue(BugReport.macos.hasPrefix("macOS "))
        XCTAssertFalse(BugReport.macos.contains("/"))
        XCTAssertFalse(BugReport.appVersion.isEmpty)
    }
}
