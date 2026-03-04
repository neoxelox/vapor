// swift-tools-version: 6.2

import PackageDescription

let package = Package(
  name: "VaporMacOS",
  platforms: [.macOS(.v15)],
  products: [
    .executable(name: "VaporApp", targets: ["VaporApp"]),
    .library(name: "VaporAppCore", targets: ["VaporAppCore"]),
  ],
  targets: [
    .target(name: "VaporAppCore"),
    .executableTarget(name: "VaporApp", dependencies: ["VaporAppCore"]),
    .testTarget(name: "VaporAppCoreTests", dependencies: ["VaporAppCore"]),
  ]
)
