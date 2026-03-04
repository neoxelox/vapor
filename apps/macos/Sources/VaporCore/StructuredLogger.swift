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
  private static let writeQueue = DispatchQueue(label: "sh.arn.vapor.logging.writer")

  #if VAPOR_PACKAGED_BUILD
    private static let buildDefaultLevel: VaporLogLevel = .warning
  #else
    private static let buildDefaultLevel: VaporLogLevel = .debug
  #endif

  private let component: String
  private let minLevel: VaporLogLevel
  private let fileURL: URL
  private let fileManager: FileManager

  public init(
    component: String,
    fileName: String = "vapor.log",
    minLevel: VaporLogLevel? = nil,
    fileManager: FileManager = .default
  ) {
    self.component = component
    self.fileManager = fileManager

    if let minLevel {
      self.minLevel = minLevel
    } else {
      self.minLevel =
        VaporLogLevel.from(environmentValue: ProcessInfo.processInfo.environment["VAPOR_LOG_LEVEL"])
        ?? StructuredLogger.buildDefaultLevel
    }

    let resolvedFileName = ProcessInfo.processInfo.environment["VAPOR_APP_LOG_FILE"] ?? fileName

    self.fileURL = StructuredLogger.logDirectory(fileManager: fileManager)
      .appendingPathComponent(resolvedFileName)

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
    let safeMessage = sanitize(message)

    let metadataSuffix: String
    if metadata.isEmpty {
      metadataSuffix = ""
    } else {
      let metadataParts = metadata.keys.sorted().map { key in
        let value = metadata[key, default: ""]
        return "\(sanitize(key))=\(sanitize(value))"
      }
      metadataSuffix = metadataParts.isEmpty ? "" : " \(metadataParts.joined(separator: " "))"
    }

    let line =
      "\(timestamp) [\(levelLabel)] (\(sanitize(component))): \(safeMessage).\(metadataSuffix)\n"
    let data = Data(line.utf8)

    StructuredLogger.writeQueue.async { [fileURL, data] in
      guard let fileHandle = try? FileHandle(forWritingTo: fileURL) else {
        return
      }

      defer {
        try? fileHandle.close()
      }

      fileHandle.seekToEndOfFile()
      fileHandle.write(data)
    }
  }

  private func prepareLogFile() {
    StructuredLogger.writeQueue.sync {
      let directory = fileURL.deletingLastPathComponent()
      try? fileManager.createDirectory(at: directory, withIntermediateDirectories: true)
      if !fileManager.fileExists(atPath: fileURL.path) {
        fileManager.createFile(atPath: fileURL.path, contents: nil)
      }
    }
  }

  private static func logDirectory(fileManager: FileManager) -> URL {
    if let configured = ProcessInfo.processInfo.environment["VAPOR_LOG_DIR"], !configured.isEmpty {
      return URL(fileURLWithPath: configured, isDirectory: true)
    }

    if let userLibraryURL = fileManager.urls(for: .libraryDirectory, in: .userDomainMask).first {
      return userLibraryURL.appendingPathComponent("Logs").appendingPathComponent("Vapor")
    }

    return fileManager.homeDirectoryForCurrentUser
      .appendingPathComponent("Library")
      .appendingPathComponent("Logs")
      .appendingPathComponent("Vapor")
  }

  private func sanitize(_ raw: String) -> String {
    raw
      .replacingOccurrences(of: "\n", with: "\\n")
      .replacingOccurrences(of: "\r", with: "\\r")
      .replacingOccurrences(of: "\t", with: "\\t")
  }
}
