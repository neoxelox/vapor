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
func loggerRedactsExpandedInlineSecretShapesAcrossAuthProviders() throws {
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

  let redactedInlineSecrets = [
    "response body contains access_token=abc123",
    "replied Set-Cookie: session=xyz",
    "captured api_key=secret-123",
    "request header X-Api-Key: keep-private",
    "config sent client_secret=shhh",
    "upstream returned refresh_token=rotate-me",
    "header authorization: basic abc",
    "cached id_token=value",
  ]
  for message in redactedInlineSecrets {
    logger.info(message)
  }
  logger.info(
    "routine metadata redaction",
    metadata: [
      "api_key": "should-be-redacted",
      "ClientSecret": "also-redacted",
      "Refresh-Token": "also-redacted",
      "safe_counter": "42",
    ]
  )

  let logURL = VaporPaths.logsDirectoryURL(vaporDirectoryURL: vaporDirectoryURL)
    .appendingPathComponent(VaporPaths.appLogFileName)
  waitForLogWrites()
  let logContents = try String(contentsOf: logURL, encoding: .utf8)

  #expect(!logContents.contains("abc123"))
  #expect(!logContents.contains("xyz"))
  #expect(!logContents.contains("secret-123"))
  #expect(!logContents.contains("keep-private"))
  #expect(!logContents.contains("shhh"))
  #expect(!logContents.contains("rotate-me"))
  #expect(!logContents.contains("basic abc"))
  #expect(!logContents.contains("id_token=value"))
  #expect(!logContents.contains("should-be-redacted"))
  #expect(!logContents.contains("also-redacted"))
  #expect(logContents.contains("safe_counter=42"))
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

@Test
func loggerRecreatesTheLogFileAfterItIsDeletedMidRun() throws {
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
  let logURL = VaporPaths.logsDirectoryURL(vaporDirectoryURL: vaporDirectoryURL)
    .appendingPathComponent(VaporPaths.appLogFileName)

  // init prepared the file but cached no handle yet. Delete it (as
  // `./scripts/clean.sh` / user cleanup would) so the first write takes
  // the no-cached-handle fallback path against a missing file.
  #expect(fileManager.fileExists(atPath: logURL.path))
  try fileManager.removeItem(at: logURL)

  logger.info("after deletion")
  waitForLogWrites()

  #expect(
    fileManager.fileExists(atPath: logURL.path),
    "the log file must be recreated rather than silently dropping every subsequent line"
  )
  let recreated = try String(contentsOf: logURL, encoding: .utf8)
  #expect(recreated.contains("after deletion"))
}

private func waitForLogWrites() {
  StructuredLogger.writeQueue.sync {}
}

private func posixPermissions(for url: URL, fileManager: FileManager) -> NSNumber? {
  (try? fileManager.attributesOfItem(atPath: url.path)[.posixPermissions]) as? NSNumber
}
