import Dispatch
import Foundation

public enum VaporLogLevel: String, CaseIterable, Sendable {
  case debug
  case info
  case warning
  case error

  var priority: Int {
    switch self {
    case .debug:
      return 10
    case .info:
      return 20
    case .warning:
      return 30
    case .error:
      return 40
    }
  }

  static func from(environmentValue: String?) -> VaporLogLevel? {
    guard let environmentValue else {
      return nil
    }

    return VaporLogLevel(rawValue: environmentValue.lowercased())
  }
}

public final class StructuredLogger: @unchecked Sendable {
  static let writeQueue = DispatchQueue(label: "sh.arn.vapor.logging.writer")

  private let component: String
  private let minLevel: VaporLogLevel
  private let fileURL: URL
  private let fileManager: FileManager
  private var cachedFileHandle: FileHandle?

  public convenience init(
    component: String,
    minLevel: VaporLogLevel? = nil,
    fileManager: FileManager = .default
  ) {
    self.init(
      component: component,
      minLevel: minLevel,
      environment: ProcessInfo.processInfo.environment,
      fileManager: fileManager
    )
  }

  public init(
    component: String,
    minLevel: VaporLogLevel? = nil,
    environment: [String: String],
    fileManager: FileManager = .default
  ) {
    self.component = component
    self.fileManager = fileManager

    if let minLevel {
      self.minLevel = minLevel
    } else {
      self.minLevel =
        VaporLogLevel.from(environmentValue: environment[VaporConstants.Environment.vaporLogLevel])
        ?? StructuredLogger.defaultMinLevel(for: environment)
    }

    self.fileURL = StructuredLogger.logDirectory(environment: environment, fileManager: fileManager)
      .appendingPathComponent(VaporPaths.appLogFileName)

    prepareLogFile()
  }

  public func debug(_ message: String, metadata: [String: String] = [:]) {
    log(.debug, message: message, metadata: metadata)
  }

  public func info(_ message: String, metadata: [String: String] = [:]) {
    log(.info, message: message, metadata: metadata)
  }

  public func warning(_ message: String, metadata: [String: String] = [:]) {
    log(.warning, message: message, metadata: metadata)
  }

  public func error(_ message: String, metadata: [String: String] = [:]) {
    log(.error, message: message, metadata: metadata)
  }

  private func log(_ level: VaporLogLevel, message: String, metadata: [String: String]) {
    guard level.priority >= minLevel.priority else {
      return
    }

    let timestamp = Int64(Date().timeIntervalSince1970 * 1000)
    let levelLabel = level.rawValue.uppercased()
    let safeMessage = redactInlineSecrets(in: sanitize(message))

    let metadataSuffix: String
    if metadata.isEmpty {
      metadataSuffix = ""
    } else {
      let metadataParts = metadata.keys.sorted().map { key in
        let value = metadata[key, default: ""]
        return "\(sanitize(key))=\(sanitizeMetadataValue(key: key, value: value))"
      }
      metadataSuffix = metadataParts.isEmpty ? "" : " \(metadataParts.joined(separator: " "))"
    }

    let line =
      "\(timestamp) [\(levelLabel)] (\(sanitize(component))): \(safeMessage).\(metadataSuffix)\n"
    let data = Data(line.utf8)

    StructuredLogger.writeQueue.async { [weak self, fileURL, data] in
      guard let self else {
        return
      }

      if self.writeReusingCachedHandle(data: data) {
        return
      }

      // Recreate the file first: it may have been deleted mid-run (user
      // cleanup, `./scripts/clean.sh` in dev). `FileHandle(forWritingTo:)`
      // cannot create a missing file, so without this the write path bails
      // and every subsequent line is silently dropped until app restart.
      try? VaporPaths.ensurePrivateFile(at: fileURL, fileManager: self.fileManager)

      guard let fresh = try? FileHandle(forWritingTo: fileURL) else {
        self.cachedFileHandle = nil
        return
      }

      self.cachedFileHandle = fresh
      _ = self.writeReusingCachedHandle(data: data)
    }
  }

  private func writeReusingCachedHandle(data: Data) -> Bool {
    guard let handle = cachedFileHandle else {
      return false
    }

    do {
      try handle.seekToEnd()
      // No synchronize() here: fsyncing every log line is battery-hostile
      // for an "invisible-first" app, and losing the last few lines on a
      // power cut is an acceptable trade for a diagnostics log.
      try handle.write(contentsOf: data)
      return true
    } catch {
      cachedFileHandle = nil
      return false
    }
  }

  private func prepareLogFile() {
    StructuredLogger.writeQueue.sync {
      let directory = fileURL.deletingLastPathComponent()
      try? VaporPaths.prepareRuntimeDirectories(
        vaporDirectoryURL: directory.deletingLastPathComponent(),
        fileManager: fileManager
      )
      try? VaporPaths.ensurePrivateFile(at: fileURL, fileManager: fileManager)
    }
  }

  private static func logDirectory(environment: [String: String], fileManager: FileManager) -> URL {
    let vaporDirectoryURL = VaporPaths.resolveVaporDirectoryURL(
      environment: environment,
      fileManager: fileManager
    )
    return VaporPaths.logsDirectoryURL(vaporDirectoryURL: vaporDirectoryURL)
  }

  private static func defaultMinLevel(for environment: [String: String]) -> VaporLogLevel {
    if environment[VaporPaths.environmentKey]?.lowercased() == "dev" {
      return .debug
    }

    return .info
  }

  private func sanitize(_ raw: String) -> String {
    raw
      .replacingOccurrences(of: "\n", with: "\\n")
      .replacingOccurrences(of: "\r", with: "\\r")
      .replacingOccurrences(of: "\t", with: "\\t")
  }

  private func sanitizeMetadataValue(key: String, value: String) -> String {
    if isSensitiveKey(key) {
      return "[REDACTED]"
    }

    return redactInlineSecrets(in: sanitize(value))
  }

  private func isSensitiveKey(_ key: String) -> Bool {
    let normalized = key.lowercased()
    return StructuredLogger.sensitiveKeyMarkers.contains(where: { normalized.contains($0) })
  }

  private func redactInlineSecrets(in raw: String) -> String {
    let normalized = raw.lowercased()
    if StructuredLogger.inlineSecretMarkers.contains(where: { normalized.contains($0) }) {
      return "[REDACTED]"
    }

    return raw
  }

  static let sensitiveKeyMarkers: [String] = [
    "api_key",
    "apikey",
    "auth_header",
    "authorization",
    "client_secret",
    "cookie",
    "credential",
    "keychain",
    "oauth",
    "password",
    "refresh",
    "secret",
    "session",
    "token",
  ]

  static let inlineSecretMarkers: [String] = [
    "access_token=",
    "api_key=",
    "api-key:",
    "apikey=",
    "authorization:",
    "bearer ",
    "client_secret=",
    "id_token=",
    "password=",
    "refresh_token=",
    "secret=",
    "session=",
    "set-cookie:",
    "token=",
    "x-api-key:",
  ]
}
