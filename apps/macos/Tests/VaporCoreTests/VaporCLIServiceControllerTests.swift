import Foundation
import Testing

@testable import VaporCore

// Contract tests for the `vapor service … --json` subprocess bridge
//. The JSON fixtures mirror the Rust renderer in
// `core/cli/src/commands/service.rs`
// (`json_contract_*` tests lock the same shapes on that side).

@Test
func bootstrapRunsServiceBootstrapWithJSONFlagAndDecodesStarted() throws {
  let runner = ScriptedCLIRunner(standardOutput: #"{"result":"started"}"#)
  let controller = VaporCLIServiceController(runner: runner)

  let result = try controller.bootstrap()

  #expect(result == .started)
  #expect(runner.invocations == [["service", "bootstrap", "--json"]])
}

@Test
func installAndStartAndStopComposeExpectedArguments() throws {
  let runner = ScriptedCLIRunner(standardOutput: #"{"result":"started"}"#)
  let controller = VaporCLIServiceController(runner: runner)

  _ = try controller.installAndEnable()
  _ = try controller.startDaemon()
  runner.standardOutput = #"{"result":"stopped"}"#
  try controller.stopDaemon()

  #expect(
    runner.invocations == [
      ["service", "install", "--json"],
      ["service", "start", "--json"],
      ["service", "stop", "--json"],
    ]
  )
}

@Test
func disableWithoutStopAppendsKeepRunningFlag() throws {
  let runner = ScriptedCLIRunner(standardOutput: #"{"result":"unchanged"}"#)
  let controller = VaporCLIServiceController(runner: runner)

  let result = try controller.disableAndUninstall(stopDaemonNow: false)

  #expect(result == .unchanged)
  #expect(runner.invocations == [["service", "uninstall", "--json", "--keep-running"]])
}

@Test
func disableWithStopOmitsKeepRunningFlag() throws {
  let runner = ScriptedCLIRunner(standardOutput: #"{"result":"stopped"}"#)
  let controller = VaporCLIServiceController(runner: runner)

  let result = try controller.disableAndUninstall(stopDaemonNow: true)

  #expect(result == .stopped)
  #expect(runner.invocations == [["service", "uninstall", "--json"]])
}

@Test
func deferredStartDecodesRemainingSeconds() throws {
  let runner = ScriptedCLIRunner(
    standardOutput: #"{"result":"relaunch_deferred","remaining_seconds":1.5}"#
  )
  let controller = VaporCLIServiceController(runner: runner)

  let result = try controller.startDaemon()

  #expect(result == .relaunchDeferred(1.5))
}

@Test
func crashLoopPausedActionDecodesToInfiniteDeferral() throws {
  let runner = ScriptedCLIRunner(standardOutput: #"{"result":"crash_loop_paused"}"#)
  let controller = VaporCLIServiceController(runner: runner)

  let result = try controller.startDaemon()

  #expect(result == .relaunchDeferred(.infinity))
}

@Test
func statusDecodesTheFullReport() throws {
  let runner = ScriptedCLIRunner(
    standardOutput: """
      {
        "status": "crash_loop_paused",
        "label": "sh.arn.vapor.daemon",
        "auto_launch": true,
        "crash_loop": {
          "paused": true,
          "consecutive_crashes": 5,
          "last_crash_at_ms": 1750000000000,
          "awaiting_restart": true
        }
      }
      """
  )
  let controller = VaporCLIServiceController(runner: runner)

  let snapshot = try controller.status()

  #expect(runner.invocations == [["service", "status", "--json"]])
  #expect(snapshot.status == "crash_loop_paused")
  #expect(snapshot.label == "sh.arn.vapor.daemon")
  #expect(snapshot.autoLaunchEnabled)
  #expect(snapshot.crashLoopPaused)
  #expect(snapshot.consecutiveCrashes == 5)
}

@Test
func checkDecodesEveryHealthOutcome() throws {
  let runner = ScriptedCLIRunner(standardOutput: #"{"health":"running"}"#)
  let controller = VaporCLIServiceController(runner: runner)

  #expect(try controller.checkDaemonHealth() == .running)

  runner.standardOutput = #"{"health":"not_installed"}"#
  #expect(try controller.checkDaemonHealth() == .notInstalled)

  runner.standardOutput = #"{"health":"stopped_expected"}"#
  #expect(try controller.checkDaemonHealth() == .stoppedExpected)

  runner.standardOutput = #"{"health":"restarted_after_crash"}"#
  #expect(try controller.checkDaemonHealth() == .restartedAfterCrash)

  runner.standardOutput = #"{"health":"restart_deferred","remaining_seconds":2.0}"#
  #expect(try controller.checkDaemonHealth() == .restartDeferred(2.0))

  runner.standardOutput = #"{"health":"crash_loop_paused"}"#
  #expect(try controller.checkDaemonHealth() == .crashLoopPaused)

  #expect(runner.invocations.allSatisfy { $0 == ["service", "check", "--json"] })
}

@Test
func acknowledgeValidatesTheAcknowledgedResult() throws {
  let runner = ScriptedCLIRunner(standardOutput: #"{"result":"acknowledged"}"#)
  let controller = VaporCLIServiceController(runner: runner)

  try controller.acknowledgeCrashLoopPause()

  #expect(runner.invocations == [["service", "acknowledge", "--json"]])
}

@Test
func nonZeroExitCodeSurfacesAsCommandFailed() {
  let runner = ScriptedCLIRunner(
    standardOutput: "",
    standardError: "vapor: lifecycle error: boom",
    exitCode: 1
  )
  let controller = VaporCLIServiceController(runner: runner)

  #expect(throws: VaporCLIServiceError.self) {
    try controller.startDaemon()
  }
}

@Test
func malformedJSONSurfacesAsMalformedResponse() {
  let runner = ScriptedCLIRunner(standardOutput: "not json at all")
  let controller = VaporCLIServiceController(runner: runner)

  #expect(throws: VaporCLIServiceError.self) {
    try controller.startDaemon()
  }
}

@Test
func unknownResultValueSurfacesAsMalformedResponse() {
  let runner = ScriptedCLIRunner(standardOutput: #"{"result":"quantum_flux"}"#)
  let controller = VaporCLIServiceController(runner: runner)

  #expect(throws: VaporCLIServiceError.self) {
    try controller.startDaemon()
  }
}

// MARK: - Fakes

private final class ScriptedCLIRunner: VaporCLIRunning {
  var invocations: [[String]] = []
  var standardOutput: String
  var standardError: String
  var exitCode: Int32

  init(standardOutput: String, standardError: String = "", exitCode: Int32 = 0) {
    self.standardOutput = standardOutput
    self.standardError = standardError
    self.exitCode = exitCode
  }

  func run(arguments: [String]) throws -> VaporCLIResult {
    invocations.append(arguments)
    return VaporCLIResult(
      exitCode: exitCode,
      standardOutput: standardOutput,
      standardError: standardError
    )
  }
}
