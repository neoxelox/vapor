import Foundation

public enum VaporPaths {
  public static let directoryEnvironmentKey = VaporConstants.Environment.vaporDirectory
  public static let environmentKey = VaporConstants.Environment.vaporEnvironment
  public static let useGitIgnoreEnvironmentKey = VaporConstants.Environment.useGitIgnore
  public static let useVaporIgnoreEnvironmentKey = VaporConstants.Environment.useVaporIgnore
  public static let syncDirectoriesEnvironmentKey = VaporConstants.Environment.syncDirectories
  public static let preIgnoreRulesEnvironmentKey = VaporConstants.Environment.preIgnoreRules
  public static let postIgnoreRulesEnvironmentKey = VaporConstants.Environment.postIgnoreRules

  public static let logsDirectoryName = VaporConstants.Runtime.logsDirectoryName
  public static let stateDirectoryName = VaporConstants.Runtime.stateDirectoryName
  public static let configurationFileName = VaporConstants.Runtime.configurationFileName
  public static let sqliteDatabaseFileName = VaporConstants.Runtime.sqliteDatabaseFileName
  public static let appLogFileName = VaporConstants.Runtime.appLogFileName
  public static let daemonLogFileName = VaporConstants.Runtime.daemonLogFileName

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
        .appendingPathComponent(VaporConstants.Runtime.vaporDirectoryName, isDirectory: true)
    }

    return fileManager.homeDirectoryForCurrentUser
      .appendingPathComponent(VaporConstants.Runtime.vaporDirectoryName, isDirectory: true)
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
