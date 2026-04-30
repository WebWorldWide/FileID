// Generates the DMG window background — yellow/black hazard-tape
// ring around an iridescent centre with a leftward arrow between
// the two icon slots. Output: 600×400 logical, rendered @2x.
//
// Usage: swift scripts/make_dmg_background.swift <output.png>

import AppKit
import CoreGraphics

guard CommandLine.arguments.count == 2 else {
    FileHandle.standardError.write(Data("usage: make_dmg_background.swift <output.png>\n".utf8))
    exit(1)
}
let outputPath = CommandLine.arguments[1]

// Logical 600×400, rendered @2x for retina.
let logical = CGSize(width: 600, height: 400)
let scale: CGFloat = 2
let pixelSize = CGSize(width: logical.width * scale, height: logical.height * scale)

let colorSpace = CGColorSpaceCreateDeviceRGB()
guard let ctx = CGContext(
    data: nil,
    width: Int(pixelSize.width),
    height: Int(pixelSize.height),
    bitsPerComponent: 8,
    bytesPerRow: 0,
    space: colorSpace,
    bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
) else { exit(1) }

ctx.scaleBy(x: scale, y: scale)

let outerRect = CGRect(origin: .zero, size: logical)
let stripeWidth: CGFloat = 30
let outerCorner: CGFloat = 28
let innerInset: CGFloat = stripeWidth
let innerCorner: CGFloat = 18
let innerRect = outerRect.insetBy(dx: innerInset, dy: innerInset)

let outerPath = CGPath(roundedRect: outerRect,
                        cornerWidth: outerCorner,
                        cornerHeight: outerCorner,
                        transform: nil)
let innerPath = CGPath(roundedRect: innerRect,
                        cornerWidth: innerCorner,
                        cornerHeight: innerCorner,
                        transform: nil)

// MARK: - Iridescent centre

ctx.saveGState()
ctx.addPath(innerPath)
ctx.clip()

ctx.setFillColor(CGColor(red: 0.86, green: 0.86, blue: 0.88, alpha: 1.0))
ctx.fill(innerRect)

struct Blob { let x: CGFloat; let y: CGFloat; let r: CGFloat; let color: CGColor }
let blobs: [Blob] = [
    Blob(x: innerRect.midX - 110, y: innerRect.midY + 60, r: 200,
         color: CGColor(red: 0.55, green: 0.95, blue: 0.75, alpha: 0.55)),
    Blob(x: innerRect.midX + 130, y: innerRect.midY - 50, r: 220,
         color: CGColor(red: 0.65, green: 0.85, blue: 1.00, alpha: 0.55)),
    Blob(x: innerRect.midX - 80,  y: innerRect.midY - 90, r: 180,
         color: CGColor(red: 1.00, green: 0.75, blue: 0.90, alpha: 0.55)),
    Blob(x: innerRect.midX + 70,  y: innerRect.midY + 80, r: 200,
         color: CGColor(red: 1.00, green: 0.78, blue: 0.55, alpha: 0.50)),
    Blob(x: innerRect.midX,       y: innerRect.midY,      r: 260,
         color: CGColor(red: 1.00, green: 1.00, blue: 1.00, alpha: 0.45))
]
for b in blobs {
    let g = CGGradient(colorsSpace: colorSpace,
                        colors: [b.color, b.color.copy(alpha: 0)!] as CFArray,
                        locations: [0, 1])!
    ctx.drawRadialGradient(
        g,
        startCenter: CGPoint(x: b.x, y: b.y), startRadius: 0,
        endCenter: CGPoint(x: b.x, y: b.y), endRadius: b.r,
        options: []
    )
}

// Soft inner vignette so the border reads as a frame rather than
// stripes painted on top of the centre.
let vignette = CGGradient(
    colorsSpace: colorSpace,
    colors: [
        CGColor(red: 0, green: 0, blue: 0, alpha: 0),
        CGColor(red: 0, green: 0, blue: 0, alpha: 0.30)
    ] as CFArray,
    locations: [0.55, 1.0]
)!
ctx.drawRadialGradient(
    vignette,
    startCenter: CGPoint(x: innerRect.midX, y: innerRect.midY),
    startRadius: 0,
    endCenter: CGPoint(x: innerRect.midX, y: innerRect.midY),
    endRadius: max(innerRect.width, innerRect.height) * 0.55,
    options: []
)

// Leftward arrow between the two icon slots. Icons sit at logical
// x=160 (Applications, left) and x=440 (FileID.app, right) per the
// AppleScript layout; the arrow points from just inside the right
// icon toward just inside the left icon.
let arrowY = innerRect.midY + 4
let arrowEndX: CGFloat = 248
let arrowStartX: CGFloat = 352
ctx.setStrokeColor(CGColor(red: 0.05, green: 0.05, blue: 0.05, alpha: 0.95))
ctx.setLineWidth(7)
ctx.setLineCap(.round)
ctx.setLineJoin(.round)
ctx.move(to: CGPoint(x: arrowStartX, y: arrowY))
ctx.addLine(to: CGPoint(x: arrowEndX, y: arrowY))
ctx.strokePath()
let headLen: CGFloat = 18
ctx.move(to: CGPoint(x: arrowEndX, y: arrowY))
ctx.addLine(to: CGPoint(x: arrowEndX + headLen, y: arrowY + headLen * 0.7))
ctx.move(to: CGPoint(x: arrowEndX, y: arrowY))
ctx.addLine(to: CGPoint(x: arrowEndX + headLen, y: arrowY - headLen * 0.7))
ctx.strokePath()

ctx.restoreGState()

// MARK: - Hazard ring

ctx.saveGState()
// Even-odd ring: outer rounded rect minus inner rounded rect.
ctx.addPath(outerPath)
ctx.addPath(innerPath)
ctx.clip(using: .evenOdd)

ctx.saveGState()
ctx.translateBy(x: logical.width / 2, y: logical.height / 2)
ctx.rotate(by: -.pi / 4)
let yellow = CGColor(red: 0.99, green: 0.78, blue: 0.0, alpha: 1.0)
let black  = CGColor(red: 0.10, green: 0.10, blue: 0.10, alpha: 1.0)
let stripe = stripeWidth * 0.95
let span: CGFloat = 900
var x: CGFloat = -span
while x < span {
    ctx.setFillColor(yellow)
    ctx.fill(CGRect(x: x, y: -span, width: stripe, height: span * 2))
    ctx.setFillColor(black)
    ctx.fill(CGRect(x: x + stripe, y: -span, width: stripe, height: span * 2))
    x += stripe * 2
}
ctx.restoreGState()
ctx.restoreGState()

// Subtle outer drop-shadow look — a 1pt darker rim outside the stripes.
ctx.setStrokeColor(CGColor(red: 0, green: 0, blue: 0, alpha: 0.35))
ctx.setLineWidth(1)
ctx.addPath(outerPath)
ctx.strokePath()

// MARK: - Output

guard let cgImage = ctx.makeImage() else { exit(1) }
let rep = NSBitmapImageRep(cgImage: cgImage)
rep.size = logical
guard let pngData = rep.representation(using: .png, properties: [:]) else { exit(1) }
let outURL = URL(fileURLWithPath: outputPath)
try? FileManager.default.createDirectory(
    at: outURL.deletingLastPathComponent(),
    withIntermediateDirectories: true
)
try? pngData.write(to: outURL)
print("wrote \(outputPath) (\(Int(pixelSize.width))×\(Int(pixelSize.height)))")
