import Foundation

public enum DaemonLifecycleActionResult: Equatable {
  case unchanged
  case started
  case stopped
  case relaunchDeferred(TimeInterval)
}

public protocol AutoLaunchSettingStore {
  func bool(forKey key: String) -> Bool?
  func set(_ value: Bool, forKey key: String) throws
}

public final class InMemoryAutoLaunchSettingStore: AutoLaunchSettingStore {
  private var values: [String: Bool]

  public init(seed: [String: Bool] = [:]) {
    values = seed
  }

  public func bool(forKey key: String) -> Bool? {
    values[key]
  }

  public func set(_ value: Bool, forKey key: String) throws {
    values[key] = value
  }
}

public final class VaporConfigurationAutoLaunchSettingStore: AutoLaunchSettingStore {
  private let configurationStore: VaporConfigurationStore

  public init(configurationStore: VaporConfigurationStore = VaporConfigurationStore()) {
    self.configurationStore = configurationStore
  }

  public func bool(forKey key: String) -> Bool? {
    guard key == DaemonLifecycleManager.autoLaunchSettingKey else {
      return nil
    }

    return configurationStore.load().autoLaunch
  }

  public func set(_ value: Bool, forKey key: String) throws {
    guard key == DaemonLifecycleManager.autoLaunchSettingKey else {
      return
    }

    var configuration = configurationStore.load()
    configuration.autoLaunch = value
    try configurationStore.save(configuration)
  }
}

public protocol LaunchAgentControlling {
  func installAndEnable() throws
  func disableAndUninstall() throws
  func startDaemon() throws
  func stopDaemon() throws
}

public protocol LoginItemControlling {
  func register() throws
  func unregister() throws
}

public struct NoopLoginItemController: LoginItemControlling {
  public init() {}

  public func register() throws {}

  public func unregister() throws {}
}

public struct NoopLaunchAgentController: LaunchAgentControlling {
  public init() {}

  public func installAndEnable() throws {}

  public func disableAndUninstall() throws {}

  public func startDaemon() throws {}

  public func stopDaemon() throws {}
}

public struct CrashLoopPolicy: Equatable, Sendable {
  public var failureWindow: TimeInterval
  public var baseDelay: TimeInterval
  public var maxDelay: TimeInterval
  public var delayStartsAfterFailures: Int
  public var maxConsecutiveFailuresBeforePause: Int

  public init(
    failureWindow: TimeInterval,
    baseDelay: TimeInterval,
    maxDelay: TimeInterval,
    delayStartsAfterFailures: Int,
    maxConsecutiveFailuresBeforePause: Int
  ) {
    self.failureWindow = failureWindow
    self.baseDelay = baseDelay
    self.maxDelay = maxDelay
    self.delayStartsAfterFailures = max(1, delayStartsAfterFailures)
    self.maxConsecutiveFailuresBeforePause = max(1, maxConsecutiveFailuresBeforePause)
  }

  public static let `default` = CrashLoopPolicy(
    failureWindow: 600,
    baseDelay: 2,
    maxDelay: 120,
    delayStartsAfterFailures: 1,
    maxConsecutiveFailuresBeforePause: 5
  )
}

public enum CrashLoopDecision: Equatable, Sendable {
  case noDelay
  case backoff(TimeInterval)
  case paused
}

public struct CrashLoopGuard: Sendable {
  private let policy: CrashLoopPolicy
  private var failureMoments: [Date] = []
  private var pausedUntil: Date?
  private var pausedIndefinitely: Bool = false

  public init(policy: CrashLoopPolicy = .default) {
    self.policy = policy
  }

  public var isPausedIndefinitely: Bool {
    pausedIndefinitely
  }

