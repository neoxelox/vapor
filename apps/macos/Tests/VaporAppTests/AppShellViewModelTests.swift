import Foundation
import Testing

@testable import Vapor
@testable import VaporCore

@MainActor
@Test
func rapidDoubleToggleAutoLaunchRevertsInsteadOfDuplicating() {
  let viewModel = AppShellViewModel(
    daemonLifecycleManager: .placeholder(),
    configuration: VaporConfiguration()
  )
  let start = viewModel.state.autoLaunchEnabled

  // Two quick clicks (each computes its target from the published value).
  // Before the optimistic flip, both read the pre-op value and the second
  // click duplicated the first instead of reverting it.
  viewModel.toggleAutoLaunch()
  #expect(viewModel.state.autoLaunchEnabled == !start)
  viewModel.toggleAutoLaunch()
  #expect(viewModel.state.autoLaunchEnabled == start)
}

@MainActor
@Test
func ignoreRuleDraftsStartFromConfigurationAndTrackPendingChanges() throws {
  let fileManager = FileManager.default
  let rootURL = makeTemporaryRoot(fileManager: fileManager)
  defer { try? fileManager.removeItem(at: rootURL) }

  let configurationStore = VaporConfigurationStore(
    fileManager: fileManager,
    environment: ["VAPOR_DIR": rootURL.path]
  )
  let configuration = VaporConfiguration(
    preIgnoreRules: ["node_modules/"],
    postIgnoreRules: ["!node_modules/keep.txt"]
  )

  let viewModel = AppShellViewModel(
    daemonLifecycleManager: .placeholder(),
    configurationStore: configurationStore,
    configuration: configuration
  )

  #expect(viewModel.state.preIgnoreRules == "node_modules/")
  #expect(viewModel.state.postIgnoreRules == "!node_modules/keep.txt")
  #expect(!viewModel.hasPendingIgnoreRuleChanges)

  viewModel.updatePreIgnoreRulesDraft("tmp/\n")

  #expect(viewModel.state.preIgnoreRules == "tmp/\n")
  #expect(viewModel.hasPendingIgnoreRuleChanges)
}

@MainActor
@Test
func startupPreservesUserSelectedLanguageCodeEvenWhenCatalogFallsBackToEnglish() throws {
  let fileManager = FileManager.default
  let rootURL = makeTemporaryRoot(fileManager: fileManager)
  defer { try? fileManager.removeItem(at: rootURL) }

  let configurationStore = VaporConfigurationStore(
    fileManager: fileManager,
    environment: ["VAPOR_DIR": rootURL.path]
  )
  let configuration = VaporConfiguration(language: "zz-unavailable-locale")
  try configurationStore.save(configuration)

  let viewModel = AppShellViewModel(
    daemonLifecycleManager: .placeholder(),
    configurationStore: configurationStore,
    configuration: configuration
  )

  #expect(viewModel.state.language == "zz-unavailable-locale")
  #expect(viewModel.state.effectiveLanguageCode == "en")

  let persisted = configurationStore.load()
  #expect(persisted.language == "zz-unavailable-locale")
}

@MainActor
@Test
func saveIgnoreRuleSettingsPersistsConfigurationAndRefreshesLifecycleFactory() throws {
  let fileManager = FileManager.default
  let rootURL = makeTemporaryRoot(fileManager: fileManager)
  defer { try? fileManager.removeItem(at: rootURL) }

  let configurationStore = VaporConfigurationStore(
    fileManager: fileManager,
    environment: ["VAPOR_DIR": rootURL.path]
  )
  let configuration = VaporConfiguration(
    preIgnoreRules: ["node_modules/"],
    postIgnoreRules: []
  )

  var refreshedConfigurations: [VaporConfiguration] = []
  let viewModel = AppShellViewModel(
    daemonLifecycleManager: .placeholder(),
    configurationStore: configurationStore,
    configuration: configuration,
    lifecycleManagerFactory: { configuration in
      refreshedConfigurations.append(configuration)
      return .placeholder()
    }
  )

  viewModel.updatePreIgnoreRulesDraft("tmp/\r\n!.keep\r\n")
  viewModel.updatePostIgnoreRulesDraft("build/\r\n")

  viewModel.saveIgnoreRuleSettings()

  let persisted = configurationStore.load()
  #expect(persisted.preIgnoreRules == ["tmp/", "!.keep"])
  #expect(persisted.postIgnoreRules == ["build/"])
  #expect(viewModel.state.preIgnoreRules == "tmp/\n!.keep")
  #expect(viewModel.state.postIgnoreRules == "build/")
  #expect(!viewModel.hasPendingIgnoreRuleChanges)
  #expect(refreshedConfigurations.count == 1)
  #expect(refreshedConfigurations[0].preIgnoreRules == ["tmp/", "!.keep"])
  #expect(refreshedConfigurations[0].postIgnoreRules == ["build/"])
}

