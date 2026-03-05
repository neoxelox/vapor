import Foundation

public struct VaporConfiguration: Codable, Equatable, Sendable {
  public var autoLaunchEnabled: Bool
  public var useGitIgnore: Bool
  public var timelineEventLimit: Int

  public init(
    autoLaunchEnabled: Bool = true,
    useGitIgnore: Bool = true,
    timelineEventLimit: Int = 1000
  ) {
    self.autoLaunchEnabled = autoLaunchEnabled
    self.useGitIgnore = useGitIgnore
    self.timelineEventLimit = timelineEventLimit
  }
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
    let vaporDirectoryURL = resolveVaporDirectoryURL()
    let configurationURL = VaporPaths.configurationFileURL(vaporDirectoryURL: vaporDirectoryURL)

    guard fileManager.fileExists(atPath: configurationURL.path) else {
      let defaultConfiguration = VaporConfiguration()
      try? save(defaultConfiguration)
      return defaultConfiguration
    }

    do {
      let data = try Data(contentsOf: configurationURL)
      return try decoder.decode(VaporConfiguration.self, from: data)
    } catch {
      logger.error(
        "Failed to load vapor configuration; falling back to defaults",
        metadata: [
          "config_path": configurationURL.path,
          "error": String(describing: error),
        ]
      )
      let fallbackConfiguration = VaporConfiguration()
      try? save(fallbackConfiguration)
      return fallbackConfiguration
    }
  }

  public func save(_ configuration: VaporConfiguration) throws {
    let vaporDirectoryURL = resolveVaporDirectoryURL()

    try fileManager.createDirectory(at: vaporDirectoryURL, withIntermediateDirectories: true)
    try fileManager.createDirectory(
      at: VaporPaths.logsDirectoryURL(vaporDirectoryURL: vaporDirectoryURL),
      withIntermediateDirectories: true
    )
    try fileManager.createDirectory(
      at: VaporPaths.stateDirectoryURL(vaporDirectoryURL: vaporDirectoryURL),
      withIntermediateDirectories: true
    )

    let data = try encoder.encode(configuration)
    let configurationURL = VaporPaths.configurationFileURL(vaporDirectoryURL: vaporDirectoryURL)
    try data.write(to: configurationURL, options: .atomic)

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
