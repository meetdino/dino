import Foundation
import XCTest
@testable import Dino

/// Swift Charts traps (EXC_BREAKPOINT, the whole app gone) when a mark's value isn't in the domain
/// a chart gives its scale. The Usage window's charts build their domains from their data.
final class StatsChartTests: XCTestCase {
    private func report(_ j: [String: Any]) -> StatsReport {
        try! JSONDecoder().decode(StatsReport.self, from: JSONSerialization.data(withJSONObject: j))
    }

    private static func tok(_ total: UInt64) -> [String: Any] { ["total": total] }

    private static func parts(_ main: UInt64, _ sub: UInt64, _ side: UInt64) -> [String: Any] {
        ["main": ["tokens": tok(main)], "subagents": ["tokens": tok(sub)], "side": ["tokens": tok(side)]]
    }

    private func agents(_ list: [[String: Any]]) -> AgentBars {
        AgentBars(report(["agents": list]).agents)
    }

    private func assertEveryBarInDomain(_ c: AgentBars, file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertEqual(c.keys.count, c.colors.count, "a colour for each key", file: file, line: line)
        XCTAssertEqual(Set(c.keys).count, c.keys.count, "no key twice", file: file, line: line)
        for b in c.bars { XCTAssertTrue(c.keys.contains(b.key), "\(b.key) isn't in the domain \(c.keys)", file: file, line: line) }
        for b in c.bars { XCTAssertGreaterThan(b.tokens, 0, file: file, line: line) }
    }

    /// What crashed the founder's dino (0.1.10, 30 days): Claude split by part, and a shell whose
    /// one call used no tokens, so its parts were all zero. Its bar was "Tokens", which the
    /// scale (Conversations, Subagents, Side requests) didn't hold.
    func testSplitAgentsBesideAShellWithNoTokens() {
        let c = agents([
            ["agent": "claude", "tokens": Self.tok(1000), "parts": Self.parts(600, 390, 10)],
            ["agent": "unknown", "tokens": Self.tok(50), "parts": Self.parts(10, 40, 0)],
            ["agent": "codex", "tokens": Self.tok(20), "parts": Self.parts(20, 0, 0)],
            ["agent": "shell", "tokens": Self.tok(0), "parts": Self.parts(0, 0, 0)],
        ])
        assertEveryBarInDomain(c)
        XCTAssertTrue(c.split)
        XCTAssertFalse(c.bars.contains { $0.agent == "shell" }, "nothing to draw for no tokens")
        XCTAssertEqual(c.rows, 3)
        XCTAssertEqual(c.keys, ["Conversations", "Subagents", "Side requests"])
    }

    /// The same with tokens: an agent dinod doesn't split (an older dinod's, or one with no parts)
    /// next to one it does. Its bar keeps its tokens, under a "Tokens" key the domain holds.
    func testSplitAgentsBesideOneNotSplit() {
        let c = agents([
            ["agent": "claude", "tokens": Self.tok(1000), "parts": Self.parts(600, 400, 0)],
            ["agent": "codex", "tokens": Self.tok(500)],
            ["agent": "pi", "tokens": Self.tok(70), "parts": Self.parts(0, 0, 0)],
        ])
        assertEveryBarInDomain(c)
        XCTAssertEqual(c.keys, ["Conversations", "Subagents", "Tokens"])
        XCTAssertEqual(c.bars.filter { $0.key == "Tokens" }.map(\.agent), ["codex", "pi"])
        XCTAssertEqual(c.rows, 3)
    }

    func testNoAgentSplit() {
        let c = agents([["agent": "codex", "tokens": Self.tok(500)], ["agent": "pi", "tokens": Self.tok(5)]])
        assertEveryBarInDomain(c)
        XCTAssertFalse(c.split)
        XCTAssertEqual(c.keys, ["Tokens"])
    }

    /// Nothing, or calls that used no tokens at all: an empty chart, not a trap.
    func testNoTokensAtAll() {
        for list in [[], [["agent": "shell", "tokens": Self.tok(0)]], [["agent": "claude", "tokens": Self.tok(0), "parts": Self.parts(0, 0, 0)]]] {
            let c = agents(list)
            assertEveryBarInDomain(c)
            XCTAssertEqual(c.bars, [])
            XCTAssertEqual(c.keys, [])
            XCTAssertEqual(c.rows, 0)
        }
    }

    /// Models whose short names are the same ("claude-haiku-4-5" and its dated id): one series,
    /// named once, and every line's label in the domain.
    func testModelScaleHoldsEveryLabelOnce() {
        let models = ["claude-haiku-4-5-20251001", "claude-haiku-4-5", "gpt-5.4", "Other"]
        let r = report([
            "models": models.map { ["model": $0, "tokens": Self.tok(10)] },
            "daily_models": ["2026-10-08", "2026-10-09"].flatMap { d in models.map { ["date": d, "model": $0, "tokens": 10] } },
        ])
        let s = ModelColors.scale(r)
        XCTAssertEqual(s.domain, ["haiku-4-5", "gpt-5.4", "Other"])
        XCTAssertEqual(s.domain.count, s.range.count)
        for d in r.daily_models { XCTAssertTrue(s.domain.contains(shortModel(d.model))) }
        XCTAssertEqual(ModelColors.scale(report([:])).domain, [])
    }

    /// The hours chart's domain: 24 different labels, whatever today is (a daylight saving day
    /// has 23 or 25 hours, and the labels came from today).
    func testHourLabelsAreTwentyFourDifferentOnes() {
        let labels = (0..<24).map(StatsFormat.hour)
        XCTAssertEqual(Set(labels).count, 24, "\(labels)")
    }
}