  /// Mirrors the canonical Rust `CrashLoopGuard::register_crash` in
  /// `core/lifecycle/src/crash_loop.rs` (parity contract:
  /// `core/lifecycle/tests/crash_loop_parity.rs`). With the default
  /// policy the schedule is: crash 1 → restart immediately, crash 2 →
  /// 2 s, crash 3 → 4 s, crash 4 → 8 s, crash 5 → paused.
  /// `delayStartsAfterFailures = N` means the first N crashes within the
  /// window are delay-free.
  @discardableResult
  public mutating func registerCrash(at now: Date) -> CrashLoopDecision {
    pruneFailures(relativeTo: now)
    failureMoments.append(now)

    if failureMoments.count >= policy.maxConsecutiveFailuresBeforePause {
      pausedIndefinitely = true
      pausedUntil = nil
      return .paused
    }

    let exponent = failureMoments.count - policy.delayStartsAfterFailures - 1
    guard exponent >= 0 else {
      return .noDelay
    }

    let delay = min(policy.maxDelay, policy.baseDelay * pow(2, Double(exponent)))
    pausedUntil = now.addingTimeInterval(delay)
    return .backoff(delay)
  }

  public mutating func remainingDelay(at now: Date) -> TimeInterval {
    if pausedIndefinitely {
      return .infinity
    }

    guard let pausedUntil else {
      return 0
    }

    if now >= pausedUntil {
      self.pausedUntil = nil
      return 0
    }

    return pausedUntil.timeIntervalSince(now)
  }

  public mutating func acknowledgeAndResume() {
    pausedIndefinitely = false
    failureMoments.removeAll(keepingCapacity: true)
    pausedUntil = nil
  }

  public mutating func reset() {
    failureMoments.removeAll(keepingCapacity: true)
    pausedUntil = nil
    pausedIndefinitely = false
  }

  private mutating func pruneFailures(relativeTo now: Date) {
    let oldestAllowed = now.addingTimeInterval(-policy.failureWindow)
    failureMoments.removeAll(where: { $0 < oldestAllowed })
  }
}

public final class DaemonLifecycleManager: @unchecked Sendable {
  public static let autoLaunchSettingKey = "vapor.lifecycle.auto-launch-enabled"

  private let launchAgentController: LaunchAgentControlling
  private let loginItemController: (any LoginItemControlling)?
  private let settingsStore: AutoLaunchSettingStore
  private let settingsKey: String
  private let stateQueue = DispatchQueue(label: "sh.arn.vapor.daemon-lifecycle.state")
  private var crashLoopGuard: CrashLoopGuard
  private let logger: StructuredLogger

  public init(
    launchAgentController: LaunchAgentControlling,
    settingsStore: AutoLaunchSettingStore,
    loginItemController: (any LoginItemControlling)? = nil,
    settingsKey: String = DaemonLifecycleManager.autoLaunchSettingKey,
    crashLoopPolicy: CrashLoopPolicy = .default,
    logger: StructuredLogger = StructuredLogger(component: "daemon-lifecycle")
  ) {
    self.launchAgentController = launchAgentController
    self.loginItemController = loginItemController
    self.settingsStore = settingsStore
    self.settingsKey = settingsKey
    self.logger = logger
    crashLoopGuard = CrashLoopGuard(policy: crashLoopPolicy)
  }

  public static func placeholder() -> DaemonLifecycleManager {
    DaemonLifecycleManager(
      launchAgentController: NoopLaunchAgentController(),
      settingsStore: InMemoryAutoLaunchSettingStore(),
      loginItemController: nil
    )
  }

  public var autoLaunchEnabled: Bool {
    stateQueue.sync {
      autoLaunchEnabledLocked()
    }
  }

  @discardableResult
  public func bootstrapIfNeeded(now: Date = .now) throws -> DaemonLifecycleActionResult {
    try stateQueue.sync {
      guard autoLaunchEnabledLocked() else {
        logger.debug("Skipped lifecycle bootstrap because auto-launch is disabled")
        return .unchanged
      }

      try launchAgentController.installAndEnable()
      registerLoginItemIfAvailable()
      logger.info("Lifecycle bootstrap completed; attempting daemon start")
      return try startDaemonIfAllowedLocked(now: now)
    }
  }

