import AppKit
import Testing
import VaporCore

@testable import Vapor

@Test
func menuBarIconLoadsAsATemplateImageAtMenuBarPointSize() throws {
  let image = try #require(MenuBarIcon.load())

  #expect(image.isTemplate)
  #expect(image.size == NSSize(width: 22, height: 18))

  let pixelSizes = Set(image.representations.map { "\($0.pixelsWide)x\($0.pixelsHigh)" })
  #expect(pixelSizes == ["22x18", "44x36"])
}

@Test
func attentionIconKeepsTheMarkShapeInTheBrandColourAndIsNotATemplate() throws {
  let template = try #require(MenuBarIcon.load())

  let attention = MenuBarIcon.attention(from: template)

  #expect(!attention.isTemplate)
  #expect(attention.size == template.size)
  let pixelSizes = Set(attention.representations.map { "\($0.pixelsWide)x\($0.pixelsHigh)" })
  #expect(pixelSizes == ["22x18", "44x36"])

  for representation in attention.representations {
    let bitmap = try #require(representation as? NSBitmapImageRep)
    let templateBitmap = try #require(
      template.representations.first {
        $0.pixelsWide == bitmap.pixelsWide && $0.pixelsHigh == bitmap.pixelsHigh
      } as? NSBitmapImageRep)
    var opaquePixels = 0
    for y in 0..<bitmap.pixelsHigh {
      for x in 0..<bitmap.pixelsWide {
        let pixel = try #require(bitmap.colorAt(x: x, y: y)).usingColorSpace(.sRGB)!
        let source = try #require(templateBitmap.colorAt(x: x, y: y))
        // The shape is the template's alpha; the colour is the brand's.
        #expect(abs(pixel.alphaComponent - source.alphaComponent) < 0.02)
        if pixel.alphaComponent > 0.9 {
          opaquePixels += 1
          #expect(pixel.redComponent > 0.95)
          #expect(abs(pixel.greenComponent - VaporConstants.Brand.primaryColorGreen) < 0.05)
          #expect(pixel.blueComponent < 0.05)
        }
      }
    }
    #expect(opaquePixels > 0, "the mark must have fully covered pixels to tint")
  }
}
