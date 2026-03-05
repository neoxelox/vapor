import Foundation

public struct VaporUserConfiguration: Codable, Equatable, Sendable {
  public var vaporDirectoryPath: String
  public var useGitIgnore: Bool
  public var timelineEventLimit: Int

  public init(
    vaporDirectoryPath: String,
    useGitIgnore: Bool = true,
    timelineEventLimit: Int = 1000
  ) {
    self.vaporDirectoryPath = vaporDirectoryPath
    self.useGitIgnore = useGitIgnore
    self.timelineEventLimit = timelineEventLimit
  }
}

public final class VaporUserConfigurationStore {
  public static let vaporDirectoryDefaultsKey = VaporPaths.persistedDirectoryDefaultsKey

  private let fileManager: FileManager
  private let environment: [String: String]
  private let defaults: UserDefaults
  private let defaultsDirectoryKey: String
  private let logger: StructuredLogger
  private let encoder = JSONEncoder()
  private let decoder = JSONDecoder()

  public init(
    fileManager: FileManager = .default,
    environment: [String: String] = ProcessInfo.processInfo.environment,
    defaults: UserDefaults = .standard,
    defaultsDirectoryKey: String = vaporDirectoryDefaultsKey,
    logger: StructuredLogger = StructuredLogger(component: "user-config")
  ) {
    self.fileManager = fileManager
    self.environment = environment
    self.defaults = defaults
    self.defaultsDirectoryKey = defaultsDirectoryKey
    self.logger = logger

    encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
  }

  public func load() -> VaporUserConfiguration {
    let vaporDirectoryURL = resolveVaporDirectoryURL()
    let configurationURL = VaporPaths.configurationFileURL(vaporDirectoryURL: vaporDirectoryURL)

    guard fileManager.fileExists(atPath: configurationURL.path) else {
      let defaultConfiguration = VaporUserConfiguration(vaporDirectoryPath: vaporDirectoryURL.path)
      try? save(defaultConfiguration)
      return defaultConfiguration
    }

    do {
      let data = try Data(contentsOf: configurationURL)
      var configuration = try decoder.decode(VaporUserConfiguration.self, from: data)
      configuration.vaporDirectoryPath = vaporDirectoryURL.path
      defaults.set(vaporDirectoryURL.path, forKey: defaultsDirectoryKey)
      return configuration
    } catch {
      logger.error(
        "Failed to load vapor user configuration; falling back to defaults",
        metadata: [
          "config_path": configurationURL.path,
          "error": String(describing: error),
        ]
      )
      let fallbackConfiguration = VaporUserConfiguration(vaporDirectoryPath: vaporDirectoryURL.path)
      try? save(fallbackConfiguration)
      return fallbackConfiguration
    }
  }

  public func save(_ configuration: VaporUserConfiguration) throws {
    let vaporDirectoryURL =
      VaporPaths.normalizedDirectoryURL(
        pathString: configuration.vaporDirectoryPath,
        fileManager: fileManager
      ) ?? resolveVaporDirectoryURL()

    let normalizedConfiguration = VaporUserConfiguration(
      vaporDirectoryPath: vaporDirectoryURL.path,
      useGitIgnore: configuration.useGitIgnore,
      timelineEventLimit: configuration.timelineEventLimit
    )

    try fileManager.createDirectory(at: vaporDirectoryURL, withIntermediateDirectories: true)
    try fileManager.createDirectory(
      at: VaporPaths.logsDirectoryURL(vaporDirectoryURL: vaporDirectoryURL),
      withIntermediateDirectories: true
    )
    try fileManager.createDirectory(
      at: VaporPaths.stateDirectoryURL(vaporDirectoryURL: vaporDirectoryURL),
      withIntermediateDirectories: true
    )

    let data = try encoder.encode(normalizedConfiguration)
    let configurationURL = VaporPaths.configurationFileURL(vaporDirectoryURL: vaporDirectoryURL)
    try data.write(to: configurationURL, options: .atomic)

    defaults.set(vaporDirectoryURL.path, forKey: defaultsDirectoryKey)
    logger.info(
      "Persisted vapor user configuration",
      metadata: ["config_path": configurationURL.path]
    )
  }

  public func resolveVaporDirectoryURL() -> URL {
    let persistedPath = defaults.string(forKey: defaultsDirectoryKey)
    return VaporPaths.resolveVaporDirectoryURL(
      environment: environment,
      fileManager: fileManager,
      persistedPath: persistedPath
    )
  }
}
