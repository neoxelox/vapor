import Foundation

public enum VaporPaths {
  public static let directoryEnvironmentKey = "VAPOR_DIR"
  public static let environmentKey = "VAPOR_ENV"
  public static let useGitIgnoreEnvironmentKey = "VAPOR_USE_GITIGNORE"
  public static let useVaporIgnoreEnvironmentKey = "VAPOR_USE_VAPORIGNORE"
  public static let syncDirectoriesEnvironmentKey = "VAPOR_SYNC_DIRECTORIES"
  public static let preIgnoreRulesEnvironmentKey = "VAPOR_PRE_IGNORE_RULES"
  public static let postIgnoreRulesEnvironmentKey = "VAPOR_POST_IGNORE_RULES"

  public static let logsDirectoryName = "logs"
  public static let stateDirectoryName = "state"
  public static let configurationFileName = "vapor.json"
  public static let sqliteDatabaseFileName = "vapor.sqlite"
  public static let appLogFileName = "vapor.logs"
  public static let daemonLogFileName = "vapord.logs"

  public static func resolveVaporDirectoryURL(
    environment: [String: String] = ProcessInfo.processInfo.environment,
    fileManager: FileManager = .default
  ) -> URL {
    if let configuredByEnvironment = normalizedDirectoryURL(
      pathString: environment[directoryEnvironmentKey],
      fileManager: fileManager
    ) {
      return configuredByEnvironment
    }

    if environment["XCTestConfigurationFilePath"] != nil
      || environment["CI"] != nil
      || environment[environmentKey]?.lowercased() == "dev"
    {
      return URL(fileURLWithPath: fileManager.currentDirectoryPath, isDirectory: true)
        .appendingPathComponent(".vapor", isDirectory: true)
    }

    return fileManager.homeDirectoryForCurrentUser
      .appendingPathComponent(".vapor", isDirectory: true)
  }

  public static func logsDirectoryURL(vaporDirectoryURL: URL) -> URL {
    vaporDirectoryURL.appendingPathComponent(logsDirectoryName, isDirectory: true)
  }

  public static func stateDirectoryURL(vaporDirectoryURL: URL) -> URL {
    vaporDirectoryURL.appendingPathComponent(stateDirectoryName, isDirectory: true)
  }

  public static func configurationFileURL(vaporDirectoryURL: URL) -> URL {
    vaporDirectoryURL.appendingPathComponent(configurationFileName, isDirectory: false)
  }

  public static func sqliteDatabaseURL(vaporDirectoryURL: URL) -> URL {
    stateDirectoryURL(vaporDirectoryURL: vaporDirectoryURL)
      .appendingPathComponent(sqliteDatabaseFileName, isDirectory: false)
  }

  public static func normalizedDirectoryURL(
    pathString: String?,
    fileManager: FileManager = .default
  ) -> URL? {
    guard let rawPath = pathString?.trimmingCharacters(in: .whitespacesAndNewlines),
      !rawPath.isEmpty
    else {
      return nil
    }

    let expandedPath = (rawPath as NSString).expandingTildeInPath
    let configuredURL = URL(fileURLWithPath: expandedPath, isDirectory: true)

    if configuredURL.path.hasPrefix("/") {
      return configuredURL.standardizedFileURL
    }

    return URL(fileURLWithPath: fileManager.currentDirectoryPath, isDirectory: true)
      .appendingPathComponent(expandedPath, isDirectory: true)
      .standardizedFileURL
  }
}
