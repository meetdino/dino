// Renders the dino app icon (the landing page's pixel sprite on a macOS icon plate) to a 1024px PNG.
// usage: swift Scripts/make-icon.swift out.png
import AppKit

let body: [(Int, Int, Int, Int)] = [(9, 20, 3, 4), (17, 20, 2, 4), (10, 0, 16, 10), (10, 10, 11, 2), (9, 12, 12, 2), (22, 12, 3, 2), (0, 14, 2, 2), (8, 14, 15, 2), (2, 16, 19, 2), (4, 18, 17, 2)]
let spikes: [(Int, Int, Int, Int)] = [(8, 2, 2, 2), (8, 6, 2, 2), (8, 10, 2, 2), (7, 12, 2, 2), (2, 14, 6, 2)]
let eyes: [(Int, Int, Int, Int)] = [(17, 4, 2, 2), (22, 4, 2, 2)]

func rgb(_ hex: UInt32) -> NSColor {
    NSColor(srgbRed: CGFloat(hex >> 16 & 0xFF) / 255, green: CGFloat(hex >> 8 & 0xFF) / 255, blue: CGFloat(hex & 0xFF) / 255, alpha: 1)
}

let size = 1024
let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: size, pixelsHigh: size, bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
let ctx = NSGraphicsContext.current!.cgContext
// Top-left origin, like the SVG.
ctx.translateBy(x: 0, y: CGFloat(size))
ctx.scaleBy(x: 1, y: -1)

// macOS icon grid: 824pt plate inset 100pt, continuous-ish corners.
let plate = CGRect(x: 100, y: 100, width: 824, height: 824)
let path = CGPath(roundedRect: plate, cornerWidth: 186, cornerHeight: 186, transform: nil)
ctx.saveGState()
ctx.addPath(path)
ctx.clip()
let gradient = CGGradient(colorsSpace: CGColorSpaceCreateDeviceRGB(), colors: [rgb(0x1B2016).cgColor, rgb(0x0A0C09).cgColor] as CFArray, locations: [0, 1])!
ctx.drawLinearGradient(gradient, start: CGPoint(x: 512, y: 100), end: CGPoint(x: 512, y: 924), options: [])
// Ground line the dino stands on.
ctx.setFillColor(rgb(0x75B340).withAlphaComponent(0.25).cgColor)
ctx.fill(CGRect(x: 180, y: 752, width: 664, height: 8))
ctx.restoreGState()

let px: CGFloat = 22
let origin = CGPoint(x: (CGFloat(size) - 26 * px) / 2, y: 752 - 24 * px)
func draw(_ rects: [(Int, Int, Int, Int)], _ color: UInt32) {
    ctx.setFillColor(rgb(color).cgColor)
    for (x, y, w, h) in rects {
        ctx.fill(CGRect(x: origin.x + CGFloat(x) * px, y: origin.y + CGFloat(y) * px, width: CGFloat(w) * px, height: CGFloat(h) * px))
    }
}
draw(body, 0x75B340)
draw(spikes, 0xFC4F26)
draw(eyes, 0x0A0C09)

NSGraphicsContext.current = nil
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: CommandLine.arguments[1]))
