import Foundation
import Testing

@testable import VaporCore

@Test
func resolveUsesEnglishCatalogByDefault() throws {
  try withTemporaryLocalizationStore(
    body: { store in
      let catalog = store.resolve(languageCode: "en")

      #expect(catalog.effectiveLanguageCode == "en")
      #expect(catalog.availableLanguageCodes.contains("en"))
      #expect(catalog.text("app_title") == "Vapor")
    }
  )
}

@Test
func resolveFallsBackToEnglishWhenOverrideLanguageMissing() throws {
  try withTemporaryLocalizationStore(
    body: { store in
      let catalog = store.resolve(languageCode: "es")

      #expect(catalog.effectiveLanguageCode == "en")
      #expect(catalog.text("settings_language") == "Language")
    }
  )
}

@Test
func missingTranslationKeyFallsBackToKeyName() throws {
  try withTemporaryLocalizationStore(
    body: { store in
      let catalog = store.resolve(languageCode: "en")

      #expect(catalog.text("unknown_key") == "unknown_key")
    }
  )
}

@Test
func discoverResourceBundleFindsSwiftPackageBundleByExactName() throws {
  let fileManager = FileManager.default
  let rootURL = fileManager.temporaryDirectory.appendingPathComponent(
    UUID().uuidString, isDirectory: true)
  defer {
    try? fileManager.removeItem(at: rootURL)
  }

  let bundleURL = rootURL.appendingPathComponent("Vapor_VaporCore.bundle", isDirectory: true)
  try fileManager.createDirectory(at: bundleURL, withIntermediateDirectories: true)
  try Data("{\"app_title\":\"Temp Vapor\"}".utf8).write(
    to: bundleURL.appendingPathComponent("en.json"))

  let bundle = VaporLocalizationStore.discoverResourceBundle(
    searchRoots: [rootURL],
    fileManager: fileManager
  )

  #expect(bundle?.bundleURL == bundleURL)
}

@Test
func discoverResourceBundleFindsLocalizedCatalogsInAlternateVaporCoreBundle() throws {
  let fileManager = FileManager.default
  let rootURL = fileManager.temporaryDirectory.appendingPathComponent(
    UUID().uuidString, isDirectory: true)
  defer {
    try? fileManager.removeItem(at: rootURL)
  }

  let bundleURL = rootURL.appendingPathComponent("CustomVaporCoreAssets.bundle", isDirectory: true)
  let localesURL = bundleURL.appendingPathComponent(
    VaporConstants.Localization.localesSubdirectory,
    isDirectory: true
  )
  try fileManager.createDirectory(at: localesURL, withIntermediateDirectories: true)
  try Data("{\"app_title\":\"Temp Vapor\"}".utf8).write(
    to: localesURL.appendingPathComponent("en.json"))

  let bundle = try #require(
    VaporLocalizationStore.discoverResourceBundle(
      searchRoots: [rootURL],
      fileManager: fileManager
    )
  )
  let store = VaporLocalizationStore(bundle: bundle)

  let catalog = store.resolve(languageCode: "en")

  #expect(catalog.text("app_title") == "Temp Vapor")
}

private func withTemporaryLocalizationStore(
  catalogEntries: [String: String] = [
    "app_title": "Vapor",
    "settings_language": "Language",
  ],
  body: (VaporLocalizationStore) throws -> Void
) throws {
  let fileManager = FileManager.default
  let rootURL = fileManager.temporaryDirectory.appendingPathComponent(
    UUID().uuidString, isDirectory: true)
  defer {
    try? fileManager.removeItem(at: rootURL)
  }

  let bundleURL = rootURL.appendingPathComponent("Vapor_VaporCore.bundle", isDirectory: true)
  try fileManager.createDirectory(at: bundleURL, withIntermediateDirectories: true)
  let catalogData = try JSONEncoder().encode(catalogEntries)
  try catalogData.write(to: bundleURL.appendingPathComponent("en.json"))

  let bundle = try #require(Bundle(url: bundleURL))
  try body(VaporLocalizationStore(bundle: bundle))
}
