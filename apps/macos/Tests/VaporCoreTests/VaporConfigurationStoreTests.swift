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
  #expect(configuration.useGitignore == true)
  #expect(configuration.useVaporignore == true)
  #expect(configuration.localRoot == VaporConfiguration.defaultLocalRoot)
  #expect(configuration.cloudRoot == nil, "the provider's default is the daemon's call")
  #expect(configuration.preIgnoreRules == VaporConfiguration.defaultPreIgnoreRules)
  #expect(configuration.postIgnoreRules == VaporConfiguration.defaultPostIgnoreRules)
  #expect(configuration.language == VaporConfiguration.defaultLanguage)
  #expect(configuration.timelineLimit == 1000)
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
    useGitignore: false,
    useVaporignore: false,
    localRoot: "~/Desktop/Vapor",
    cloudRoot: "/RemoteVapor",
    preIgnoreRules: [],
    postIgnoreRules: ["*.bak"],
    language: "en",
    timelineLimit: 1500
  )

  try store.save(configuration, keys: VaporConfiguration.modelledKeys)
  let loaded = store.load()

  #expect(loaded.autoLaunch == false)
  #expect(loaded.useGitignore == false)
  #expect(loaded.useVaporignore == false)
  #expect(loaded.localRoot == "~/Desktop/Vapor")
  #expect(loaded.cloudRoot == "/RemoteVapor")
  #expect(loaded.preIgnoreRules.isEmpty)
  #expect(loaded.postIgnoreRules == ["*.bak"])
  #expect(loaded.language == "en")
  #expect(loaded.timelineLimit == 1500)

  try fileManager.removeItem(at: rootURL)
}

// The `autoLaunch` key in `vapor.json` is owned by the Rust lifecycle
// core (`JsonFileAutoLaunchSettingStore`, covered in
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
      "useGitignore": false,
      "localRoot": "~/Projects/Vapor",
      "postIgnoreRules": ["*.bak"],
      "timelineLimit": 42,
    ],
    options: [.sortedKeys]
  )
  try partialConfiguration.write(to: configurationURL, options: .atomic)

  let configuration = store.load()

  #expect(configuration.autoLaunch == true)
  #expect(configuration.useGitignore == false)
  #expect(configuration.localRoot == "~/Projects/Vapor")
  #expect(configuration.postIgnoreRules == ["*.bak"])
  #expect(configuration.language == "en")
  #expect(configuration.timelineLimit == 42)

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
  let malformedContents = "{\n  \"autoLaunch\": true,\n  \"useGitignore\":\n".data(using: .utf8)!
  try malformedContents.write(to: configurationURL, options: .atomic)

  let result = store.loadResult()

  #expect(result.configuration == VaporConfiguration())
  #expect(result.issue?.configPath == configurationURL.path)
  #expect(result.issue?.reason.isEmpty == false)
  #expect((try Data(contentsOf: configurationURL)) == malformedContents)

  try fileManager.removeItem(at: rootURL)
}

