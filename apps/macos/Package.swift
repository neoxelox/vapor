// swift-tools-version: 6.2

import PackageDescription

let package = Package(
  name: "Vapor",
  platforms: [.macOS("26.0")],
  products: [
    .executable(name: "Vapor", targets: ["Vapor"]),
    .library(name: "VaporCore", targets: ["VaporCore"]),
  ],
  targets: [
    .target(name: "VaporCore"),
    .executableTarget(name: "Vapor", dependencies: ["VaporCore"]),
    .testTarget(name: "VaporCoreTests", dependencies: ["VaporCore"]),
  ]
)
