import Foundation

public enum VaporBundleLayout {
  public static let appExecutableName = "Vapor"
  public static let daemonExecutableName = "vapord"
  public static let cliExecutableName = "vapor"
  public static let executablesDirectoryRelativePath = "Contents/MacOS"
  /// The `vapor` CLI cannot live in `Contents/MacOS/` — the default
  /// macOS filesystem is case-insensitive, so `vapor` would collide
  /// with the `Vapor` app executable. It ships in `Contents/Helpers/`
  /// (the conventional home for bundled helper tools) instead.
  public static let helpersDirectoryRelativePath = "Contents/Helpers"
  public static let appExecutableRelativePath =
    "\(executablesDirectoryRelativePath)/\(appExecutableName)"
  public static let daemonExecutableRelativePath =
    "\(executablesDirectoryRelativePath)/\(daemonExecutableName)"
  public static let cliExecutableRelativePath =
    "\(helpersDirectoryRelativePath)/\(cliExecutableName)"

  public enum ValidationError: Error, Equatable {
    case missingExecutable(path: String)
    case executableNotExecutable(path: String)
  }

  public static func bundledDaemonExecutableURL(bundleURL: URL, executableURL: URL?) -> URL {
    if let executableURL {
      return executableURL.deletingLastPathComponent().appendingPathComponent(daemonExecutableName)
    }

    return bundleURL.appendingPathComponent(daemonExecutableRelativePath)
  }

  /// The bundled `vapor` CLI at `Contents/Helpers/vapor`. The CLI in
  /// turn resolves `vapord` from `../MacOS/vapord` relative to itself,
  /// so the whole chain stays bundle-local.
  public static func bundledCLIExecutableURL(bundleURL: URL, executableURL: URL?) -> URL {
    if let executableURL {
      return
        executableURL
        .deletingLastPathComponent()  // Contents/MacOS
        .deletingLastPathComponent()  // Contents
        .appendingPathComponent("Helpers", isDirectory: true)
        .appendingPathComponent(cliExecutableName)
    }

    return bundleURL.appendingPathComponent(cliExecutableRelativePath)
  }

  public static func validateRequiredExecutables(
    in bundleURL: URL,
    fileManager: FileManager = .default
  ) throws {
    try validateExecutable(
      at: bundleURL.appendingPathComponent(appExecutableRelativePath),
      fileManager: fileManager
    )
    try validateExecutable(
      at: bundleURL.appendingPathComponent(daemonExecutableRelativePath),
      fileManager: fileManager
    )
    try validateExecutable(
      at: bundleURL.appendingPathComponent(cliExecutableRelativePath),
      fileManager: fileManager
    )
  }

  private static func validateExecutable(at url: URL, fileManager: FileManager) throws {
    var isDirectory = ObjCBool(false)
    guard fileManager.fileExists(atPath: url.path, isDirectory: &isDirectory),
      !isDirectory.boolValue
    else {
      throw ValidationError.missingExecutable(path: url.path)
    }

    guard fileManager.isExecutableFile(atPath: url.path) else {
      throw ValidationError.executableNotExecutable(path: url.path)
    }
  }
}