  @discardableResult
  public func setAutoLaunchEnabled(
    _ enabled: Bool,
    stopDaemonNow: Bool = false,
    now: Date = .now
  ) throws -> DaemonLifecycleActionResult {
    try stateQueue.sync {
      try settingsStore.set(enabled, forKey: settingsKey)
      logger.info(
        "Updated auto-launch setting",
        metadata: ["enabled": String(enabled), "stop_now": String(stopDaemonNow)]
      )

      if enabled {
        try launchAgentController.installAndEnable()
        registerLoginItemIfAvailable()
        return try startDaemonIfAllowedLocked(now: now)
      }

      try launchAgentController.disableAndUninstall()
      unregisterLoginItemIfAvailable()
      crashLoopGuard.reset()
      logger.warning("Disabled auto-launch and reset crash-loop guard")

      if stopDaemonNow {
        try launchAgentController.stopDaemon()
        logger.warning("Daemon stop requested due to stop-now disable flow")
        return .stopped
      }

      return .unchanged
    }
  }

  @discardableResult
  public func registerUnexpectedDaemonExit(now: Date = .now) -> CrashLoopDecision {
    stateQueue.sync {
      let decision = crashLoopGuard.registerCrash(at: now)
      switch decision {
      case .noDelay:
        logger.warning(
          "Registered unexpected daemon exit",
          metadata: ["relaunch_delay_seconds": "0"]
        )
      case .backoff(let seconds):
        logger.warning(
          "Registered unexpected daemon exit",
          metadata: ["relaunch_delay_seconds": String(seconds)]
        )
      case .paused:
        logger.error(
          "Daemon entered crash-loop paused state; auto-restart is suspended until user acknowledges",
          metadata: [
            "failure_window_seconds": String(
              crashLoopGuard.isPausedIndefinitely
                ? DaemonLifecycleManager.crashLoopPauseSurfaceValue : 0)
          ]
        )
      }
      return decision
    }
  }

  public var isInCrashLoopPause: Bool {
    stateQueue.sync {
      crashLoopGuard.isPausedIndefinitely
    }
  }

  public func acknowledgeCrashLoopPause() {
    stateQueue.sync {
      crashLoopGuard.acknowledgeAndResume()
      logger.warning("Acknowledged crash-loop pause; auto-restart may proceed again")
    }
  }

  fileprivate static let crashLoopPauseSurfaceValue: TimeInterval = -1

  @discardableResult
  public func startDaemonIfAllowed(now: Date = .now) throws -> DaemonLifecycleActionResult {
    try stateQueue.sync {
      try startDaemonIfAllowedLocked(now: now)
    }
  }

  public func stopDaemonForTermination() throws {
    try stateQueue.sync {
      try launchAgentController.stopDaemon()
      logger.warning("Requested daemon stop for app termination")
    }
  }

  private func autoLaunchEnabledLocked() -> Bool {
    if let persisted = settingsStore.bool(forKey: settingsKey) {
      logger.debug("Read persisted auto-launch setting", metadata: ["value": String(persisted)])
      return persisted
    }

    do {
      try settingsStore.set(true, forKey: settingsKey)
    } catch {
      logger.error(
        "Failed to persist default auto-launch setting",
        metadata: ["error": String(describing: error)]
      )
    }
    logger.info("Auto-launch setting missing; defaulting to enabled")
    return true
  }

  @discardableResult
  private func startDaemonIfAllowedLocked(now: Date) throws -> DaemonLifecycleActionResult {
    if crashLoopGuard.isPausedIndefinitely {
      logger.error(
        "Refusing to start daemon while crash-loop pause is active; awaiting user acknowledgement"
      )
      return .relaunchDeferred(.infinity)
    }

    let remaining = crashLoopGuard.remainingDelay(at: now)
    guard remaining <= 0 else {
      logger.warning(
        "Deferred daemon relaunch due to crash-loop policy",
        metadata: ["remaining_seconds": String(remaining)]
      )
      return .relaunchDeferred(remaining)
    }

    try launchAgentController.startDaemon()
    logger.info("Requested daemon start")
    return .started
  }

  private func registerLoginItemIfAvailable() {
    do {
      try loginItemController?.register()
    } catch {
      logger.warning(
        "Failed to register app login item",
        metadata: ["error": String(describing: error)]
      )
    }
  }

  private func unregisterLoginItemIfAvailable() {
    do {
      try loginItemController?.unregister()
    } catch {
      logger.warning(
        "Failed to unregister app login item",
        metadata: ["error": String(describing: error)]
      )
    }
  }
}
