import Foundation
import XCTest
@testable import Dino

/// Where ⌘N starts an agent is the folder you chose, and the sidebar lists it under that folder.
final class StartFolderTests: XCTestCase {
    private let home = "/Users/someone"

    /// Your home folder's row selected, ⌘N: the agent starts there, not in the folder used last
    /// (it started in ~/Movies, listed under your home folder).
    func testHomeChosenStartsInHome() {
        XCTAssertEqual(DinoModel.startFolder((home, chosen: true), agent: true, home: home), home)
        XCTAssertEqual(DinoModel.startFolder((home + "/", chosen: true), agent: true, home: home), home + "/")
    }

    /// In your home folder only by default (the shell at launch, nothing used yet): the picker asks,
    /// rather than an agent there or anywhere else in its place. A shell starts there.
    func testHomeByDefaultAsks() {
        XCTAssertNil(DinoModel.startFolder((home, chosen: false), agent: true, home: home))
        XCTAssertEqual(DinoModel.startFolder((home, chosen: false), agent: false, home: home), home)
    }

    /// Anywhere else, chosen or not: right there.
    func testElsewhereStartsThere() {
        for chosen in [true, false] {
            XCTAssertEqual(DinoModel.startFolder((home + "/Movies", chosen: chosen), agent: true, home: home), home + "/Movies")
        }
    }

    /// An agent in ~/Movies goes under ~/Movies's row (dinod gives it one), not your home folder's.
    func testSessionIsUnderItsOwnFolder() throws {
        let json: [String: Any] = [
            "id": "1", "name": "Edit", "agent_id": "claude", "exited": false, "bells": 0, "requests": 0,
            "in_flight": 0, "input_tokens": 0, "output_tokens": 0, "cwd": home + "/Movies",
        ]
        let s = try JSONDecoder().decode(SessionInfo.self, from: JSONSerialization.data(withJSONObject: json))
        let folder = { (path: String) in RepoInfo(path: path, name: URL(fileURLWithPath: path).lastPathComponent, worktrees: []) }
        let tree = SessionTree.build(repos: [folder(home), folder(home + "/Movies")], sessions: [s])
        let place = tree.repos.flatMap(\.places).first { $0.sessions.contains { $0.id == "1" } }
        XCTAssertEqual(place?.path, home + "/Movies")
    }

    /// The folder panel's button names the folder it takes: the highlighted one, else the one shown.
    func testFolderPanelButtonNamesTheFolder() {
        XCTAssertEqual(FolderPanel.prompt("Open", taking: URL(fileURLWithPath: "/tmp/dino-panel-test-nonexistent/Movies")), "Open “Movies”")
        XCTAssertEqual(FolderPanel.prompt("Open", taking: nil), "Open")
    }
}
