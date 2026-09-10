import AppKit
import SwiftUI
import VaporCore

/// Vapor's primary colour for AppKit and SwiftUI consumers. The value
/// itself lives in `VaporConstants.Brand`; nothing in the app spells
/// the components a second time.
extension NSColor {
  static let vaporPrimary = NSColor(
    srgbRed: VaporConstants.Brand.primaryColorRed,
    green: VaporConstants.Brand.primaryColorGreen,
    blue: VaporConstants.Brand.primaryColorBlue,
    alpha: 1
  )
}

extension Color {
  static let vaporPrimary = Color(nsColor: .vaporPrimary)
}
