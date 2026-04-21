import Foundation

public enum VaporPaths {
  private static let privateDirectoryPermissions = 0o700
  private static let privateFilePermissions = 0o600

  public static let directoryEnvironmentKey = VaporConstants.Environment.vaporDirectory
  public static let environmentKey = VaporConstants.Environment.vaporEnvironment
  public static let useGitIgnoreEnvironmentKey = VaporConstants.Environment.useGitIgnore
  public static let useVaporIgnoreEnvironmentKey = VaporConstants.Environment.useVaporIgnore
  public static let localSyncDirectoryEnvironmentKey = VaporConstants.Environment.localSyncDirectory
  public static let cloudSyncDirectoryEnvironmentKey = VaporConstants.Environment.cloudSyncDirectory
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

  public static func prepareRuntimeDirectories(
    vaporDirectoryURL: URL,
    fileManager: FileManager = .default
  ) throws {
    try ensurePrivateDirectory(at: vaporDirectoryURL, fileManager: fileManager)
    try ensurePrivateDirectory(
      at: logsDirectoryURL(vaporDirectoryURL: vaporDirectoryURL),
      fileManager: fileManager
    )
    try ensurePrivateDirectory(
      at: stateDirectoryURL(vaporDirectoryURL: vaporDirectoryURL),
      fileManager: fileManager
    )
  }

  public static func ensurePrivateFile(
    at fileURL: URL,
    fileManager: FileManager = .default
  ) throws {
    let parentDirectoryURL = fileURL.deletingLastPathComponent()
    try ensurePrivateDirectory(at: parentDirectoryURL, fileManager: fileManager)

    if !fileManager.fileExists(atPath: fileURL.path) {
      fileManager.createFile(
        atPath: fileURL.path,
        contents: nil,
        attributes: [.posixPermissions: privateFilePermissions]
      )
    }
    try fileManager.setAttributes(
      [.posixPermissions: privateFilePermissions], ofItemAtPath: fileURL.path)
  }

  public static func ensurePrivateDirectory(
    at directoryURL: URL,
    fileManager: FileManager = .default
  ) throws {
    try fileManager.createDirectory(
      at: directoryURL,
      withIntermediateDirectories: true,
      attributes: [.posixPermissions: privateDirectoryPermissions]
    )
    try fileManager.setAttributes(
      [.posixPermissions: privateDirectoryPermissions],
      ofItemAtPath: directoryURL.path
    )
  }
}
