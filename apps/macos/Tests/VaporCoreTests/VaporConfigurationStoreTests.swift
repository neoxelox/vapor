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

// The `autoLaunch` key in `vapor.json` is owned by the Rust lifecycle
// core since M2-1/C4-7 (`JsonFileAutoLaunchSettingStore`, covered in
// core/lifecycle); Swift only reads it as part of the configuration.

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

@Test
func loadResultPreservesMalformedConfigurationFileAndReportsIssue() throws {
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
  let malformedContents = "{\n  \"autoLaunch\": true,\n  \"useGitIgnore\":\n".data(using: .utf8)!
  try malformedContents.write(to: configurationURL, options: .atomic)

  let result = store.loadResult()

  #expect(result.configuration == VaporConfiguration())
  #expect(result.issue?.configPath == configurationURL.path)
  #expect(result.issue?.reason.isEmpty == false)
  #expect((try Data(contentsOf: configurationURL)) == malformedContents)

  try fileManager.removeItem(at: rootURL)
}

@Test
func loadSaveRoundTripPreservesRuntimeOwnedKeysAndDeviceId() throws {
  let fileManager = FileManager.default
  let rootURL = fileManager.temporaryDirectory
    .appendingPathComponent("vapor-config-tests")
    .appendingPathComponent(UUID().uuidString, isDirectory: true)
  try fileManager.createDirectory(at: rootURL, withIntermediateDirectories: true)
  let configurationURL = VaporPaths.configurationFileURL(vaporDirectoryURL: rootURL)

  // A vapor.json shaped by the Rust runtime: Wave 8 keys the app does
  // not model first-class must survive an app-side save untouched.
  let daemonWritten = """
    {
      "languageCode": "en",
      "deviceId": "mac-studio-01",
      "provider": "google_drive",
      "syncMode": "pull-only",
      "profiles": [{"id": "work", "localSyncDirectory": "~/Work"}],
      "resourceLimits": {"cpuPercent": 20},
      "idleBoost": {"enabled": false}
    }
    """
  try daemonWritten.data(using: .utf8)!.write(to: configurationURL)

  let store = VaporConfigurationStore(
    fileManager: fileManager,
    environment: ["VAPOR_DIR": rootURL.path]
  )
  var configuration = store.load()
  #expect(configuration.deviceId == "mac-studio-01")
  #expect(
    configuration.additionalKeys[VaporConstants.ConfigKeys.provider] == .string("google_drive"))
  #expect(configuration.additionalKeys[VaporConstants.ConfigKeys.syncMode] == .string("pull-only"))

  // The app edits one of its own settings and saves.
  configuration.autoLaunch = false
  try store.save(configuration)

  let reloaded =
    try JSONSerialization.jsonObject(
      with: Data(contentsOf: configurationURL)
    ) as! [String: Any]
  #expect(reloaded["autoLaunch"] as? Bool == false)
  #expect(reloaded["deviceId"] as? String == "mac-studio-01")
  #expect(reloaded["provider"] as? String == "google_drive")
  #expect(reloaded["syncMode"] as? String == "pull-only")
  #expect((reloaded["profiles"] as? [[String: Any]])?.first?["id"] as? String == "work")
  #expect((reloaded["resourceLimits"] as? [String: Any])?["cpuPercent"] as? Double == 20)
  #expect((reloaded["idleBoost"] as? [String: Any])?["enabled"] as? Bool == false)

  try fileManager.removeItem(at: rootURL)
}

@Test
func absentDeviceIdIsNotInventedByTheApp() throws {
  let configuration = VaporConfiguration()
  #expect(configuration.deviceId == nil)

  let encoder = JSONEncoder()
  let data = try encoder.encode(configuration)
  let object = try JSONSerialization.jsonObject(with: data) as! [String: Any]
  #expect(object["deviceId"] == nil, "the daemon owns device-id generation")
}
