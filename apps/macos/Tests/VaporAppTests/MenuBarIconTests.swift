import AppKit
import Testing

@testable import Vapor

@Test
func menuBarIconLoadsAsATemplateImageAtMenuBarPointSize() throws {
  let image = try #require(MenuBarIcon.load())

  #expect(image.isTemplate)
  #expect(image.size == NSSize(width: 22, height: 18))

  let pixelSizes = Set(image.representations.map { "\($0.pixelsWide)x\($0.pixelsHigh)" })
  #expect(pixelSizes == ["22x18", "44x36"])
}
