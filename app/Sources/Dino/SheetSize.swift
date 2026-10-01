import AppKit
import SwiftUI

extension View {
    /// A sheet's size: `width` by `height`, but never bigger than the window it comes down from,
    /// less a margin, and never smaller than the minimum.
    func sheetSize(width: CGFloat, height: CGFloat, minWidth: CGFloat = 420, minHeight: CGFloat = 320) -> some View {
        let window = (NSApp.mainWindow ?? NSApp.keyWindow)?.contentLayoutRect.size
        let w = window.map { min(width, $0.width - 48) } ?? width
        let h = window.map { min(height, $0.height - 48) } ?? height
        return frame(width: max(minWidth, w), height: max(minHeight, h))
    }
}