@Test
func oneInvalidValueFallsBackAloneAndASaveTouchesOnlyItsKey() throws {
  let fileManager = FileManager.default
  let rootURL = fileManager.temporaryDirectory
    .appendingPathComponent("vapor-config-tests")
    .appendingPathComponent(UUID().uuidString, isDirectory: true)
  try fileManager.createDirectory(at: rootURL, withIntermediateDirectories: true)
  defer { try? fileManager.removeItem(at: rootURL) }
  let configurationURL = VaporPaths.configurationFileURL(vaporDirectoryURL: rootURL)
  // Ignore rules in the older one-string shape, next to keys the app
  // does not model.
  let written = """
    {
      "localRoot": "~/Work",
      "preIgnoreRules": "node_modules/\\nbuild/",
      "provider": "gdrive",
      "deviceId": "mac-studio"
    }
    """
  try written.data(using: .utf8)!.write(to: configurationURL)
  let store = VaporConfigurationStore(
    fileManager: fileManager,
    environment: ["VAPOR_DIR": rootURL.path]
  )

  let result = store.loadResult()
  #expect(result.configuration.localRoot == "~/Work")
  #expect(result.configuration.preIgnoreRules == VaporConfiguration.defaultPreIgnoreRules)
  #expect(result.issue?.reason.contains("preIgnoreRules") == true)

  var configuration = result.configuration
  configuration.useGitignore = false
  try store.save(configuration, keys: [VaporConstants.ConfigKeys.useGitignore])
  let saved = store.loadResult()
  // The save touched one key: the invalid value is still the user's to
  // fix, and the banner still names it.
  #expect(saved.issue?.reason.contains("preIgnoreRules") == true)
  #expect(saved.configuration.useGitignore == false)
  #expect(saved.configuration.localRoot == "~/Work")
  #expect(saved.configuration.deviceId == "mac-studio")
  #expect(
    saved.configuration.additionalKeys[VaporConstants.ConfigKeys.provider] == .string("gdrive"))
}

@Test
func aSaveNeverOverwritesAFileThatDoesNotParse() throws {
  let fileManager = FileManager.default
  let rootURL = fileManager.temporaryDirectory
    .appendingPathComponent("vapor-config-tests")
    .appendingPathComponent(UUID().uuidString, isDirectory: true)
  try fileManager.createDirectory(at: rootURL, withIntermediateDirectories: true)
  defer { try? fileManager.removeItem(at: rootURL) }
  let configurationURL = VaporPaths.configurationFileURL(vaporDirectoryURL: rootURL)
  let broken = "{ \"provider\": \"gdrive\",".data(using: .utf8)!
  try broken.write(to: configurationURL)
  let store = VaporConfigurationStore(
    fileManager: fileManager,
    environment: ["VAPOR_DIR": rootURL.path]
  )

  #expect(throws: VaporConfigurationStoreError.self) {
    try store.save(VaporConfiguration(), keys: [VaporConstants.ConfigKeys.useGitignore])
  }
  #expect((try Data(contentsOf: configurationURL)) == broken)
}

