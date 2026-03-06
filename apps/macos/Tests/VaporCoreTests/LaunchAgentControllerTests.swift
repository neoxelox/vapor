import Foundation
import Testing

@testable import VaporCore

@Test
func installWritesPlistAndBootstrapsLaunchAgent() throws {
  let sandboxURL = try makeTemporaryDirectory()
  defer { try? FileManager.default.removeItem(at: sandboxURL) }

  let daemonExecutableURL = try makeExecutable(named: "vapord", in: sandboxURL)
  let runner = RecordingLaunchctlRunner()
  let config = LaunchAgentConfiguration(
    label: "sh.arn.vapor.daemon",
    plistURL: sandboxURL.appendingPathComponent("sh.arn.vapor.daemon.plist"),
    daemonExecutableURL: daemonExecutableURL
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
      ["enable", "gui/501/sh.arn.vapor.daemon"],
      ["bootstrap", "gui/501", config.plistURL.path],
    ]
  )
}

@Test
func disableUnloadsLaunchAgentAndRemovesPlist() throws {
  let sandboxURL = try makeTemporaryDirectory()
  defer { try? FileManager.default.removeItem(at: sandboxURL) }

  let daemonExecutableURL = try makeExecutable(named: "vapord", in: sandboxURL)
  let runner = RecordingLaunchctlRunner()
  let config = LaunchAgentConfiguration(
    label: "sh.arn.vapor.daemon",
    plistURL: sandboxURL.appendingPathComponent("sh.arn.vapor.daemon.plist"),
    daemonExecutableURL: daemonExecutableURL
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

  let daemonExecutableURL = try makeExecutable(named: "vapord", in: sandboxURL)
  let runner = RecordingLaunchctlRunner()
  let config = LaunchAgentConfiguration(
    label: "sh.arn.vapor.daemon",
    plistURL: sandboxURL.appendingPathComponent("sh.arn.vapor.daemon.plist"),
    daemonExecutableURL: daemonExecutableURL
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

@Test
func installFailsWhenDaemonExecutableIsMissing() throws {
  let sandboxURL = try makeTemporaryDirectory()
  defer { try? FileManager.default.removeItem(at: sandboxURL) }

  let runner = RecordingLaunchctlRunner()
  let missingExecutableURL = sandboxURL.appendingPathComponent("vapord")
  let config = LaunchAgentConfiguration(
    label: "sh.arn.vapor.daemon",
    plistURL: sandboxURL.appendingPathComponent("sh.arn.vapor.daemon.plist"),
    daemonExecutableURL: missingExecutableURL
  )

  let controller = LaunchAgentController(
    configuration: config,
    runner: runner,
    userID: 501
  )

  do {
    try controller.installAndEnable()
    #expect(Bool(false))
  } catch let error as LaunchAgentControllerError {
    #expect(error == .daemonExecutableMissing(path: missingExecutableURL.path))
  }

  #expect(runner.calls.isEmpty)
  #expect(!FileManager.default.fileExists(atPath: config.plistURL.path))
}

@Test
func installFailsWhenDaemonExecutableIsNotExecutable() throws {
  let sandboxURL = try makeTemporaryDirectory()
  defer { try? FileManager.default.removeItem(at: sandboxURL) }

  let runner = RecordingLaunchctlRunner()
  let daemonExecutableURL = try makeNonExecutableFile(named: "vapord", in: sandboxURL)
  let config = LaunchAgentConfiguration(
    label: "sh.arn.vapor.daemon",
    plistURL: sandboxURL.appendingPathComponent("sh.arn.vapor.daemon.plist"),
    daemonExecutableURL: daemonExecutableURL
  )

  let controller = LaunchAgentController(
    configuration: config,
    runner: runner,
    userID: 501
  )

  do {
    try controller.installAndEnable()
    #expect(Bool(false))
  } catch let error as LaunchAgentControllerError {
    #expect(error == .daemonExecutableNotExecutable(path: daemonExecutableURL.path))
  }

  #expect(runner.calls.isEmpty)
  #expect(!FileManager.default.fileExists(atPath: config.plistURL.path))
}

private func makeTemporaryDirectory() throws -> URL {
  let tempURL = FileManager.default.temporaryDirectory
    .appendingPathComponent("vapor-tests")
    .appendingPathComponent(UUID().uuidString)
  try FileManager.default.createDirectory(at: tempURL, withIntermediateDirectories: true)
  return tempURL
}

private func makeExecutable(named name: String, in directory: URL) throws -> URL {
  let executableURL = directory.appendingPathComponent(name)
  try "#!/bin/sh\nexit 0\n".write(to: executableURL, atomically: true, encoding: .utf8)
  try FileManager.default.setAttributes(
    [.posixPermissions: 0o755], ofItemAtPath: executableURL.path)
  return executableURL
}

private func makeNonExecutableFile(named name: String, in directory: URL) throws -> URL {
  let fileURL = directory.appendingPathComponent(name)
  try "daemon".write(to: fileURL, atomically: true, encoding: .utf8)
  try FileManager.default.setAttributes([.posixPermissions: 0o644], ofItemAtPath: fileURL.path)
  return fileURL
}

private final class RecordingLaunchctlRunner: LaunchctlCommandRunning {
  var calls: [[String]] = []

  func run(arguments: [String]) throws -> LaunchctlCommandResult {
    calls.append(arguments)
    return LaunchctlCommandResult(exitCode: 0, standardError: "")
  }
}
