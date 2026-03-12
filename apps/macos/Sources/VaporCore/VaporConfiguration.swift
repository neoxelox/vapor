import Foundation

public struct VaporConfiguration: Codable, Equatable, Sendable {
  public var autoLaunch: Bool
  public var useGitIgnore: Bool
  public var useVaporIgnore: Bool
  public var localSyncDirectory: String
  public var cloudSyncDirectory: String
  public var preIgnoreRules: String
  public var postIgnoreRules: String
  public var languageCode: String
  public var timelineEventLimit: Int

  public static let defaultPreIgnoreRuleLines = VaporConstants.Defaults.preIgnoreRuleLines
  public static let defaultLocalSyncDirectory = VaporConstants.Defaults.localSyncDirectory
  public static let defaultCloudSyncDirectory = VaporConstants.Defaults.cloudSyncDirectory
  public static let defaultPreIgnoreRules = VaporConstants.Defaults.preIgnoreRules
  public static let defaultPostIgnoreRules = VaporConstants.Defaults.postIgnoreRules
  public static let defaultLanguageCode = VaporConstants.Defaults.languageCode

  public init(
    autoLaunch: Bool = VaporConstants.Defaults.autoLaunch,
    useGitIgnore: Bool = VaporConstants.Defaults.useGitIgnore,
    useVaporIgnore: Bool = VaporConstants.Defaults.useVaporIgnore,
    localSyncDirectory: String = defaultLocalSyncDirectory,
    cloudSyncDirectory: String = defaultCloudSyncDirectory,
    preIgnoreRules: String = defaultPreIgnoreRules,
    postIgnoreRules: String = defaultPostIgnoreRules,
    languageCode: String = defaultLanguageCode,
    timelineEventLimit: Int = VaporConstants.Defaults.timelineEventLimit
  ) {
    self.autoLaunch = autoLaunch
    self.useGitIgnore = useGitIgnore
    self.useVaporIgnore = useVaporIgnore
    self.localSyncDirectory = localSyncDirectory
    self.cloudSyncDirectory = cloudSyncDirectory
    self.preIgnoreRules = preIgnoreRules
    self.postIgnoreRules = postIgnoreRules
    self.languageCode = Self.normalizedLanguageCode(languageCode)
    self.timelineEventLimit = timelineEventLimit
  }

  enum CodingKeys: String, CodingKey {
    case autoLaunch
    case useGitIgnore
    case useVaporIgnore
    case localSyncDirectory
    case cloudSyncDirectory
    case preIgnoreRules
    case postIgnoreRules
    case languageCode
    case timelineEventLimit
  }

  public init(from decoder: any Decoder) throws {
    let container = try decoder.container(keyedBy: CodingKeys.self)
    self.init(
      autoLaunch: try container.decodeIfPresent(Bool.self, forKey: .autoLaunch)
        ?? VaporConstants.Defaults.autoLaunch,
      useGitIgnore: try container.decodeIfPresent(Bool.self, forKey: .useGitIgnore)
        ?? VaporConstants.Defaults.useGitIgnore,
      useVaporIgnore: try container.decodeIfPresent(Bool.self, forKey: .useVaporIgnore)
        ?? VaporConstants.Defaults.useVaporIgnore,
      localSyncDirectory: try container.decodeIfPresent(String.self, forKey: .localSyncDirectory)
        ?? Self.defaultLocalSyncDirectory,
      cloudSyncDirectory: try container.decodeIfPresent(String.self, forKey: .cloudSyncDirectory)
        ?? Self.defaultCloudSyncDirectory,
      preIgnoreRules: try container.decodeIfPresent(String.self, forKey: .preIgnoreRules)
        ?? Self.defaultPreIgnoreRules,
      postIgnoreRules: try container.decodeIfPresent(String.self, forKey: .postIgnoreRules)
        ?? Self.defaultPostIgnoreRules,
      languageCode: try container.decodeIfPresent(String.self, forKey: .languageCode)
        ?? Self.defaultLanguageCode,
      timelineEventLimit: try container.decodeIfPresent(Int.self, forKey: .timelineEventLimit)
        ?? VaporConstants.Defaults.timelineEventLimit
    )
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
      let defaultConfiguration = VaporConfiguration()
      try? save(defaultConfiguration)
      return VaporConfigurationLoadResult(configuration: defaultConfiguration, issue: nil)
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

  public func save(_ configuration: VaporConfiguration) throws {
    let vaporDirectoryURL = resolveVaporDirectoryURL()

    try VaporPaths.prepareRuntimeDirectories(
      vaporDirectoryURL: vaporDirectoryURL,
      fileManager: fileManager
    )

    let data = try encoder.encode(configuration)
    let configurationURL = VaporPaths.configurationFileURL(vaporDirectoryURL: vaporDirectoryURL)
    try data.write(to: configurationURL, options: .atomic)
    try VaporPaths.ensurePrivateFile(at: configurationURL, fileManager: fileManager)

    logger.info(
      "Persisted vapor configuration",
      metadata: ["config_path": configurationURL.path]
    )
  }

  public func resolveVaporDirectoryURL() -> URL {
    return VaporPaths.resolveVaporDirectoryURL(
      environment: environment,
      fileManager: fileManager
    )
  }
}
