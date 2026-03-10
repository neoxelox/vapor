import Foundation

public enum VaporBundleLayout {
  public static let appExecutableName = "Vapor"
  public static let daemonExecutableName = "vapord"
  public static let executablesDirectoryRelativePath = "Contents/MacOS"
  public static let appExecutableRelativePath =
    "\(executablesDirectoryRelativePath)/\(appExecutableName)"
  public static let daemonExecutableRelativePath =
    "\(executablesDirectoryRelativePath)/\(daemonExecutableName)"

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
