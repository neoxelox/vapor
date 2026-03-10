import Foundation

public enum VaporBuildInfo {
  public static let version = resolveVersion()
  public static let releaseVersion = releaseVersion(from: version)
  public static let buildVersion = resolveBuildVersion()
  public static let gitCommitShort = resolveInfoValue(for: "VaporGitCommit")

  public static var displayVersion: String {
    if let gitCommitShort, !gitCommitShort.isEmpty {
      return "\(version) (\(gitCommitShort))"
    }

    return version
  }

  static func releaseVersion(from version: String) -> String {
    String(
      version.split(separator: "-", maxSplits: 1, omittingEmptySubsequences: true).first
        ?? Substring(version))
  }

  private static func resolveVersion() -> String {
    if let packagedVersion = resolveInfoValue(for: "VaporVersion"), !packagedVersion.isEmpty {
      return packagedVersion
    }

    if let fallbackVersion = loadVersionFromRepository(), !fallbackVersion.isEmpty {
      return fallbackVersion
    }

    return "0.1.0"
  }

  private static func resolveBuildVersion() -> String {
    if let bundledBuildVersion = resolveInfoValue(for: "CFBundleVersion"),
      !bundledBuildVersion.isEmpty
    {
      return bundledBuildVersion
    }

    return releaseVersion
  }

  private static func resolveInfoValue(for key: String) -> String? {
    Bundle.main.object(forInfoDictionaryKey: key) as? String
  }

  private static func loadVersionFromRepository() -> String? {
    let versionURL = repositoryRootURL().appendingPathComponent("VERSION")
    guard let contents = try? String(contentsOf: versionURL, encoding: .utf8) else {
      return nil
    }

    let version = contents.trimmingCharacters(in: .whitespacesAndNewlines)
    return version.isEmpty ? nil : version
  }

  private static func repositoryRootURL() -> URL {
    var url = URL(fileURLWithPath: #filePath)
    for _ in 0..<5 {
      url.deleteLastPathComponent()
    }
    return url
  }
}
