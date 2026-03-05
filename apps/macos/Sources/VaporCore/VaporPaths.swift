import Foundation

public enum VaporPaths {
  public static let directoryEnvironmentKey = "VAPOR_DIR"
  public static let persistedDirectoryDefaultsKey = "vapor.user-config.vapor-directory-path"

  public static func resolveVaporDirectoryURL(
    environment: [String: String] = ProcessInfo.processInfo.environment,
    fileManager: FileManager = .default,
    persistedPath: String? = nil
  ) -> URL {
    if let configuredByEnvironment = normalizedDirectoryURL(
      pathString: environment[directoryEnvironmentKey],
      fileManager: fileManager
    ) {
      return configuredByEnvironment
    }

    let resolvedPersistedPath =
      persistedPath
      ?? UserDefaults.standard.string(forKey: persistedDirectoryDefaultsKey)

    if let resolvedPersistedPath,
      let configuredByUserSettings = normalizedDirectoryURL(
        pathString: resolvedPersistedPath,
        fileManager: fileManager
      )
    {
      return configuredByUserSettings
    }

    if environment["XCTestConfigurationFilePath"] != nil
      || environment["CI"] != nil
      || environment["VAPOR_LOCAL_DEV"] == "1"
    {
      return URL(fileURLWithPath: fileManager.currentDirectoryPath, isDirectory: true)
        .appendingPathComponent(".vapor", isDirectory: true)
    }

    return fileManager.homeDirectoryForCurrentUser
      .appendingPathComponent(".vapor", isDirectory: true)
  }

  public static func logsDirectoryURL(vaporDirectoryURL: URL) -> URL {
    vaporDirectoryURL.appendingPathComponent("logs", isDirectory: true)
  }

  public static func stateDirectoryURL(vaporDirectoryURL: URL) -> URL {
    vaporDirectoryURL.appendingPathComponent("state", isDirectory: true)
  }

  public static func configurationFileURL(vaporDirectoryURL: URL) -> URL {
    vaporDirectoryURL.appendingPathComponent("vapor.json", isDirectory: false)
  }

  public static func sqliteDatabaseURL(vaporDirectoryURL: URL) -> URL {
    stateDirectoryURL(vaporDirectoryURL: vaporDirectoryURL)
      .appendingPathComponent("vapor.sqlite", isDirectory: false)
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
