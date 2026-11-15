import Foundation

/// Minimal JSON tree used to round-trip `vapor.json` keys the app does
/// not model first-class (profiles, resource budgets, provider config
/// owned by the Rust runtime). Mirrors AGENTS.md §8.6: the Rust side is
/// the source of truth; the app must never destroy keys it does not
/// understand.
public enum JSONValue: Codable, Equatable, Sendable {
  case null
  case bool(Bool)
  case number(Double)
  case string(String)
  case array([JSONValue])
  case object([String: JSONValue])

  public init(from decoder: any Decoder) throws {
    let container = try decoder.singleValueContainer()
    if container.decodeNil() {
      self = .null
    } else if let value = try? container.decode(Bool.self) {
      self = .bool(value)
    } else if let value = try? container.decode(Double.self) {
      self = .number(value)
    } else if let value = try? container.decode(String.self) {
      self = .string(value)
    } else if let value = try? container.decode([JSONValue].self) {
      self = .array(value)
    } else if let value = try? container.decode([String: JSONValue].self) {
      self = .object(value)
    } else {
      throw DecodingError.dataCorruptedError(
        in: container,
        debugDescription: "Unsupported JSON value"
      )
    }
  }

  public func encode(to encoder: any Encoder) throws {
    var container = encoder.singleValueContainer()
    switch self {
    case .null: try container.encodeNil()
    case .bool(let value): try container.encode(value)
    case .number(let value): try container.encode(value)
    case .string(let value): try container.encode(value)
    case .array(let value): try container.encode(value)
    case .object(let value): try container.encode(value)
    }
  }
}

public struct VaporConfiguration: Codable, Equatable, Sendable {
  public var autoLaunch: Bool
  public var useGitignore: Bool
  public var useVaporignore: Bool
  /// The two roots of the profile the top-level keys describe, or
  /// `nil` while the user has not set them. Neither has a default: a
  /// profile exists only when the user chose both, so the app never
  /// writes a root it did not read.
  public var localRoot: String?
  public var cloudRoot: String?
  /// Ignore rules in `.gitignore` syntax, one rule per entry.
  public var preIgnoreRules: [String]
  public var postIgnoreRules: [String]
  public var language: String
  public var timelineLimit: Int
  /// Stable per-device identifier. Generated and persisted by
  /// the daemon; the app only preserves and displays it, so `nil`
  /// simply means the daemon has not run yet.
  public var deviceId: String?
  /// Top-level `vapor.json` keys the app does not model (for example
  /// `provider`, `syncMode`, `profiles`, `resourceLimits`,
  /// `idleBoost`). Preserved verbatim across load/save so an app-side
  /// settings write can never destroy runtime configuration.
  public var additionalKeys: [String: JSONValue]
  /// Modelled keys whose value had the wrong type and fell back to the
  /// default, the way the daemon reads the file: one bad value never
  /// costs the rest of the configuration. Not written back.
  public var invalidKeys: [String] = []

  /// Every key the app models and may write. `deviceId` is the daemon's.
  public static let modelledKeys: Set<String> = Set(
    CodingKeys.allCases.map(\.stringValue).filter { $0 != VaporConstants.ConfigKeys.deviceId })

  /// The configured provider kind. The app does not model `provider`
  /// as a first-class field (it lives in `additionalKeys` so app-side
  /// saves preserve it verbatim); this read-only view feeds the UI.
  public var providerKind: String {
    if case .string(let value) = additionalKeys[VaporConstants.ConfigKeys.provider],
      !value.trimmingCharacters(in: .whitespaces).isEmpty
    {
      return value
    }
    return VaporConstants.Provider.defaultKind
  }

  public static let defaultPreIgnoreRules = VaporConstants.Defaults.preIgnoreRules
  public static let defaultPostIgnoreRules = VaporConstants.Defaults.postIgnoreRules
  public static let defaultLanguage = VaporConstants.Defaults.language

  /// The settings editor shows one rule per line.
  public static func editorText(for rules: [String]) -> String {
    rules.joined(separator: "\n")
  }

  /// The rules an editor text holds: one per line, trimmed, blank
  /// lines dropped.
  public static func rules(fromEditorText text: String) -> [String] {
    text
      .split(whereSeparator: \.isNewline)
      .map { $0.trimmingCharacters(in: .whitespaces) }
      .filter { !$0.isEmpty }
  }

