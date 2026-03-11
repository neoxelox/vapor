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

  #expect(configuration.autoLaunch == true)
  #expect(configuration.useGitIgnore == true)
  #expect(configuration.useVaporIgnore == true)
  #expect(configuration.localSyncDirectory == VaporConfiguration.defaultLocalSyncDirectory)
  #expect(configuration.cloudSyncDirectory == VaporConfiguration.defaultCloudSyncDirectory)
  #expect(configuration.preIgnoreRules == VaporConfiguration.defaultPreIgnoreRules)
  #expect(configuration.postIgnoreRules == VaporConfiguration.defaultPostIgnoreRules)
  #expect(configuration.languageCode == VaporConfiguration.defaultLanguageCode)
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
    autoLaunch: false,
    useGitIgnore: false,
    useVaporIgnore: false,
    localSyncDirectory: "~/Desktop/Vapor",
    cloudSyncDirectory: "/RemoteVapor",
    preIgnoreRules: "",
    postIgnoreRules: "*.bak",
    languageCode: "en",
    timelineEventLimit: 1500
  )

  try store.save(configuration)
  let loaded = store.load()

  #expect(loaded.autoLaunch == false)
  #expect(loaded.useGitIgnore == false)
  #expect(loaded.useVaporIgnore == false)
  #expect(loaded.localSyncDirectory == "~/Desktop/Vapor")
  #expect(loaded.cloudSyncDirectory == "/RemoteVapor")
  #expect(loaded.preIgnoreRules.isEmpty)
  #expect(loaded.postIgnoreRules == "*.bak")
  #expect(loaded.languageCode == "en")
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
  #expect(configuration.autoLaunch == false)

  try fileManager.removeItem(at: rootURL)
}

@Test
func loadDefaultsMissingKeysWithoutDroppingOtherSavedValues() throws {
  let fileManager = FileManager.default
  let rootURL = fileManager.temporaryDirectory
    .appendingPathComponent("vapor-config-tests")
    .appendingPathComponent(UUID().uuidString, isDirectory: true)
  let store = VaporConfigurationStore(
    fileManager: fileManager,
    environment: ["VAPOR_DIR": rootURL.path]
  )

  try fileManager.createDirectory(at: rootURL, withIntermediateDirectories: true)
  let configurationURL = VaporPaths.configurationFileURL(vaporDirectoryURL: rootURL)
  let partialConfiguration = try JSONSerialization.data(
    withJSONObject: [
      "useGitIgnore": false,
      "localSyncDirectory": "~/Projects/Vapor",
      "postIgnoreRules": "*.bak",
      "timelineEventLimit": 42,
    ],
    options: [.sortedKeys]
  )
  try partialConfiguration.write(to: configurationURL, options: .atomic)

  let configuration = store.load()

  #expect(configuration.autoLaunch == true)
  #expect(configuration.useGitIgnore == false)
  #expect(configuration.localSyncDirectory == "~/Projects/Vapor")
  #expect(configuration.postIgnoreRules == "*.bak")
  #expect(configuration.languageCode == "en")
  #expect(configuration.timelineEventLimit == 42)

  try fileManager.removeItem(at: rootURL)
}
