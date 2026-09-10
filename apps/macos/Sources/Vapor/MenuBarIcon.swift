import AppKit
import SwiftUI
import VaporCore

/// The status item image: the monochrome Vapor mark from
/// `assets/macos/menubar`, mirrored into the resource bundle by
/// `scripts/swift/resources.sh`. The file name ends in `Template`, so AppKit
/// pairs the 1x and 2x files into one image, marks it as a template, and
/// tints it for the current menu bar appearance.
///
/// The mark has a second face for the moments Vapor is waiting on the
/// user (`AppShellState.needsUserAction`): the same shape filled with the
/// brand colour. That variant is deliberately not a template, so AppKit
/// leaves the colour alone whatever the menu bar appearance.
enum MenuBarIcon {
  static let resourceName = "VaporMenuBarTemplate"

  /// The SF Symbol shown when the resource bundle lacks the mark, which only
  /// happens in a checkout where the resource sync has not run.
  static let fallbackSystemImage = "wind"

  /// `nil` when the resource is missing or holds no pixels (a truncated
  /// file), so the caller falls back to the system symbol instead of
  /// showing an empty status item.
  static func load() -> NSImage? {
    let bundle = VaporResourceBundle.resolve(containing: contains(_:))
    guard let image = bundle.image(forResource: resourceName), !image.representations.isEmpty
    else {
      return nil
    }
    return image
  }

  /// Both faces of the mark, or `nil` when the resource is missing.
  static func loadSet() -> MenuBarIconSet? {
    guard let template = load() else {
      return nil
    }
    return MenuBarIconSet(template: template, attention: attention(from: template))
  }

  /// The mark's shape filled with `fill`, rasterised once per scale the
  /// template carries so the status item gets crisp 1x and 2x pixels.
  static func attention(from template: NSImage, fill: NSColor = .vaporPrimary) -> NSImage {
    let image = NSImage(size: template.size)
    image.isTemplate = false
    for representation in template.representations {
      guard
        let bitmap = NSBitmapImageRep(
          bitmapDataPlanes: nil,
          pixelsWide: representation.pixelsWide,
          pixelsHigh: representation.pixelsHigh,
          bitsPerSample: 8,
          samplesPerPixel: 4,
          hasAlpha: true,
          isPlanar: false,
          colorSpaceName: .deviceRGB,
          bytesPerRow: 0,
          bitsPerPixel: 0
        ),
        let context = NSGraphicsContext(bitmapImageRep: bitmap)
      else {
        continue
      }
      let pixelBounds = NSRect(
        x: 0, y: 0, width: representation.pixelsWide, height: representation.pixelsHigh)
      NSGraphicsContext.saveGraphicsState()
      NSGraphicsContext.current = context
      representation.draw(in: pixelBounds)
      fill.set()
      pixelBounds.fill(using: .sourceAtop)
      NSGraphicsContext.restoreGraphicsState()
      // The point size is what pairs a 44 × 36 bitmap with the 22 × 18
      // mark as its 2x scale.
      bitmap.size = template.size
      image.addRepresentation(bitmap)
    }
    return image
  }

  private static func contains(_ bundle: Bundle) -> Bool {
    bundle.url(forResource: resourceName, withExtension: "png") != nil
  }
}

struct MenuBarIconSet {
  /// Tinted by AppKit for the menu bar appearance.
  let template: NSImage
  /// The brand-coloured face shown while Vapor needs the user.
  let attention: NSImage
}

struct MenuBarLabel: View {
  let title: String
  let icons: MenuBarIconSet?
  let needsUserAction: Bool

  var body: some View {
    if let icons {
      Label {
        Text(title)
      } icon: {
        if needsUserAction {
          Image(nsImage: icons.attention)
            .renderingMode(.original)
        } else {
          Image(nsImage: icons.template)
            .renderingMode(.template)
        }
      }
    } else {
      Label(title, systemImage: MenuBarIcon.fallbackSystemImage)
    }
  }
}
