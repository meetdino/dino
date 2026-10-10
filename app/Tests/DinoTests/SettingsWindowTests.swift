import AppKit
import SwiftUI
import XCTest
@testable import Dino

/// Settings opens with the keyboard on the window, not its search field: a text field taking it at
/// open made macOS add its text-input windows (TextInputUI's 500×500 TUINSWindow, AutoFill's
/// SPRoundedWindow) beside Settings. The window is never shown.
@MainActor
final class SettingsWindowTests: XCTestCase {
    func testKeyboardStartsOffTheSearchField() throws {
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 800, height: 600), styleMask: [.titled], backing: .buffered, defer: true)
        window.isReleasedWhenClosed = false
        let root = VStack {
            TextField("Search", text: .constant(""))
            Text("Page")
        }
        .background(SettingsWindowSetup(minimum: NSSize(width: 760, height: 500)))
        window.contentView = NSHostingView(rootView: root)
        window.contentView?.layoutSubtreeIfNeeded()

        let first = try XCTUnwrap(window.initialFirstResponder, "AppKit would pick the first key view: the search field")
        XCTAssertFalse(first is NSTextField || first is NSText)
        XCTAssertTrue(first.acceptsFirstResponder)
        XCTAssertEqual(window.contentMinSize, NSSize(width: 760, height: 500))
    }
}
