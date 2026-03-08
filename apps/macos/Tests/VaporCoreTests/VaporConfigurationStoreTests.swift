import Foundation
import Testing

@testable import VaporCore

@Test
func loadCreatesDefaultConfigurationAndRuntimeDirectories() throws {
  let fileManager = FileManager.default
  let rootURL = fileManager.temporaryDirectory
    .appendingPathComponent("vapor-config-tests")
    .appendingPathComponent(UUID().uuidString, isDirectory: true)
  let store = VaporConfigurationStore(
    fileManager: fileManager,
    environment: ["VAPOR_DIR": rootURL.path]
  )

  let configuration = store.load()

  #expect(configuration.autoLaunchEnabled == true)
  #expect(configuration.useGitIgnore == true)
  #expect(configuration.useVaporIgnore == true)
  #expect(configuration.preIgnoreRules == VaporConfiguration.defaultPreIgnoreRules)
  #expect(configuration.postIgnoreRules == VaporConfiguration.defaultPostIgnoreRules)
  #expect(configuration.timelineEventLimit == 1000)
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
func savePersistsConfigurationInResolvedRuntimeDirectory() throws {
  let fileManager = FileManager.default
  let rootURL = fileManager.temporaryDirectory
    .appendingPathComponent("vapor-config-tests")
    .appendingPathComponent(UUID().uuidString, isDirectory: true)
  let store = VaporConfigurationStore(
    fileManager: fileManager,
    environment: ["VAPOR_DIR": rootURL.path]
  )

  let configuration = VaporConfiguration(
    autoLaunchEnabled: false,
    useGitIgnore: false,
    useVaporIgnore: false,
    preIgnoreRules: "",
    postIgnoreRules: "*.bak",
    timelineEventLimit: 1500
  )

  try store.save(configuration)
  let loaded = store.load()

  #expect(loaded.autoLaunchEnabled == false)
  #expect(loaded.useGitIgnore == false)
  #expect(loaded.useVaporIgnore == false)
  #expect(loaded.preIgnoreRules.isEmpty)
  #expect(loaded.postIgnoreRules == "*.bak")
  #expect(loaded.timelineEventLimit == 1500)

  try fileManager.removeItem(at: rootURL)
}

@Test
func autoLaunchSettingStorePersistsIntoVaporJSON() throws {
  let fileManager = FileManager.default
  let rootURL = fileManager.temporaryDirectory
    .appendingPathComponent("vapor-config-tests")
    .appendingPathComponent(UUID().uuidString, isDirectory: true)

  let configurationStore = VaporConfigurationStore(
    fileManager: fileManager,
    environment: ["VAPOR_DIR": rootURL.path]
  )
  let autoLaunchStore = VaporConfigurationAutoLaunchSettingStore(
    configurationStore: configurationStore)

  #expect(autoLaunchStore.bool(forKey: DaemonLifecycleManager.autoLaunchSettingKey) == true)

  try autoLaunchStore.set(false, forKey: DaemonLifecycleManager.autoLaunchSettingKey)

  let configuration = configurationStore.load()
  #expect(configuration.autoLaunchEnabled == false)

  try fileManager.removeItem(at: rootURL)
}
