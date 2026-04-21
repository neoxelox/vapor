import Foundation
import Testing

@testable import Vapor
@testable import VaporCore

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
    preIgnoreRules: "node_modules/\n",
    postIgnoreRules: "!node_modules/keep.txt\n"
  )

  let viewModel = AppShellViewModel(
    daemonLifecycleManager: .placeholder(),
    configurationStore: configurationStore,
    configuration: configuration
  )

  #expect(viewModel.state.preIgnoreRules == "node_modules/\n")
  #expect(viewModel.state.postIgnoreRules == "!node_modules/keep.txt\n")
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
  let configuration = VaporConfiguration(languageCode: "zz-unavailable-locale")
  try configurationStore.save(configuration)

  let viewModel = AppShellViewModel(
    daemonLifecycleManager: .placeholder(),
    configurationStore: configurationStore,
    configuration: configuration
  )

  #expect(viewModel.state.languageCode == "zz-unavailable-locale")
  #expect(viewModel.state.effectiveLanguageCode == "en")

  let persisted = configurationStore.load()
  #expect(persisted.languageCode == "zz-unavailable-locale")
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
    preIgnoreRules: "node_modules/\n",
    postIgnoreRules: ""
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
  #expect(persisted.preIgnoreRules == "tmp/\n!.keep\n")
  #expect(persisted.postIgnoreRules == "build/\n")
  #expect(viewModel.state.preIgnoreRules == "tmp/\n!.keep\n")
  #expect(viewModel.state.postIgnoreRules == "build/\n")
  #expect(!viewModel.hasPendingIgnoreRuleChanges)
  #expect(refreshedConfigurations.count == 1)
  #expect(refreshedConfigurations[0].preIgnoreRules == "tmp/\n!.keep\n")
  #expect(refreshedConfigurations[0].postIgnoreRules == "build/\n")
}

@MainActor
@Test
func useGitIgnoreTogglePersistsConfigurationAndRefreshesLifecycleFactory() throws {
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
    configuration: VaporConfiguration(useGitIgnore: true),
    lifecycleManagerFactory: { configuration in
      refreshedConfigurations.append(configuration)
      return .placeholder()
    }
  )

  viewModel.setUseGitIgnore(false)

  let persisted = configurationStore.load()
  #expect(persisted.useGitIgnore == false)
  #expect(viewModel.state.useGitIgnore == false)
  #expect(refreshedConfigurations.count == 1)
  #expect(refreshedConfigurations[0].useGitIgnore == false)
}

@MainActor
@Test
func useVaporIgnoreTogglePersistsConfigurationAndRefreshesLifecycleFactory() throws {
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
    configuration: VaporConfiguration(useVaporIgnore: true),
    lifecycleManagerFactory: { configuration in
      refreshedConfigurations.append(configuration)
      return .placeholder()
    }
  )

  viewModel.setUseVaporIgnore(false)

  let persisted = configurationStore.load()
  #expect(persisted.useVaporIgnore == false)
  #expect(viewModel.state.useVaporIgnore == false)
  #expect(refreshedConfigurations.count == 1)
  #expect(refreshedConfigurations[0].useVaporIgnore == false)
}

@MainActor
@Test
func startupPreservesMalformedConfigurationAndSurfacesDiagnosticState() throws {
  let fileManager = FileManager.default
  let rootURL = makeTemporaryRoot(fileManager: fileManager)
  defer { try? fileManager.removeItem(at: rootURL) }

  try fileManager.createDirectory(at: rootURL, withIntermediateDirectories: true)
  let configurationURL = VaporPaths.configurationFileURL(vaporDirectoryURL: rootURL)
  let malformedContents = "{\n  \"autoLaunch\": true,\n  \"useGitIgnore\":\n".data(using: .utf8)!
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
