import Foundation
import Testing

@testable import VaporCore

@Test
func installWritesPlistAndBootstrapsLaunchAgent() throws {
  let sandboxURL = try makeTemporaryDirectory()
  defer { try? FileManager.default.removeItem(at: sandboxURL) }

  let runner = RecordingLaunchctlRunner()
  let config = LaunchAgentConfiguration(
    label: "sh.arn.vapor.daemon",
    plistURL: sandboxURL.appendingPathComponent("sh.arn.vapor.daemon.plist"),
    daemonExecutableURL: URL(fileURLWithPath: "/usr/local/bin/vapord")
  )

  let controller = LaunchAgentController(
    configuration: config,
    runner: runner,
    userID: 501
  )

  try controller.installAndEnable()

  #expect(FileManager.default.fileExists(atPath: config.plistURL.path))
  #expect(
    runner.calls == [
      ["bootout", "gui/501", config.plistURL.path],
      ["bootstrap", "gui/501", config.plistURL.path],
      ["enable", "gui/501/sh.arn.vapor.daemon"],
    ]
  )
}

@Test
func disableUnloadsLaunchAgentAndRemovesPlist() throws {
  let sandboxURL = try makeTemporaryDirectory()
  defer { try? FileManager.default.removeItem(at: sandboxURL) }

  let runner = RecordingLaunchctlRunner()
  let config = LaunchAgentConfiguration(
    label: "sh.arn.vapor.daemon",
    plistURL: sandboxURL.appendingPathComponent("sh.arn.vapor.daemon.plist"),
    daemonExecutableURL: URL(fileURLWithPath: "/usr/local/bin/vapord")
  )

  let controller = LaunchAgentController(
    configuration: config,
    runner: runner,
    userID: 501
  )

  try controller.installAndEnable()
  runner.calls.removeAll()

  try controller.disableAndUninstall()

  #expect(!FileManager.default.fileExists(atPath: config.plistURL.path))
  #expect(
    runner.calls == [
      ["disable", "gui/501/sh.arn.vapor.daemon"],
      ["bootout", "gui/501", config.plistURL.path],
    ]
  )
}

@Test
func startAndStopIssueExpectedLaunchctlCommands() throws {
  let sandboxURL = try makeTemporaryDirectory()
  defer { try? FileManager.default.removeItem(at: sandboxURL) }

  let runner = RecordingLaunchctlRunner()
  let config = LaunchAgentConfiguration(
    label: "sh.arn.vapor.daemon",
    plistURL: sandboxURL.appendingPathComponent("sh.arn.vapor.daemon.plist"),
    daemonExecutableURL: URL(fileURLWithPath: "/usr/local/bin/vapord")
  )

  let controller = LaunchAgentController(
    configuration: config,
    runner: runner,
    userID: 501
  )

  try controller.startDaemon()
  try controller.stopDaemon()

  #expect(
    runner.calls == [
      ["kickstart", "-k", "gui/501/sh.arn.vapor.daemon"],
      ["kill", "TERM", "gui/501/sh.arn.vapor.daemon"],
    ]
  )
}

private func makeTemporaryDirectory() throws -> URL {
  let tempURL = FileManager.default.temporaryDirectory
    .appendingPathComponent("vapor-tests")
    .appendingPathComponent(UUID().uuidString)
  try FileManager.default.createDirectory(at: tempURL, withIntermediateDirectories: true)
  return tempURL
}

private final class RecordingLaunchctlRunner: LaunchctlCommandRunning {
  var calls: [[String]] = []

  func run(arguments: [String]) throws -> LaunchctlCommandResult {
    calls.append(arguments)
    return LaunchctlCommandResult(exitCode: 0, standardError: "")
  }
}
