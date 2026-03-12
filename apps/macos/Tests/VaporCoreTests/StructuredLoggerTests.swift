import Foundation
import Testing

@testable import VaporCore

@Test
func parsesSupportedLogLevelsFromEnvironmentValues() {
  #expect(VaporLogLevel.from(environmentValue: "debug") == .debug)
  #expect(VaporLogLevel.from(environmentValue: "INFO") == .info)
  #expect(VaporLogLevel.from(environmentValue: "warning") == .warning)
  #expect(VaporLogLevel.from(environmentValue: "error") == .error)
  #expect(VaporLogLevel.from(environmentValue: "trace") == nil)
}

@Test
func loggerRedactsSensitiveMetadataAndAppliesPrivateLogPermissions() throws {
  let fileManager = FileManager.default
  let vaporDirectoryURL = fileManager.temporaryDirectory
    .appendingPathComponent("vapor-logger-tests")
    .appendingPathComponent(UUID().uuidString, isDirectory: true)
  defer { try? fileManager.removeItem(at: vaporDirectoryURL) }

  let logger = StructuredLogger(
    component: "tests",
    minLevel: .debug,
    environment: ["VAPOR_DIR": vaporDirectoryURL.path],
    fileManager: fileManager
  )

  logger.info(
    "Authorization: Bearer token",
    metadata: [
      "auth_token": "secret-value",
      "note": "Bearer abc123",
      "safe": "ok",
    ]
  )

  let logURL = VaporPaths.logsDirectoryURL(vaporDirectoryURL: vaporDirectoryURL)
    .appendingPathComponent(VaporPaths.appLogFileName)
  waitForLogWrites()
  let logContents = try String(contentsOf: logURL, encoding: .utf8)

  #expect(logContents.contains("[REDACTED]"))
  #expect(!logContents.contains("secret-value"))
  #expect(!logContents.contains("Bearer abc123"))
  #expect(posixPermissions(for: logURL, fileManager: fileManager) == 0o600)
  #expect(
    posixPermissions(for: logURL.deletingLastPathComponent(), fileManager: fileManager) == 0o700)
}

@Test
func loggerGracefullyFallsBackWhenConfiguredRuntimeRootIsUnusable() throws {
  let fileManager = FileManager.default
  let tempRoot = fileManager.temporaryDirectory
    .appendingPathComponent("vapor-logger-tests")
    .appendingPathComponent(UUID().uuidString, isDirectory: true)
  defer { try? fileManager.removeItem(at: tempRoot) }

  try fileManager.createDirectory(at: tempRoot, withIntermediateDirectories: true)
  let unusableRoot = tempRoot.appendingPathComponent("not-a-directory", isDirectory: false)
  fileManager.createFile(atPath: unusableRoot.path, contents: Data())

  let logger = StructuredLogger(
    component: "tests",
    minLevel: .debug,
    environment: ["VAPOR_DIR": unusableRoot.path],
    fileManager: fileManager
  )

  logger.info("still works")
  waitForLogWrites()

  let fallbackLogURL =
    unusableRoot
    .appendingPathComponent(VaporPaths.logsDirectoryName, isDirectory: true)
    .appendingPathComponent(VaporPaths.appLogFileName, isDirectory: false)
  #expect(!fileManager.fileExists(atPath: fallbackLogURL.path))
}

private func waitForLogWrites() {
  StructuredLogger.writeQueue.sync {}
}

private func posixPermissions(for url: URL, fileManager: FileManager) -> NSNumber? {
  (try? fileManager.attributesOfItem(atPath: url.path)[.posixPermissions]) as? NSNumber
}