  public init(
    autoLaunch: Bool = VaporConstants.Defaults.autoLaunch,
    useGitignore: Bool = VaporConstants.Defaults.useGitignore,
    useVaporignore: Bool = VaporConstants.Defaults.useVaporignore,
    localRoot: String? = nil,
    cloudRoot: String? = nil,
    preIgnoreRules: [String] = defaultPreIgnoreRules,
    postIgnoreRules: [String] = defaultPostIgnoreRules,
    language: String = defaultLanguage,
    timelineLimit: Int = VaporConstants.Defaults.timelineLimit,
    deviceId: String? = nil,
    additionalKeys: [String: JSONValue] = [:]
  ) {
    self.autoLaunch = autoLaunch
    self.useGitignore = useGitignore
    self.useVaporignore = useVaporignore
    self.localRoot = localRoot
    self.cloudRoot = cloudRoot
    self.preIgnoreRules = preIgnoreRules
    self.postIgnoreRules = postIgnoreRules
    self.language = Self.normalizedLanguage(language)
    self.timelineLimit = timelineLimit
    self.deviceId = deviceId
    self.additionalKeys = additionalKeys
  }

  enum CodingKeys: String, CodingKey, CaseIterable {
    case autoLaunch
    case useGitignore
    case useVaporignore
    case localRoot
    case cloudRoot
    case preIgnoreRules
    case postIgnoreRules
    case language
    case timelineLimit
    case deviceId
  }

  /// Free-form key for the unknown-key passthrough containers.
  private struct DynamicCodingKey: CodingKey {
    let stringValue: String
    var intValue: Int? { nil }
    init(stringValue: String) { self.stringValue = stringValue }
    init?(intValue: Int) { nil }
  }

  public init(from decoder: any Decoder) throws {
    let container = try decoder.container(keyedBy: CodingKeys.self)
    // Everything the struct does not model is captured verbatim so a
    // later save cannot destroy runtime-owned configuration.
    let knownKeys = Set(CodingKeys.allCases.map(\.stringValue))
    let dynamic = try decoder.container(keyedBy: DynamicCodingKey.self)
    var additionalKeys: [String: JSONValue] = [:]
    for key in dynamic.allKeys where !knownKeys.contains(key.stringValue) {
      additionalKeys[key.stringValue] = try dynamic.decode(JSONValue.self, forKey: key)
    }
    var invalidKeys: [String] = []
    func value<T: Decodable>(_ key: CodingKeys) -> T? {
      do {
        return try container.decodeIfPresent(T.self, forKey: key)
      } catch {
        invalidKeys.append(key.stringValue)
        return nil
      }
    }
    self.init(
      autoLaunch: value(.autoLaunch) ?? VaporConstants.Defaults.autoLaunch,
      useGitignore: value(.useGitignore) ?? VaporConstants.Defaults.useGitignore,
      useVaporignore: value(.useVaporignore) ?? VaporConstants.Defaults.useVaporignore,
      localRoot: value(.localRoot),
      cloudRoot: value(.cloudRoot),
      preIgnoreRules: value(.preIgnoreRules) ?? Self.defaultPreIgnoreRules,
      postIgnoreRules: value(.postIgnoreRules) ?? Self.defaultPostIgnoreRules,
      language: value(.language) ?? Self.defaultLanguage,
      timelineLimit: value(.timelineLimit) ?? VaporConstants.Defaults.timelineLimit,
      deviceId: value(.deviceId),
      additionalKeys: additionalKeys
    )
    self.invalidKeys = invalidKeys
  }

  public func encode(to encoder: any Encoder) throws {
    var container = encoder.container(keyedBy: CodingKeys.self)
    try container.encode(autoLaunch, forKey: .autoLaunch)
    try container.encode(useGitignore, forKey: .useGitignore)
    try container.encode(useVaporignore, forKey: .useVaporignore)
    try container.encodeIfPresent(localRoot, forKey: .localRoot)
    try container.encodeIfPresent(cloudRoot, forKey: .cloudRoot)
    try container.encode(preIgnoreRules, forKey: .preIgnoreRules)
    try container.encode(postIgnoreRules, forKey: .postIgnoreRules)
    try container.encode(language, forKey: .language)
    try container.encode(timelineLimit, forKey: .timelineLimit)
    // The daemon owns device-id generation; the app writes the key only
    // when one already exists.
    try container.encodeIfPresent(deviceId, forKey: .deviceId)
    var dynamic = encoder.container(keyedBy: DynamicCodingKey.self)
    for (key, value) in additionalKeys {
      try dynamic.encode(value, forKey: DynamicCodingKey(stringValue: key))
    }
  }

