import Foundation
import Testing

@testable import VaporCore

@Test
func resolvesVaporDirectoryFromEnvironmentOverride() {
  let resolved = VaporPaths.resolveVaporDirectoryURL(
    environment: ["VAPOR_DIR": "/tmp/custom-vapor"],
    fileManager: .default
  )

  #expect(resolved.path == "/tmp/custom-vapor")
}

@Test
func resolvesVaporDirectoryToCurrentDirectoryForDevEnvironment() {
  let expected = URL(fileURLWithPath: FileManager.default.currentDirectoryPath, isDirectory: true)
    .appendingPathComponent(".vapor", isDirectory: true)

  let resolved = VaporPaths.resolveVaporDirectoryURL(
    environment: ["VAPOR_ENV": "dev"],
    fileManager: .default
  )

  #expect(resolved.path == expected.path)
}

@Test
func normalizesRelativeDirectoryPathsAgainstCurrentDirectory() {
  let expected = URL(fileURLWithPath: FileManager.default.currentDirectoryPath, isDirectory: true)
    .appendingPathComponent("nested/.vapor", isDirectory: true)
    .standardizedFileURL

  let normalized = VaporPaths.normalizedDirectoryURL(pathString: "nested/.vapor")

  #expect(normalized?.path == expected.path)
}
