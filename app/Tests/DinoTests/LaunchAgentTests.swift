import XCTest
@testable import Dino

/// When dinod has to restart for its launch agent (#157): Restart to Update shows only then.
final class DinodAgentRestartTests: XCTestCase {
    private let label = "dev.dino.app.dinod-host"

    private func needsRestart(under: String?, thisBuild: Bool, stale: Bool,
                              registeredPlist: String? = "p1", plist: String? = "p1") -> Bool {
        DinodAgent.needsRestart(label: label, runningUnder: under, thisBuild: thisBuild, stale: stale,
                                registeredPlist: registeredPlist, plist: plist)
    }

    /// An update installed as dino quit: the old app registered the agent, and launchd already
    /// runs the new dinod under it. Nothing to finish.
    func testNewDinodUnderAgentRegisteredByOldBuildNeedsNoRestart() {
        XCTAssertFalse(needsRestart(under: label, thisBuild: true, stale: true))
        // Registered by a dino from before the plist was recorded on its own.
        XCTAssertFalse(needsRestart(under: label, thisBuild: true, stale: true, registeredPlist: nil))
    }

    /// The app is new, and the dinod launchd runs is still the old one.
    func testOldDinodUnderAgentNeedsRestart() {
        XCTAssertTrue(needsRestart(under: label, thisBuild: false, stale: true))
    }

    /// launchd couldn't start the new build under the old registration (an ad hoc signature), and
    /// `dino ping` started dinod itself.
    func testDinodOutsideAgentNeedsRestart() {
        XCTAssertTrue(needsRestart(under: nil, thisBuild: true, stale: true))
        XCTAssertTrue(needsRestart(under: nil, thisBuild: true, stale: false))
        XCTAssertTrue(needsRestart(under: "dev.dino.app.dinod", thisBuild: true, stale: false))
    }

    /// The update changed the agent's plist: launchd runs it as registered until it's registered again.
    func testChangedPlistNeedsRestart() {
        XCTAssertTrue(needsRestart(under: label, thisBuild: true, stale: true, registeredPlist: "p1", plist: "p2"))
    }

    func testNothingChangedNeedsNoRestart() {
        XCTAssertFalse(needsRestart(under: label, thisBuild: true, stale: false))
    }
}
