import AppKit
import SwiftUI
import VaporCore

/// The status item image: the monochrome Vapor mark from
/// `assets/macos/menubar`, mirrored into the resource bundle by
/// `scripts/swift/resources.sh`. The file name ends in `Template`, so AppKit
/// pairs the 1x and 2x files into one image, marks it as a template, and
/// tints it for the current menu bar appearance.
enum MenuBarIcon {
  static let resourceName = "VaporMenuBarTemplate"

  /// The SF Symbol shown when the resource bundle lacks the mark, which only
  /// happens in a checkout where the resource sync has not run.
  static let fallbackSystemImage = "wind"

  static func load() -> NSImage? {
    let bundle = VaporResourceBundle.resolve(containing: contains(_:))
    return bundle.image(forResource: resourceName)
  }

  private static func contains(_ bundle: Bundle) -> Bool {
    bundle.url(forResource: resourceName, withExtension: "png") != nil
  }
}

struct MenuBarLabel: View {
  let title: String
  let icon: NSImage?

  var body: some View {
    if let icon {
      Label {
        Text(title)
      } icon: {
        Image(nsImage: icon)
          .renderingMode(.template)
      }
    } else {
      Label(title, systemImage: MenuBarIcon.fallbackSystemImage)
    }
  }
}
