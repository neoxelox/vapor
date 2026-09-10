// Optional macOS-only step: use SwiftUI itself to produce the legacy continuous mask.
// Run from the assets/ directory: swift source/tools/render-system-mask.swift
// Then: cd source/tools && npm install && npm run build
import Foundation
import SwiftUI
import CoreGraphics
import ImageIO
import UniformTypeIdentifiers

let output = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "source/masters/macos-system-mask-1024.png"
let size = 1024
let colorSpace = CGColorSpace(name: CGColorSpace.sRGB)!
guard let context = CGContext(data: nil, width: size, height: size, bitsPerComponent: 8,
                              bytesPerRow: size * 4, space: colorSpace,
                              bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else {
    fatalError("Cannot create image context")
}
context.clear(CGRect(x: 0, y: 0, width: size, height: size))
let outline = RoundedRectangle(cornerRadius: 185, style: .continuous)
    .path(in: CGRect(x: 100, y: 100, width: 824, height: 824))
context.addPath(outline.cgPath)
context.setFillColor(CGColor(gray: 1, alpha: 1))
context.fillPath()
guard let image = context.makeImage() else { fatalError("Cannot render mask") }
let url = URL(fileURLWithPath: output)
try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
guard let destination = CGImageDestinationCreateWithURL(url as CFURL, UTType.png.identifier as CFString, 1, nil) else {
    fatalError("Cannot create PNG destination")
}
CGImageDestinationAddImage(destination, image, nil)
guard CGImageDestinationFinalize(destination) else { fatalError("Cannot write mask") }
print("Rendered SwiftUI continuous mask: \(url.path)")
