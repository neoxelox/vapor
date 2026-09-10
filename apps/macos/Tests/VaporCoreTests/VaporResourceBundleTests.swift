import Foundation
import Testing

@testable import VaporCore

@Test
func discoverSkipsBundlesWithoutTheMarker() throws {
  let fileManager = FileManager.default
  let rootURL = fileManager.temporaryDirectory.appendingPathComponent(
    UUID().uuidString, isDirectory: true)
  defer {
    try? fileManager.removeItem(at: rootURL)
  }

  let localesOnlyURL = rootURL.appendingPathComponent("Vapor_VaporCore.bundle", isDirectory: true)
  try fileManager.createDirectory(at: localesOnlyURL, withIntermediateDirectories: true)
  try Data("{}".utf8).write(to: localesOnlyURL.appendingPathComponent("en.json"))

  // Foundation keeps every `Bundle` ever created in `allBundles`, and the
  // real resolver searches that list, so the marker file must not share a
  // name with a shipped resource: an empty `VaporMenuBarTemplate.png` here
  // would be found by the menu bar icon tests running in the same process.
  let withImageURL = rootURL.appendingPathComponent("OtherVaporCore.bundle", isDirectory: true)
  try fileManager.createDirectory(at: withImageURL, withIntermediateDirectories: true)
  try Data().write(to: withImageURL.appendingPathComponent("VaporTestMarker.png"))

  let hasMenuBarImage: (Bundle) -> Bool = { bundle in
    bundle.url(forResource: "VaporTestMarker", withExtension: "png") != nil
  }

  let bundle = VaporResourceBundle.discover(
    searchRoots: [rootURL],
    containing: hasMenuBarImage,
    fileManager: fileManager
  )

  #expect(bundle?.bundleURL == withImageURL)
}

@Test
func discoverReturnsNilWhenNoBundleCarriesTheMarker() throws {
  let fileManager = FileManager.default
  let rootURL = fileManager.temporaryDirectory.appendingPathComponent(
    UUID().uuidString, isDirectory: true)
  defer {
    try? fileManager.removeItem(at: rootURL)
  }

  let bundleURL = rootURL.appendingPathComponent("Vapor_VaporCore.bundle", isDirectory: true)
  try fileManager.createDirectory(at: bundleURL, withIntermediateDirectories: true)
  try Data("{}".utf8).write(to: bundleURL.appendingPathComponent("en.json"))

  let bundle = VaporResourceBundle.discover(
    searchRoots: [rootURL],
    containing: { _ in false },
    fileManager: fileManager
  )

  #expect(bundle == nil)
}

@Test
func resolveFallsBackToMainBundleWhenNothingMatches() {
  let bundle = VaporResourceBundle.resolve(containing: { _ in false })

  #expect(bundle == Bundle.main)
}

@Test
func resolveSurvivesABundleWhoseDirectoryWasDeleted() throws {
  let fileManager = FileManager.default
  let rootURL = fileManager.temporaryDirectory.appendingPathComponent(
    UUID().uuidString, isDirectory: true)
  let bundleURL = rootURL.appendingPathComponent("Vapor_VaporCore.bundle", isDirectory: true)
  try fileManager.createDirectory(at: bundleURL, withIntermediateDirectories: true)
  // Foundation keeps every Bundle it ever created in `allBundles`; once the
  // directory is gone the instance answers with no URL at all.
  let deleted = try #require(Bundle(url: bundleURL))
  try fileManager.removeItem(at: rootURL)
  #expect(deleted.bundleIdentifier == nil)

  let bundle = VaporResourceBundle.resolve(containing: { _ in false }, fileManager: fileManager)

  #expect(bundle == Bundle.main)
}
