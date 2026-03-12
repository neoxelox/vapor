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

@Test
func prepareRuntimeDirectoriesAppliesRestrictivePermissions() throws {
  let fileManager = FileManager.default
  let tempRoot = fileManager.temporaryDirectory
    .appendingPathComponent("vapor-paths-tests")
    .appendingPathComponent(UUID().uuidString, isDirectory: true)
  defer { try? fileManager.removeItem(at: tempRoot) }

  try VaporPaths.prepareRuntimeDirectories(vaporDirectoryURL: tempRoot, fileManager: fileManager)
  try VaporPaths.ensurePrivateFile(
    at: VaporPaths.configurationFileURL(vaporDirectoryURL: tempRoot),
    fileManager: fileManager
  )

  #expect(posixPermissions(for: tempRoot, fileManager: fileManager) == 0o700)
  #expect(
    posixPermissions(
      for: VaporPaths.logsDirectoryURL(vaporDirectoryURL: tempRoot), fileManager: fileManager)
      == 0o700)
  #expect(
    posixPermissions(
      for: VaporPaths.stateDirectoryURL(vaporDirectoryURL: tempRoot), fileManager: fileManager)
      == 0o700)
  #expect(
    posixPermissions(
      for: VaporPaths.configurationFileURL(vaporDirectoryURL: tempRoot),
      fileManager: fileManager
    ) == 0o600)
}

private func posixPermissions(for url: URL, fileManager: FileManager) -> NSNumber? {
  (try? fileManager.attributesOfItem(atPath: url.path)[.posixPermissions]) as? NSNumber
}
