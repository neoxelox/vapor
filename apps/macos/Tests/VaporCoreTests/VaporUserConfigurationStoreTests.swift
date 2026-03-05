import Foundation
import Testing

@testable import VaporCore

@Test
func loadCreatesDefaultConfigurationAndRuntimeDirectories() throws {
  let fileManager = FileManager.default
  let rootURL = fileManager.temporaryDirectory
    .appendingPathComponent("vapor-config-tests")
    .appendingPathComponent(UUID().uuidString, isDirectory: true)
  let defaults = try configuredUserDefaults()
  let store = VaporUserConfigurationStore(
    fileManager: fileManager,
    environment: ["VAPOR_DIR": rootURL.path],
    defaults: defaults
  )

  let configuration = store.load()

  #expect(configuration.vaporDirectoryPath == rootURL.path)
  #expect(
    fileManager.fileExists(atPath: VaporPaths.configurationFileURL(vaporDirectoryURL: rootURL).path)
  )
  #expect(
    fileManager.fileExists(atPath: VaporPaths.logsDirectoryURL(vaporDirectoryURL: rootURL).path))
  #expect(
    fileManager.fileExists(atPath: VaporPaths.stateDirectoryURL(vaporDirectoryURL: rootURL).path))

  try fileManager.removeItem(at: rootURL)
}

@Test
func savePersistsConfigurationAndDefaultsPointer() throws {
  let fileManager = FileManager.default
  let rootURL = fileManager.temporaryDirectory
    .appendingPathComponent("vapor-config-tests")
    .appendingPathComponent(UUID().uuidString, isDirectory: true)
  let defaults = try configuredUserDefaults()
  let store = VaporUserConfigurationStore(
    fileManager: fileManager,
    environment: [:],
    defaults: defaults
  )

  let configuration = VaporUserConfiguration(
    vaporDirectoryPath: rootURL.path,
    useGitIgnore: false,
    timelineEventLimit: 1500
  )

  try store.save(configuration)
  let loaded = store.load()

  #expect(loaded.vaporDirectoryPath == rootURL.path)
  #expect(loaded.useGitIgnore == false)
  #expect(loaded.timelineEventLimit == 1500)
  #expect(
    defaults.string(forKey: VaporUserConfigurationStore.vaporDirectoryDefaultsKey) == rootURL.path)

  try fileManager.removeItem(at: rootURL)
}

private func configuredUserDefaults() throws -> UserDefaults {
  let suiteName = "vapor-tests-\(UUID().uuidString)"
  guard let defaults = UserDefaults(suiteName: suiteName) else {
    struct CreateDefaultsError: Error {}
    throw CreateDefaultsError()
  }
  defaults.removePersistentDomain(forName: suiteName)
  return defaults
}