  private static func normalizedLanguage(_ language: String) -> String {
    let normalized = language.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
    return normalized.isEmpty ? defaultLanguage : normalized
  }
}

public enum VaporConfigurationStoreError: Error, CustomStringConvertible {
  /// `vapor.json` exists but is not a JSON object the app can read.
  case unreadableFile(String)

  public var description: String {
    switch self {
    case .unreadableFile(let path):
      return "\(path) cannot be read; fix or remove it before changing settings"
    }
  }
}

public struct VaporConfigurationLoadIssue: Equatable, Sendable {
  public let configPath: String
  public let reason: String
}

public struct VaporConfigurationLoadResult: Equatable, Sendable {
  public let configuration: VaporConfiguration
  public let issue: VaporConfigurationLoadIssue?
}

public final class VaporConfigurationStore {
  private let fileManager: FileManager
  private let environment: [String: String]
  private let logger: StructuredLogger
  private let encoder = JSONEncoder()
  private let decoder = JSONDecoder()

  public init(
    fileManager: FileManager = .default,
    environment: [String: String] = ProcessInfo.processInfo.environment,
    logger: StructuredLogger = StructuredLogger(component: "config")
  ) {
    self.fileManager = fileManager
    self.environment = environment
    self.logger = logger

    encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
  }

  public func load() -> VaporConfiguration {
    loadResult().configuration
  }

  public func loadResult() -> VaporConfigurationLoadResult {
    let vaporDirectoryURL = resolveVaporDirectoryURL()
    let configurationURL = VaporPaths.configurationFileURL(vaporDirectoryURL: vaporDirectoryURL)

    guard fileManager.fileExists(atPath: configurationURL.path) else {
      return persistDefaultConfiguration(
        at: configurationURL,
        vaporDirectoryURL: vaporDirectoryURL
      )
    }

    do {
      let data = try Data(contentsOf: configurationURL)
      // An empty file is the defaults, as the daemon and the CLI read it.
      if Self.isBlank(data) {
        return VaporConfigurationLoadResult(configuration: VaporConfiguration(), issue: nil)
      }
      let configuration = try decoder.decode(VaporConfiguration.self, from: data)
      guard configuration.invalidKeys.isEmpty else {
        let keys = configuration.invalidKeys.map { "`\($0)`" }.joined(separator: ", ")
        logger.error(
          "Ignoring invalid vapor configuration values; using their defaults",
          metadata: ["config_path": configurationURL.path, "keys": keys]
        )
        return VaporConfigurationLoadResult(
          configuration: configuration,
          issue: VaporConfigurationLoadIssue(
            configPath: configurationURL.path,
            reason: "ignoring invalid \(keys); using the default"
          )
        )
      }
      return VaporConfigurationLoadResult(configuration: configuration, issue: nil)
    } catch {
      logger.error(
        "Failed to load vapor configuration; preserving existing file and using in-memory defaults",
        metadata: [
          "config_path": configurationURL.path,
          "error": String(describing: error),
        ]
      )
      return VaporConfigurationLoadResult(
        configuration: VaporConfiguration(),
        issue: VaporConfigurationLoadIssue(
          configPath: configurationURL.path,
          reason: String(describing: error)
        )
      )
    }
  }

  /// Seeds the default configuration on first launch with an *exclusive*
  /// create (no fileExists→write TOCTOU): if a concurrently-starting
  /// daemon wins the create race, adopt the file it wrote rather than
  /// clobbering its `deviceId`/runtime-owned keys; a genuine write failure
  /// (read-only, full disk) is surfaced as a load issue instead of being
  /// swallowed by `try?` and reported as a clean, persisted config.
  private func persistDefaultConfiguration(
    at configurationURL: URL,
    vaporDirectoryURL: URL
  ) -> VaporConfigurationLoadResult {
    let defaultConfiguration = VaporConfiguration()
    do {
      try VaporPaths.prepareRuntimeDirectories(
        vaporDirectoryURL: vaporDirectoryURL,
        fileManager: fileManager
      )
      let data = try encoder.encode(defaultConfiguration)
      try data.write(to: configurationURL, options: .withoutOverwriting)
      try VaporPaths.ensurePrivateFile(at: configurationURL, fileManager: fileManager)
      return VaporConfigurationLoadResult(configuration: defaultConfiguration, issue: nil)
    } catch let error as CocoaError where error.code == .fileWriteFileExists {
      // Lost the exclusive-create race: adopt the winner's file.
      if let onDisk = decodeOnDisk(at: configurationURL) {
        return VaporConfigurationLoadResult(configuration: onDisk, issue: nil)
      }
      return VaporConfigurationLoadResult(configuration: defaultConfiguration, issue: nil)
    } catch {
      logger.error(
        "Failed to seed default vapor configuration on first launch",
        metadata: [
          "config_path": configurationURL.path,
          "error": String(describing: error),
        ]
      )
      return VaporConfigurationLoadResult(
        configuration: defaultConfiguration,
        issue: VaporConfigurationLoadIssue(
          configPath: configurationURL.path,
          reason: String(describing: error)
        )
      )
    }
  }

