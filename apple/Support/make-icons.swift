// Draws remoteman's app icon (the design's tile: #20324A, a white display outline with a filled
// dot in its centre) and writes every size the apps need into Support/Assets.xcassets.
// Run from apple/: swift Support/make-icons.swift
import AppKit
import CoreGraphics

/// The mark in a square of side `s` (origin bottom left), white on whatever is beneath.
func mark(_ ctx: CGContext, in r: CGRect) {
    let s = r.width
    func p(_ x: CGFloat, _ y: CGFloat) -> CGPoint { CGPoint(x: r.minX + x * s, y: r.minY + y * s) }
    ctx.setStrokeColor(CGColor(gray: 1, alpha: 1))
    ctx.setFillColor(CGColor(gray: 1, alpha: 1))
    ctx.setLineWidth(s * 0.052)
    ctx.setLineCap(.round)
    ctx.setLineJoin(.round)
    // The screen.
    let screen = CGRect(x: r.minX + 0.205 * s, y: r.minY + 0.375 * s, width: 0.59 * s, height: 0.40 * s)
    ctx.addPath(CGPath(roundedRect: screen, cornerWidth: 0.065 * s, cornerHeight: 0.065 * s, transform: nil))
    ctx.strokePath()
    // Its stand.
    ctx.move(to: p(0.5, 0.375))
    ctx.addLine(to: p(0.5, 0.255))
    ctx.move(to: p(0.375, 0.255))
    ctx.addLine(to: p(0.625, 0.255))
    ctx.strokePath()
    // The dot.
    let d = 0.105 * s
    ctx.fillEllipse(in: CGRect(x: screen.midX - d / 2, y: screen.midY - d / 2, width: d, height: d))
}

func image(_ px: Int, mac: Bool) -> Data {
    let space = CGColorSpace(name: CGColorSpace.sRGB)!
    let ctx = CGContext(
        data: nil, width: px, height: px, bitsPerComponent: 8, bytesPerRow: 0, space: space,
        bitmapInfo: (mac ? CGImageAlphaInfo.premultipliedLast : CGImageAlphaInfo.noneSkipLast).rawValue)!
    let s = CGFloat(px)
    let top = CGColor(red: 0x2A / 255, green: 0x41 / 255, blue: 0x5F / 255, alpha: 1)
    let base = CGColor(red: 0x20 / 255, green: 0x32 / 255, blue: 0x4A / 255, alpha: 1)
    let bottom = CGColor(red: 0x19 / 255, green: 0x28 / 255, blue: 0x3C / 255, alpha: 1)
    let gradient = CGGradient(colorsSpace: space, colors: [top, base, bottom] as CFArray, locations: [0, 0.5, 1])!
    if mac {
        // A Mac icon is a rounded tile inside the canvas, with a soft shadow under it.
        let tile = CGRect(x: s * 100 / 1024, y: s * 100 / 1024, width: s * 824 / 1024, height: s * 824 / 1024)
        let shape = CGPath(roundedRect: tile, cornerWidth: s * 185 / 1024, cornerHeight: s * 185 / 1024, transform: nil)
        ctx.saveGState()
        ctx.setShadow(offset: CGSize(width: 0, height: -s * 10 / 1024), blur: s * 22 / 1024, color: CGColor(gray: 0, alpha: 0.32))
        ctx.addPath(shape)
        ctx.setFillColor(base)
        ctx.fillPath()
        ctx.restoreGState()
        ctx.saveGState()
        ctx.addPath(shape)
        ctx.clip()
        ctx.drawLinearGradient(gradient, start: CGPoint(x: 0, y: tile.maxY), end: CGPoint(x: 0, y: tile.minY), options: [])
        ctx.restoreGState()
        mark(ctx, in: tile)
    } else {
        // iPhone and iPad: the whole square, no transparency; the system rounds it.
        ctx.drawLinearGradient(gradient, start: CGPoint(x: 0, y: s), end: .zero, options: [])
        mark(ctx, in: CGRect(x: 0, y: 0, width: s, height: s))
    }
    return NSBitmapImageRep(cgImage: ctx.makeImage()!).representation(using: .png, properties: [:])!
}

let dir = URL(fileURLWithPath: "Support/Assets.xcassets/AppIcon.appiconset")
try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
try #"{"info":{"author":"xcode","version":1}}"#.write(
    to: dir.deletingLastPathComponent().appendingPathComponent("Contents.json"), atomically: true, encoding: .utf8)
var entries = [#"{"idiom":"universal","platform":"ios","size":"1024x1024","filename":"ios-1024.png"}"#]
try image(1024, mac: false).write(to: dir.appendingPathComponent("ios-1024.png"))
for (points, scale) in [(16, 1), (16, 2), (32, 1), (32, 2), (128, 1), (128, 2), (256, 1), (256, 2), (512, 1), (512, 2)] {
    let name = "mac-\(points)@\(scale)x.png"
    try image(points * scale, mac: true).write(to: dir.appendingPathComponent(name))
    entries.append(#"{"idiom":"mac","size":"\#(points)x\#(points)","scale":"\#(scale)x","filename":"\#(name)"}"#)
}
try ("{\"images\":[\n  " + entries.joined(separator: ",\n  ") + "\n],\"info\":{\"author\":\"xcode\",\"version\":1}}\n")
    .write(to: dir.appendingPathComponent("Contents.json"), atomically: true, encoding: .utf8)
print("wrote \(entries.count) icons to \(dir.path)")
