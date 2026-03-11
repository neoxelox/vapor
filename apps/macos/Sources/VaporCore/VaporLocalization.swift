import Foundation

public struct VaporLocalizedCatalog: Equatable, Sendable {
  public let effectiveLanguageCode: String
  public let availableLanguageCodes: [String]

  private let values: [String: String]
  private let fallbackValues: [String: String]

  init(
    effectiveLanguageCode: String,
    availableLanguageCodes: [String],
    values: [String: String],
    fallbackValues: [String: String]
  ) {
    self.effectiveLanguageCode = effectiveLanguageCode
    self.availableLanguageCodes = availableLanguageCodes
    self.values = values
    self.fallbackValues = fallbackValues
  }

  public func text(_ key: String) -> String {
    values[key] ?? fallbackValues[key] ?? key
  }

  public func formatted(_ key: String, _ arguments: CVarArg...) -> String {
    formatted(key, arguments)
  }

  public func formatted(_ key: String, _ arguments: [CVarArg]) -> String {
    String(
      format: text(key), locale: Locale(identifier: effectiveLanguageCode), arguments: arguments)
  }
}

public final class VaporLocalizationStore {
  private static let swiftPackageResourceBundleName = "Vapor_VaporCore.bundle"

  private let bundle: Bundle
  private let fileManager: FileManager

  public init(
    bundle: Bundle? = nil,
    fileManager: FileManager = .default
  ) {
    self.bundle = bundle ?? Self.resolveDefaultBundle(fileManager: fileManager)
    self.fileManager = fileManager
  }

  private static func resolveDefaultBundle(fileManager: FileManager) -> Bundle {
    let candidateBundles = loadedBundleCandidates()
    for candidate in candidateBundles {
      if hasDefaultCatalog(in: candidate) {
        return candidate
      }
    }

    if let discovered = discoverResourceBundle(
      searchRoots: resourceSearchRoots(for: candidateBundles),
      fileManager: fileManager
    ) {
      return discovered
    }

    return Bundle.main
  }

  static func discoverResourceBundle(searchRoots: [URL], fileManager: FileManager) -> Bundle? {
    for root in uniqueSearchRoots(searchRoots) {
      let directCandidate = root.appendingPathComponent(
        swiftPackageResourceBundleName,
        isDirectory: true
      )
      if let bundle = Bundle(url: directCandidate), hasDefaultCatalog(in: bundle) {
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
        if let bundle = Bundle(url: candidate), hasDefaultCatalog(in: bundle) {
          return bundle
        }
      }
    }

    return nil
  }

  private static func loadedBundleCandidates() -> [Bundle] {
    [Bundle.main] + Bundle.allBundles + Bundle.allFrameworks
  }

  private static func resourceSearchRoots(for bundles: [Bundle]) -> [URL] {
    bundles.flatMap { bundle in
      searchRoots(around: bundle)
    }
  }

  private static func uniqueSearchRoots(_ searchRoots: [URL]) -> [URL] {
    var seenPaths = Set<String>()
    return searchRoots.filter { root in
      seenPaths.insert(root.standardizedFileURL.path).inserted
    }
  }

  private static func searchRoots(around bundle: Bundle) -> [URL] {
    [bundle.bundleURL, bundle.resourceURL, bundle.executableURL?.deletingLastPathComponent()]
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

  private static func hasDefaultCatalog(in bundle: Bundle) -> Bool {
    let scopedURL = bundle.url(
      forResource: VaporConstants.Localization.defaultLanguageCode,
      withExtension: "json",
      subdirectory: VaporConstants.Localization.localesSubdirectory
    )
    let rootURL = bundle.url(
      forResource: VaporConstants.Localization.defaultLanguageCode,
      withExtension: "json"
    )

    return scopedURL != nil || rootURL != nil
  }

  public func resolve(languageCode: String) -> VaporLocalizedCatalog {
    let availableLanguageCodes = discoverAvailableLanguageCodes()
    let selectedLanguageCode = resolveLanguageCode(
      requestedLanguageCode: languageCode,
      availableLanguageCodes: availableLanguageCodes
    )

    let selectedValues = loadCatalog(languageCode: selectedLanguageCode)
    let fallbackLanguageCode = VaporConstants.Localization.defaultLanguageCode
    let fallbackValues: [String: String]
    if selectedLanguageCode == fallbackLanguageCode {
      fallbackValues = selectedValues
    } else {
      fallbackValues = loadCatalog(languageCode: fallbackLanguageCode)
    }

    return VaporLocalizedCatalog(
      effectiveLanguageCode: selectedLanguageCode,
      availableLanguageCodes: availableLanguageCodes,
      values: selectedValues,
      fallbackValues: fallbackValues
    )
  }

  private func discoverAvailableLanguageCodes() -> [String] {
    guard let localesURL = catalogDirectoryURL(for: bundle) else {
      return [VaporConstants.Localization.defaultLanguageCode]
    }

    guard
      let files = try? fileManager.contentsOfDirectory(
        at: localesURL, includingPropertiesForKeys: nil)
    else {
      return [VaporConstants.Localization.defaultLanguageCode]
    }

    let languageCodes =
      files
      .filter { $0.pathExtension.lowercased() == "json" }
      .map { $0.deletingPathExtension().lastPathComponent.lowercased() }
      .sorted()

    if languageCodes.isEmpty {
      return [VaporConstants.Localization.defaultLanguageCode]
    }

    return languageCodes
  }

  private func resolveLanguageCode(
    requestedLanguageCode: String,
    availableLanguageCodes: [String]
  ) -> String {
    var candidates: [String] = []
    candidates.append(contentsOf: languageCandidates(requestedLanguageCode))
    candidates.append(VaporConstants.Localization.defaultLanguageCode)

    for candidate in candidates {
      if availableLanguageCodes.contains(candidate) {
        return candidate
      }
    }

    return availableLanguageCodes.first ?? VaporConstants.Localization.defaultLanguageCode
  }

  private func languageCandidates(_ rawLanguageCode: String) -> [String] {
    let normalized =
      rawLanguageCode
      .trimmingCharacters(in: .whitespacesAndNewlines)
      .lowercased()
    guard !normalized.isEmpty else {
      return []
    }

    let base = normalized.split(separator: "-").first.map(String.init)
    if let base, base != normalized {
      return [normalized, base]
    }
    return [normalized]
  }

  private func loadCatalog(languageCode: String) -> [String: String] {
    let scopedURL = bundle.url(
      forResource: languageCode,
      withExtension: "json",
      subdirectory: VaporConstants.Localization.localesSubdirectory
    )
    let rootURL = bundle.url(forResource: languageCode, withExtension: "json")

    guard
      let url = scopedURL ?? rootURL,
      let data = try? Data(contentsOf: url),
      let catalog = try? JSONDecoder().decode([String: String].self, from: data)
    else {
      return [:]
    }

    return catalog
  }

  private func catalogDirectoryURL(for bundle: Bundle) -> URL? {
    guard let resourceURL = bundle.resourceURL else {
      return nil
    }

    let scopedURL = resourceURL.appendingPathComponent(
      VaporConstants.Localization.localesSubdirectory)
    var isDirectory = ObjCBool(false)
    if fileManager.fileExists(atPath: scopedURL.path, isDirectory: &isDirectory),
      isDirectory.boolValue
    {
      return scopedURL
    }

    return resourceURL
  }
}