  /// Writes `keys` from `configuration` onto `vapor.json` as it is now,
  /// under the lock every writer of the file takes. Every other key
  /// keeps the value on disk: settings changed with `vapor config set`
  /// or `vapor service` since the app read the file, keys the app does
  /// not model, and values it could not read. The daemon owns
  /// `deviceId`, so it is never written here. A file that does not parse
  /// is the user's to fix and is never overwritten.
  public func save(_ configuration: VaporConfiguration, keys: Set<String>) throws {
    let vaporDirectoryURL = resolveVaporDirectoryURL()
    try VaporPaths.prepareRuntimeDirectories(
      vaporDirectoryURL: vaporDirectoryURL,
      fileManager: fileManager
    )
    let configurationURL = VaporPaths.configurationFileURL(vaporDirectoryURL: vaporDirectoryURL)
    try withConfigurationLock(configurationURL) {
      var document = try readDocument(at: configurationURL)
      let encoded = try decoder.decode(
        [String: JSONValue].self, from: try encoder.encode(configuration))
      for key in keys where key != VaporConstants.ConfigKeys.deviceId {
        document[key] = encoded[key]
      }
      var data = try encoder.encode(document)
      data.append(contentsOf: Array("\n".utf8))
      try data.write(to: configurationURL, options: .atomic)
      try VaporPaths.ensurePrivateFile(at: configurationURL, fileManager: fileManager)
    }
    logger.info(
      "Persisted vapor configuration",
      metadata: [
        "config_path": configurationURL.path,
        "keys": keys.sorted().joined(separator: ","),
      ]
    )
  }

  /// The file as a JSON object: empty when it is missing or blank.
  private func readDocument(at configurationURL: URL) throws -> [String: JSONValue] {
    guard fileManager.fileExists(atPath: configurationURL.path) else {
      return [:]
    }
    let data = try Data(contentsOf: configurationURL)
    if Self.isBlank(data) {
      return [:]
    }
    guard let document = try? decoder.decode([String: JSONValue].self, from: data) else {
      throw VaporConfigurationStoreError.unreadableFile(configurationURL.path)
    }
    return document
  }

  /// Runs `body` holding the advisory lock on `vapor.json.lock`, the
  /// same lock the CLI, the daemon, and `vapor service` take (`flock`).
  private func withConfigurationLock(_ configurationURL: URL, _ body: () throws -> Void) throws {
    let lockPath = configurationURL.path + ".lock"
    let descriptor = open(lockPath, O_CREAT | O_WRONLY, 0o600)
    guard descriptor >= 0 else {
      throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
    }
    defer { close(descriptor) }
    guard flock(descriptor, LOCK_EX) == 0 else {
      throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
    }
    defer { flock(descriptor, LOCK_UN) }
    try body()
  }

  private static func isBlank(_ data: Data) -> Bool {
    String(decoding: data, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
      .isEmpty
  }

  /// Best-effort decode of the current on-disk configuration (nil on a
  /// missing, unreadable, or corrupt file), used to adopt the file a
  /// concurrently starting daemon wrote first.
  private func decodeOnDisk(at configurationURL: URL) -> VaporConfiguration? {
    guard let data = try? Data(contentsOf: configurationURL) else {
      return nil
    }
    return try? decoder.decode(VaporConfiguration.self, from: data)
  }

  public func resolveVaporDirectoryURL() -> URL {
    return VaporPaths.resolveVaporDirectoryURL(
      environment: environment,
      fileManager: fileManager
    )
  }
}