@Test
func loadSaveRoundTripPreservesRuntimeOwnedKeysAndDeviceId() throws {
  let fileManager = FileManager.default
  let rootURL = fileManager.temporaryDirectory
    .appendingPathComponent("vapor-config-tests")
    .appendingPathComponent(UUID().uuidString, isDirectory: true)
  try fileManager.createDirectory(at: rootURL, withIntermediateDirectories: true)
  let configurationURL = VaporPaths.configurationFileURL(vaporDirectoryURL: rootURL)

  // A vapor.json shaped by the Rust runtime: runtime keys the app does
  // not model first-class must survive an app-side save untouched.
  let daemonWritten = """
    {
      "language": "en",
      "deviceId": "mac-studio-01",
      "provider": "gdrive",
      "syncMode": "pull-only",
      "profiles": [{"id": "work", "localRoot": "~/Work"}],
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
    configuration.additionalKeys[VaporConstants.ConfigKeys.provider] == .string("gdrive"))
  #expect(configuration.additionalKeys[VaporConstants.ConfigKeys.syncMode] == .string("pull-only"))

  // The app edits one of its own settings and saves.
  configuration.autoLaunch = false
  try store.save(configuration, keys: [VaporConstants.ConfigKeys.autoLaunch])

  let reloaded =
    try JSONSerialization.jsonObject(
      with: Data(contentsOf: configurationURL)
    ) as! [String: Any]
  #expect(reloaded["autoLaunch"] as? Bool == false)
  #expect(reloaded["deviceId"] as? String == "mac-studio-01")
  #expect(reloaded["provider"] as? String == "gdrive")
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

@Test
func saveMergesOntoDiskAndPreservesDaemonOwnedKeys() throws {
  let fileManager = FileManager.default
  let rootURL = fileManager.temporaryDirectory
    .appendingPathComponent("vapor-config-merge-tests")
    .appendingPathComponent(UUID().uuidString, isDirectory: true)
  try fileManager.createDirectory(at: rootURL, withIntermediateDirectories: true)
  defer { try? fileManager.removeItem(at: rootURL) }

  let store = VaporConfigurationStore(
    fileManager: fileManager,
    environment: ["VAPOR_DIR": rootURL.path]
  )
  // App loads its snapshot at launch.
  var appConfig = store.load()

  // Meanwhile the daemon/CLI writes daemon-owned keys straight to disk.
  let configURL = VaporPaths.configurationFileURL(vaporDirectoryURL: rootURL)
  let daemonWritten = """
    {
      "autoLaunch": true,
      "deviceId": "device-xyz",
      "syncMode": "pull-only",
      "provider": "gdrive"
    }
    """
  try Data(daemonWritten.utf8).write(to: configURL)

  // The app saves its (stale) snapshot after a settings toggle.
  appConfig.useGitignore = false
  try store.save(appConfig, keys: [VaporConstants.ConfigKeys.useGitignore])

  let reloaded = store.load()
  // The app-modeled change applied...
  #expect(reloaded.useGitignore == false)
  // ...and the daemon-owned keys survived (not clobbered by the stale
  // snapshot).
  #expect(reloaded.deviceId == "device-xyz")
  #expect(reloaded.additionalKeys["syncMode"] == .string("pull-only"))
  #expect(reloaded.additionalKeys["provider"] == .string("gdrive"))
}

@Test
func aSettingChangedWithTheCLIAfterLaunchSurvivesAnAppSave() throws {
  let fileManager = FileManager.default
  let rootURL = fileManager.temporaryDirectory
    .appendingPathComponent("vapor-config-tests")
    .appendingPathComponent(UUID().uuidString, isDirectory: true)
  try fileManager.createDirectory(at: rootURL, withIntermediateDirectories: true)
  defer { try? fileManager.removeItem(at: rootURL) }
  let store = VaporConfigurationStore(
    fileManager: fileManager,
    environment: ["VAPOR_DIR": rootURL.path]
  )
  var appConfig = store.load()

  // After launch: `vapor service uninstall` and `vapor config set`.
  let configURL = VaporPaths.configurationFileURL(vaporDirectoryURL: rootURL)
  let changed = """
    {"autoLaunch": false, "postIgnoreRules": ["private/"], "cloudRoot": "/Work"}
    """
  try Data(changed.utf8).write(to: configURL)

  appConfig.language = "es"
  try store.save(appConfig, keys: [VaporConstants.ConfigKeys.language])

  let reloaded = store.load()
  #expect(reloaded.language == "es")
  #expect(reloaded.autoLaunch == false)
  #expect(reloaded.postIgnoreRules == ["private/"])
  #expect(reloaded.cloudRoot == "/Work")
}

@Test
func anEmptyFileIsTheDefaultsAndCanBeSavedOver() throws {
  let fileManager = FileManager.default
  let rootURL = fileManager.temporaryDirectory
    .appendingPathComponent("vapor-config-tests")
    .appendingPathComponent(UUID().uuidString, isDirectory: true)
  try fileManager.createDirectory(at: rootURL, withIntermediateDirectories: true)
  defer { try? fileManager.removeItem(at: rootURL) }
  let configURL = VaporPaths.configurationFileURL(vaporDirectoryURL: rootURL)
  try Data("  \n".utf8).write(to: configURL)
  let store = VaporConfigurationStore(
    fileManager: fileManager,
    environment: ["VAPOR_DIR": rootURL.path]
  )

  let result = store.loadResult()
  #expect(result.issue == nil)
  #expect(result.configuration == VaporConfiguration())
  var configuration = result.configuration
  configuration.useVaporignore = false
  try store.save(configuration, keys: [VaporConstants.ConfigKeys.useVaporignore])
  #expect(store.load().useVaporignore == false)
}
