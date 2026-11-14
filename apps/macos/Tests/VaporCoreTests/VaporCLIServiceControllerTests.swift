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
    standardOutput: #"{"result":"relaunch-deferred","remainingSeconds":1.5}"#
  )
  let controller = VaporCLIServiceController(runner: runner)

  let result = try controller.startDaemon()

  #expect(result == .relaunchDeferred(1.5))
}

@Test
func crashLoopPausedActionDecodesToInfiniteDeferral() throws {
  let runner = ScriptedCLIRunner(standardOutput: #"{"result":"crash-loop-paused"}"#)
  let controller = VaporCLIServiceController(runner: runner)

  let result = try controller.startDaemon()

  #expect(result == .relaunchDeferred(.infinity))
}

@Test
func statusDecodesTheFullReport() throws {
  let runner = ScriptedCLIRunner(
    standardOutput: """
      {
        "status": "crash-loop-paused",
        "label": "sh.arn.vapor.daemon",
        "autoLaunch": true,
        "crashLoop": {
          "paused": true,
          "consecutiveCrashes": 5,
          "lastCrashAtMs": 1750000000000,
          "awaitingRestart": true
        }
      }
      """
  )
  let controller = VaporCLIServiceController(runner: runner)

  let snapshot = try controller.status()

  #expect(runner.invocations == [["service", "status", "--json"]])
  #expect(snapshot.status == "crash-loop-paused")
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

  runner.standardOutput = #"{"health":"not-installed"}"#
  #expect(try controller.checkDaemonHealth() == .notInstalled)

  runner.standardOutput = #"{"health":"stopped-expected"}"#
  #expect(try controller.checkDaemonHealth() == .stoppedExpected)

  runner.standardOutput = #"{"health":"restarted-after-crash"}"#
  #expect(try controller.checkDaemonHealth() == .restartedAfterCrash)

  runner.standardOutput = #"{"health":"restart-deferred","remainingSeconds":2.0}"#
  #expect(try controller.checkDaemonHealth() == .restartDeferred(2.0))

  runner.standardOutput = #"{"health":"crash-loop-paused"}"#
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
  /// The bound each invocation asked for; `nil` for the default one.
  var timeouts: [Int?] = []
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
    timeouts.append(nil)
    return VaporCLIResult(
      exitCode: exitCode,
      standardOutput: standardOutput,
      standardError: standardError
    )
  }

  func run(arguments: [String], timeoutSeconds: Int) throws -> VaporCLIResult {
    let result = try run(arguments: arguments)
    timeouts[timeouts.count - 1] = timeoutSeconds
    return result
  }
}

@Test
func daemonStatusDecodesTheCountsTheAppRenders() throws {
  // Mirrors the `status_json_shape_is_stable` lock in
  // core/cli/src/commands/ipc.rs; unknown fields are ignored.
  let runner = ScriptedCLIRunner(
    standardOutput: """
      {
        "schemaVersion": 4,
        "daemon": "vapord/0.0.0-test",
        "provider": "filesystem",
        "run": {"state": "running", "reason": "watching ~/Vapor"},
        "throttle": {"state": "idle", "reason": "idle, plugged in, and cool"},
        "reconcile": {"state": "idle", "detail": ""},
        "queued": 3,
        "failed": 1,
        "decisions": 2,
        "mirror": {"reverts": 0, "deletes": 0},
        "restartRequired": "syncMode changed; restart the daemon to apply (vapor service restart)",
        "resources": null,
        "profiles": []
      }
      """)
  let controller = VaporCLIServiceController(runner: runner)

  let snapshot = try controller.daemonStatus()

  #expect(runner.invocations == [["status", "--json"]])
  #expect(snapshot.runState == "running")
  #expect(snapshot.runReason == "watching ~/Vapor")
  #expect(snapshot.throttleState == "idle")
  #expect(snapshot.throttleReason == "idle, plugged in, and cool")
  #expect(snapshot.queued == 3)
  #expect(snapshot.failed == 1)
  #expect(snapshot.restartRequired?.hasPrefix("syncMode changed") == true)
  #expect(snapshot.decisions == 2)
}

