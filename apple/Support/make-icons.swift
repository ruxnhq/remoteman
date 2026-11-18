// Cuts remoteman's app icons from the masters in assets/icon (viewer.svg for the iPhone app,
// the iPad app and the Mac viewer; host.svg for the Mac host) into Support/Assets.xcassets.
// The SVGs are the source; nothing is redrawn here. Run from apple/: swift Support/make-icons.swift
import AppKit
import CoreGraphics

func image(_ master: NSImage, _ px: Int, mac: Bool) -> Data {
    let space = CGColorSpace(name: CGColorSpace.sRGB)!
    let ctx = CGContext(
        data: nil, width: px, height: px, bitsPerComponent: 8, bytesPerRow: 0, space: space,
        bitmapInfo: (mac ? CGImageAlphaInfo.premultipliedLast : CGImageAlphaInfo.noneSkipLast).rawValue)!
    ctx.interpolationQuality = .high
    let s = CGFloat(px)
    func draw(in r: CGRect) {
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = NSGraphicsContext(cgContext: ctx, flipped: false)
        master.draw(in: r, from: .zero, operation: .sourceOver, fraction: 1)
        NSGraphicsContext.restoreGraphicsState()
    }
    if mac {
        // A Mac icon is a rounded tile inside the canvas, with a soft shadow under it.
        let tile = CGRect(x: s * 100 / 1024, y: s * 100 / 1024, width: s * 824 / 1024, height: s * 824 / 1024)
        let shape = CGPath(roundedRect: tile, cornerWidth: s * 185 / 1024, cornerHeight: s * 185 / 1024, transform: nil)
        ctx.saveGState()
        ctx.setShadow(offset: CGSize(width: 0, height: -s * 10 / 1024), blur: s * 22 / 1024, color: CGColor(gray: 0, alpha: 0.32))
        ctx.addPath(shape)
        ctx.setFillColor(CGColor(gray: 0, alpha: 1))
        ctx.fillPath()
        ctx.restoreGState()
        ctx.saveGState()
        ctx.addPath(shape)
        ctx.clip()
        draw(in: tile)
        ctx.restoreGState()
    } else {
        // iPhone and iPad: the whole square, no transparency; the system rounds it.
        draw(in: CGRect(x: 0, y: 0, width: s, height: s))
    }
    return NSBitmapImageRep(cgImage: ctx.makeImage()!).representation(using: .png, properties: [:])!
}

func set(_ name: String, from svg: String, iOS: Bool) throws {
    guard let master = NSImage(contentsOf: URL(fileURLWithPath: svg)) else { fatalError("can't read \(svg)") }
    let dir = URL(fileURLWithPath: "Support/Assets.xcassets/\(name).appiconset")
    try? FileManager.default.removeItem(at: dir)
    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    var entries: [String] = []
    if iOS {
        entries.append(#"{"idiom":"universal","platform":"ios","size":"1024x1024","filename":"ios-1024.png"}"#)
        try image(master, 1024, mac: false).write(to: dir.appendingPathComponent("ios-1024.png"))
    }
    for (points, scale) in [(16, 1), (16, 2), (32, 1), (32, 2), (128, 1), (128, 2), (256, 1), (256, 2), (512, 1), (512, 2)] {
        let file = "mac-\(points)@\(scale)x.png"
        try image(master, points * scale, mac: true).write(to: dir.appendingPathComponent(file))
        entries.append(#"{"idiom":"mac","size":"\#(points)x\#(points)","scale":"\#(scale)x","filename":"\#(file)"}"#)
    }
    try ("{\"images\":[\n  " + entries.joined(separator: ",\n  ") + "\n],\"info\":{\"author\":\"xcode\",\"version\":1}}\n")
        .write(to: dir.appendingPathComponent("Contents.json"), atomically: true, encoding: .utf8)
    print("wrote \(entries.count) icons to \(dir.path)")
}

try FileManager.default.createDirectory(atPath: "Support/Assets.xcassets", withIntermediateDirectories: true)
try #"{"info":{"author":"xcode","version":1}}"#.write(toFile: "Support/Assets.xcassets/Contents.json", atomically: true, encoding: .utf8)
try set("AppIcon", from: "../assets/icon/viewer.svg", iOS: true)
try set("HostIcon", from: "../assets/icon/host.svg", iOS: false)
