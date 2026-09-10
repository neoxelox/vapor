import Foundation

/// Locates the SwiftPM resource bundle that carries the files mirrored from
/// `assets/` by `scripts/swift/resources.sh` (locale catalogs, the menu bar
/// image).
///
/// SwiftPM's generated `Bundle.module` traps when the bundle is not at the
/// path it expects, and the packaged `Vapor.app`, `swift run`, and
/// `swift test` each place it somewhere else. The bundle is found instead by
/// asking every loaded bundle, then searching the directories around them,
/// for a marker file the caller names.
public enum VaporResourceBundle {
  static let swiftPackageBundleName = "Vapor_VaporCore.bundle"

  /// Anchors the search on the bundle that holds this code (the app, a test
  /// bundle, or a bare executable), the same anchor SwiftPM's generated
  /// accessor uses. `Bundle.allBundles` and `Bundle.allFrameworks` return an
  /// incomplete snapshot the first time a process asks for them, so they
  /// cannot be the only source of search roots.
  private final class BundleFinder {}

  /// Returns the first bundle `marker` accepts, or the main bundle when none
  /// does, so a missing resource degrades to a lookup miss instead of a crash.
  public static func resolve(
    containing marker: (Bundle) -> Bool,
    fileManager: FileManager = .default
  ) -> Bundle {
    let candidates = loadedBundleCandidates()
    if let loaded = candidates.first(where: marker) {
      return loaded
    }

    if let discovered = discover(
      searchRoots: resourceSearchRoots(for: candidates),
      containing: marker,
      fileManager: fileManager
    ) {
      return discovered
    }

    return Bundle.main
  }

  static func discover(
    searchRoots: [URL],
    containing marker: (Bundle) -> Bool,
    fileManager: FileManager
  ) -> Bundle? {
    for root in uniqueSearchRoots(searchRoots) {
      let directCandidate = root.appendingPathComponent(
        swiftPackageBundleName,
        isDirectory: true
      )
      if let bundle = Bundle(url: directCandidate), marker(bundle) {
        return bundle
      }

      guard
        let candidates = try? fileManager.contentsOfDirectory(
          at: root,
          includingPropertiesForKeys: nil,
          options: [.skipsHiddenFiles]
        )
      else {
        continue
      }

      for candidate in candidates where candidate.pathExtension == "bundle" {
        guard candidate.lastPathComponent.lowercased().contains("vaporcore") else {
          continue
        }
        if let bundle = Bundle(url: candidate), marker(bundle) {
          return bundle
        }
      }
    }

    return nil
  }

  private static func loadedBundleCandidates() -> [Bundle] {
    [Bundle.main, Bundle(for: BundleFinder.self)] + Bundle.allBundles + Bundle.allFrameworks
  }

  private static func resourceSearchRoots(for bundles: [Bundle]) -> [URL] {
    bundles.flatMap { bundle in
      searchRoots(around: bundle)
    }
  }

  /// Foundation keeps every `Bundle` ever created in `allBundles`, and one
  /// whose directory has since been deleted answers `bundleURL` with nothing,
  /// which traps when bridged to the non-optional `URL`. The path string
  /// bridges a missing value to an empty string instead, so it is the safe
  /// way to ask.
  private static func directory(of bundle: Bundle) -> URL? {
    let path = bundle.bundlePath
    guard !path.isEmpty else {
      return nil
    }

    return URL(fileURLWithPath: path, isDirectory: true)
  }

  private static func uniqueSearchRoots(_ searchRoots: [URL]) -> [URL] {
    var seenPaths = Set<String>()
    return searchRoots.filter { root in
      seenPaths.insert(root.standardizedFileURL.path).inserted
    }
  }

  private static func searchRoots(around bundle: Bundle) -> [URL] {
    guard let bundleURL = directory(of: bundle) else {
      return []
    }

    return [bundleURL, bundle.resourceURL, bundle.executableURL?.deletingLastPathComponent()]
      .compactMap { $0 }
      .flatMap(searchRoots(around:))
  }

  private static func searchRoots(around url: URL) -> [URL] {
    var roots: [URL] = []
    var current = url.standardizedFileURL

    roots.append(current)
    for _ in 0..<3 {
      let parent = current.deletingLastPathComponent()
      guard parent.path != current.path else {
        break
      }

      roots.append(parent)
      current = parent
    }

    return roots
  }
}
