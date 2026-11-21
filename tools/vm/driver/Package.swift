// swift-tools-version: 6.0
import PackageDescription

// The Tier VM driver. It runs inside a test guest only and is never
// part of `Vapor.app`.
let package = Package(
  name: "vapor-vm-driver",
  platforms: [.macOS("26.0")],
  targets: [
    .executableTarget(
      name: "vapor-vm-driver",
      path: "Sources/VaporVMDriver",
      swiftSettings: [.swiftLanguageMode(.v5)]
    )
  ]
)
