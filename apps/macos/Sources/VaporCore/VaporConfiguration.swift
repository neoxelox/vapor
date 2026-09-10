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
  public var useGitIgnore: Bool
  public var useVaporIgnore: Bool
  public var localSyncDirectory: String
  /// The cloud root, or `nil` while the file leaves it to the provider's
  /// default (the daemon resolves that; the app never writes one).
  public var cloudSyncDirectory: String?
  public var preIgnoreRules: String
  public var postIgnoreRules: String
  public var languageCode: String
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

  public static let defaultPreIgnoreRuleLines = VaporConstants.Defaults.preIgnoreRuleLines
  public static let defaultLocalSyncDirectory = VaporConstants.Defaults.localSyncDirectory
  public static let defaultPreIgnoreRules = VaporConstants.Defaults.preIgnoreRules
  public static let defaultPostIgnoreRules = VaporConstants.Defaults.postIgnoreRules
  public static let defaultLanguageCode = VaporConstants.Defaults.languageCode

  public init(
    autoLaunch: Bool = VaporConstants.Defaults.autoLaunch,
    useGitIgnore: Bool = VaporConstants.Defaults.useGitIgnore,
    useVaporIgnore: Bool = VaporConstants.Defaults.useVaporIgnore,
    localSyncDirectory: String = defaultLocalSyncDirectory,
    cloudSyncDirectory: String? = nil,
    preIgnoreRules: String = defaultPreIgnoreRules,
    postIgnoreRules: String = defaultPostIgnoreRules,
    languageCode: String = defaultLanguageCode,
    timelineLimit: Int = VaporConstants.Defaults.timelineLimit,
    deviceId: String? = nil,
    additionalKeys: [String: JSONValue] = [:]
  ) {
    self.autoLaunch = autoLaunch
    self.useGitIgnore = useGitIgnore
    self.useVaporIgnore = useVaporIgnore
    self.localSyncDirectory = localSyncDirectory
    self.cloudSyncDirectory = cloudSyncDirectory
    self.preIgnoreRules = preIgnoreRules
    self.postIgnoreRules = postIgnoreRules
    self.languageCode = Self.normalizedLanguageCode(languageCode)
    self.timelineLimit = timelineLimit
    self.deviceId = deviceId
    self.additionalKeys = additionalKeys
  }

  enum CodingKeys: String, CodingKey, CaseIterable {
    case autoLaunch
    case useGitIgnore
    case useVaporIgnore
    case localSyncDirectory
    case cloudSyncDirectory
    case preIgnoreRules
    case postIgnoreRules
    case languageCode
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
    self.init(
      autoLaunch: try container.decodeIfPresent(Bool.self, forKey: .autoLaunch)
        ?? VaporConstants.Defaults.autoLaunch,
      useGitIgnore: try container.decodeIfPresent(Bool.self, forKey: .useGitIgnore)
        ?? VaporConstants.Defaults.useGitIgnore,
      useVaporIgnore: try container.decodeIfPresent(Bool.self, forKey: .useVaporIgnore)
        ?? VaporConstants.Defaults.useVaporIgnore,
      localSyncDirectory: try container.decodeIfPresent(String.self, forKey: .localSyncDirectory)
        ?? Self.defaultLocalSyncDirectory,
      cloudSyncDirectory: try container.decodeIfPresent(String.self, forKey: .cloudSyncDirectory),
      preIgnoreRules: try container.decodeIfPresent(String.self, forKey: .preIgnoreRules)
        ?? Self.defaultPreIgnoreRules,
      postIgnoreRules: try container.decodeIfPresent(String.self, forKey: .postIgnoreRules)
        ?? Self.defaultPostIgnoreRules,
      languageCode: try container.decodeIfPresent(String.self, forKey: .languageCode)
        ?? Self.defaultLanguageCode,
      timelineLimit: try container.decodeIfPresent(Int.self, forKey: .timelineLimit)
        ?? VaporConstants.Defaults.timelineLimit,
      deviceId: try container.decodeIfPresent(String.self, forKey: .deviceId),
      additionalKeys: additionalKeys
    )
  }

  public func encode(to encoder: any Encoder) throws {
    var container = encoder.container(keyedBy: CodingKeys.self)
    try container.encode(autoLaunch, forKey: .autoLaunch)
    try container.encode(useGitIgnore, forKey: .useGitIgnore)
    try container.encode(useVaporIgnore, forKey: .useVaporIgnore)
    try container.encode(localSyncDirectory, forKey: .localSyncDirectory)
    try container.encodeIfPresent(cloudSyncDirectory, forKey: .cloudSyncDirectory)
    try container.encode(preIgnoreRules, forKey: .preIgnoreRules)
    try container.encode(postIgnoreRules, forKey: .postIgnoreRules)
    try container.encode(languageCode, forKey: .languageCode)
    try container.encode(timelineLimit, forKey: .timelineLimit)
    // The daemon owns device-id generation; the app writes the key only
    // when one already exists.
    try container.encodeIfPresent(deviceId, forKey: .deviceId)
    var dynamic = encoder.container(keyedBy: DynamicCodingKey.self)
    for (key, value) in additionalKeys {
      try dynamic.encode(value, forKey: DynamicCodingKey(stringValue: key))
    }
  }

  private static func normalizedLanguageCode(_ languageCode: String) -> String {
    let normalized = languageCode.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
    return normalized.isEmpty ? defaultLanguageCode : normalized
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

    encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
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
      return VaporConfigurationLoadResult(
        configuration: try decoder.decode(VaporConfiguration.self, from: data),
        issue: nil
      )
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

  public func save(_ configuration: VaporConfiguration) throws {
    let vaporDirectoryURL = resolveVaporDirectoryURL()

    try VaporPaths.prepareRuntimeDirectories(
      vaporDirectoryURL: vaporDirectoryURL,
      fileManager: fileManager
    )

    let configurationURL = VaporPaths.configurationFileURL(vaporDirectoryURL: vaporDirectoryURL)

    // Merge onto a FRESH read of the on-disk file rather than writing the
    // caller's (possibly stale) snapshot. The app loads its configuration
    // once at launch, so a naive write would clobber daemon/CLI-owned keys
    // that changed since: `deviceId` (persisted by the daemon) and every
    // key the app does not model — `provider`, `syncMode`, `profiles`,
    // `resourceLimits`, `idleBoost` (carried in `additionalKeys`).
    var merged = configuration
    if let onDisk = decodeOnDisk(at: configurationURL) {
      merged.additionalKeys = onDisk.additionalKeys
      if let diskDeviceId = onDisk.deviceId {
        merged.deviceId = diskDeviceId
      }
    }

    let data = try encoder.encode(merged)
    try data.write(to: configurationURL, options: .atomic)
    try VaporPaths.ensurePrivateFile(at: configurationURL, fileManager: fileManager)

    logger.info(
      "Persisted vapor configuration",
      metadata: ["config_path": configurationURL.path]
    )
  }

  /// Best-effort decode of the current on-disk configuration (nil on
  /// missing/unreadable/corrupt file), used to preserve daemon-owned keys
  /// across an app-side save.
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
