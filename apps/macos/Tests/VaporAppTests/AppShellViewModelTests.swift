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

private func makeTemporaryRoot(fileManager: FileManager) -> URL {
  fileManager.temporaryDirectory
    .appendingPathComponent("vapor-app-shell-tests")
    .appendingPathComponent(UUID().uuidString, isDirectory: true)
}