@MainActor
@Test
func useGitignoreTogglePersistsConfigurationAndRefreshesLifecycleFactory() throws {
  let fileManager = FileManager.default
  let rootURL = makeTemporaryRoot(fileManager: fileManager)
  defer { try? fileManager.removeItem(at: rootURL) }

  let configurationStore = VaporConfigurationStore(
    fileManager: fileManager,
    environment: ["VAPOR_DIR": rootURL.path]
  )
  var refreshedConfigurations: [VaporConfiguration] = []
  let viewModel = AppShellViewModel(
    daemonLifecycleManager: .placeholder(),
    configurationStore: configurationStore,
    configuration: VaporConfiguration(useGitignore: true),
    lifecycleManagerFactory: { configuration in
      refreshedConfigurations.append(configuration)
      return .placeholder()
    }
  )

  viewModel.setUseGitignore(false)

  let persisted = configurationStore.load()
  #expect(persisted.useGitignore == false)
  #expect(viewModel.state.useGitignore == false)
  #expect(refreshedConfigurations.count == 1)
  #expect(refreshedConfigurations[0].useGitignore == false)
}

@MainActor
@Test
func useVaporignoreTogglePersistsConfigurationAndRefreshesLifecycleFactory() throws {
  let fileManager = FileManager.default
  let rootURL = makeTemporaryRoot(fileManager: fileManager)
  defer { try? fileManager.removeItem(at: rootURL) }

  let configurationStore = VaporConfigurationStore(
    fileManager: fileManager,
    environment: ["VAPOR_DIR": rootURL.path]
  )
  var refreshedConfigurations: [VaporConfiguration] = []
  let viewModel = AppShellViewModel(
    daemonLifecycleManager: .placeholder(),
    configurationStore: configurationStore,
    configuration: VaporConfiguration(useVaporignore: true),
    lifecycleManagerFactory: { configuration in
      refreshedConfigurations.append(configuration)
      return .placeholder()
    }
  )

  viewModel.setUseVaporignore(false)

  let persisted = configurationStore.load()
  #expect(persisted.useVaporignore == false)
  #expect(viewModel.state.useVaporignore == false)
  #expect(refreshedConfigurations.count == 1)
  #expect(refreshedConfigurations[0].useVaporignore == false)
}

@MainActor
@Test
func startupPreservesMalformedConfigurationAndSurfacesDiagnosticState() throws {
  let fileManager = FileManager.default
  let rootURL = makeTemporaryRoot(fileManager: fileManager)
  defer { try? fileManager.removeItem(at: rootURL) }

  try fileManager.createDirectory(at: rootURL, withIntermediateDirectories: true)
  let configurationURL = VaporPaths.configurationFileURL(vaporDirectoryURL: rootURL)
  let malformedContents = "{\n  \"autoLaunch\": true,\n  \"useGitignore\":\n".data(using: .utf8)!
  try malformedContents.write(to: configurationURL, options: .atomic)

  let configurationStore = VaporConfigurationStore(
    fileManager: fileManager,
    environment: ["VAPOR_DIR": rootURL.path]
  )

  let viewModel = AppShellViewModel(
    daemonLifecycleManager: .placeholder(),
    configurationStore: configurationStore
  )

  #expect(viewModel.state.syncState == .error)
  #expect(viewModel.state.configurationIssuePath == configurationURL.path)
  #expect(viewModel.state.configurationIssueReason?.isEmpty == false)
  #expect((try Data(contentsOf: configurationURL)) == malformedContents)
}

private func makeTemporaryRoot(fileManager: FileManager) -> URL {
  fileManager.temporaryDirectory
    .appendingPathComponent("vapor-app-shell-tests")
    .appendingPathComponent(UUID().uuidString, isDirectory: true)
}