@Test
func daemonStatusTreatsMissingCountsAsZero() throws {
  let runner = ScriptedCLIRunner(
    standardOutput: """
      {"provider":"filesystem","run":{"state":"running"},"throttle":{"state":"light"}}
      """)
  let controller = VaporCLIServiceController(runner: runner)

  let snapshot = try controller.daemonStatus()

  #expect(snapshot.decisions == 0)
  #expect(snapshot.queued == 0)
  #expect(snapshot.runReason == nil)
}

@Test
func restartDecodesNotInstalledWithoutTreatingItAsAFailure() throws {
  let runner = ScriptedCLIRunner(standardOutput: #"{"result":"not-installed"}"#)
  let controller = VaporCLIServiceController(runner: runner)

  let result = try controller.restartDaemon()

  #expect(result == .notInstalled)
  #expect(runner.invocations == [["service", "restart", "--json"]])
}

@Test
func daemonStatusDecodesTheScanStateAndReconcileDrivesTheCLI() throws {
  let runner = ScriptedCLIRunner(
    standardOutput: """
      {
        "provider": "filesystem",
        "run": {"state": "running"},
        "throttle": {"state": "throttled", "reason": "user activity is active"},
        "queued": 1,
        "failed": 0,
        "reconcile": {"state": "waiting", "detail": "waiting for an idle moment: user activity is active"}
      }
      """)
  let controller = VaporCLIServiceController(runner: runner)

  let snapshot = try controller.daemonStatus()
  #expect(snapshot.scanIsWaiting)
  #expect(!snapshot.scanIsRunning)
  #expect(snapshot.reconcileDetail == "waiting for an idle moment: user activity is active")

  runner.standardOutput = #"{"schemaVersion":4,"accepted":true,"note":"reconcile requested"}"#
  try controller.reconcile()
  #expect(runner.invocations.last == ["reconcile", "--json"])

  runner.standardOutput = #"{"schemaVersion":4,"accepted":false,"note":"unsupported"}"#
  #expect(throws: (any Error).self) {
    try controller.reconcile()
  }

  // A refusal exits non-zero; the daemon's note still reaches the error.
  runner.exitCode = 1
  runner.standardOutput = #"{"schemaVersion":4,"accepted":false,"note":"profile is paused"}"#
  do {
    try controller.reconcile()
    Issue.record("a refusal must throw")
  } catch {
    #expect(String(describing: error).contains("profile is paused"))
  }
}

@Test
func daemonStatusListsTheProfilesWaitingOnASignIn() throws {
  let runner = ScriptedCLIRunner(
    standardOutput: """
      {
        "provider": "gdrive",
        "run": {"state": "error", "reason": "Vapor needs to sign in to gdrive again"},
        "throttle": {"state": "light"},
        "queued": 2,
        "failed": 0,
        "profiles": [
          {"id": "docs", "provider": "filesystem", "run": {"state": "running"}},
          {"id": "default", "provider": "gdrive", "run": {"state": "error"}, "signInRequired": true}
        ]
      }
      """)
  let controller = VaporCLIServiceController(runner: runner)

  let snapshot = try controller.daemonStatus()

  #expect(snapshot.signInRequired == [SignInRequest(providerKind: "gdrive", profileId: "default")])
}

@Test
func signInRunsTheBrowserLoginWithTheLongerBound() throws {
  let runner = ScriptedCLIRunner(
    standardOutput: "Signed profile work in to gdrive.")
  let controller = VaporCLIServiceController(runner: runner)

  try controller.signIn(profile: "work")

  #expect(runner.invocations == [["auth", "login", "--profile", "work", "--browser"]])
  let bound = try #require(runner.timeouts.first ?? nil)
  #expect(bound > VaporConstants.Provider.browserSignInTimeoutSeconds)

  runner.exitCode = 1
  runner.standardError = "vapor: authorization was denied by the user: access_denied"
  #expect(throws: VaporCLIServiceError.self) {
    try controller.signIn(profile: "work")
  }
}
