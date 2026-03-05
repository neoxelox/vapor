import Foundation

public enum DaemonLifecycleActionResult: Equatable {
  case unchanged
  case started
  case stopped
  case relaunchDeferred(TimeInterval)
}

public protocol AutoLaunchSettingStore {
  func bool(forKey key: String) -> Bool?
  func set(_ value: Bool, forKey key: String)
}

public struct UserDefaultsAutoLaunchSettingStore: AutoLaunchSettingStore {
  private let defaults: UserDefaults

  public init(defaults: UserDefaults = .standard) {
    self.defaults = defaults
  }

  public func bool(forKey key: String) -> Bool? {
    defaults.object(forKey: key) as? Bool
  }

  public func set(_ value: Bool, forKey key: String) {
    defaults.set(value, forKey: key)
  }
}

public final class InMemoryAutoLaunchSettingStore: AutoLaunchSettingStore {
  private var values: [String: Bool]

  public init(seed: [String: Bool] = [:]) {
    values = seed
  }

  public func bool(forKey key: String) -> Bool? {
    values[key]
  }

  public func set(_ value: Bool, forKey key: String) {
    values[key] = value
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

  public init(
    failureWindow: TimeInterval,
    baseDelay: TimeInterval,
    maxDelay: TimeInterval,
    delayStartsAfterFailures: Int
  ) {
    self.failureWindow = failureWindow
    self.baseDelay = baseDelay
    self.maxDelay = maxDelay
    self.delayStartsAfterFailures = max(1, delayStartsAfterFailures)
  }

  public static let `default` = CrashLoopPolicy(
    failureWindow: 300,
    baseDelay: 5,
    maxDelay: 300,
    delayStartsAfterFailures: 2
  )
}

public struct CrashLoopGuard: Sendable {
  private let policy: CrashLoopPolicy
  private var failureMoments: [Date] = []
  private var pausedUntil: Date?

  public init(policy: CrashLoopPolicy = .default) {
    self.policy = policy
  }

  public mutating func registerCrash(at now: Date) -> TimeInterval? {
    pruneFailures(relativeTo: now)
    failureMoments.append(now)

    let exponent = failureMoments.count - policy.delayStartsAfterFailures
    guard exponent >= 0 else {
      return nil
    }

    let delay = min(policy.maxDelay, policy.baseDelay * pow(2, Double(exponent)))
    pausedUntil = now.addingTimeInterval(delay)
    return delay
  }

  public mutating func remainingDelay(at now: Date) -> TimeInterval {
    guard let pausedUntil else {
      return 0
    }

    if now >= pausedUntil {
      self.pausedUntil = nil
      return 0
    }

    return pausedUntil.timeIntervalSince(now)
  }

  public mutating func reset() {
    failureMoments.removeAll(keepingCapacity: true)
    pausedUntil = nil
  }

  private mutating func pruneFailures(relativeTo now: Date) {
    let oldestAllowed = now.addingTimeInterval(-policy.failureWindow)
    failureMoments.removeAll(where: { $0 < oldestAllowed })
  }
}

public final class DaemonLifecycleManager {
  public static let autoLaunchSettingKey = "vapor.lifecycle.auto-launch-enabled"

  private let launchAgentController: LaunchAgentControlling
  private let loginItemController: (any LoginItemControlling)?
  private let settingsStore: AutoLaunchSettingStore
  private let settingsKey: String
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
      settingsStore: UserDefaultsAutoLaunchSettingStore(),
      loginItemController: nil
    )
  }

  public var autoLaunchEnabled: Bool {
    if let persisted = settingsStore.bool(forKey: settingsKey) {
      logger.debug("Read persisted auto-launch setting", metadata: ["value": String(persisted)])
      return persisted
    }

    settingsStore.set(true, forKey: settingsKey)
    logger.info("Auto-launch setting missing; defaulting to enabled")
    return true
  }

  @discardableResult
  public func bootstrapIfNeeded(now: Date = .now) throws -> DaemonLifecycleActionResult {
    guard autoLaunchEnabled else {
      logger.debug("Skipped lifecycle bootstrap because auto-launch is disabled")
      return .unchanged
    }

    try launchAgentController.installAndEnable()
    try loginItemController?.register()
    logger.info("Lifecycle bootstrap completed; attempting daemon start")
    return try startDaemonIfAllowed(now: now)
  }

  @discardableResult
  public func setAutoLaunchEnabled(
    _ enabled: Bool,
    stopDaemonNow: Bool = false,
    now: Date = .now
  ) throws -> DaemonLifecycleActionResult {
    settingsStore.set(enabled, forKey: settingsKey)
    logger.info(
      "Updated auto-launch setting",
      metadata: ["enabled": String(enabled), "stop_now": String(stopDaemonNow)]
    )

    if enabled {
      try launchAgentController.installAndEnable()
      try loginItemController?.register()
      return try startDaemonIfAllowed(now: now)
    }

    try launchAgentController.disableAndUninstall()
    try loginItemController?.unregister()
    crashLoopGuard.reset()
    logger.warning("Disabled auto-launch and reset crash-loop guard")

    if stopDaemonNow {
      try launchAgentController.stopDaemon()
      logger.warning("Daemon stop requested due to stop-now disable flow")
      return .stopped
    }

    return .unchanged
  }

  public func registerUnexpectedDaemonExit(now: Date = .now) -> TimeInterval? {
    let delay = crashLoopGuard.registerCrash(at: now)
    logger.warning(
      "Registered unexpected daemon exit",
      metadata: ["relaunch_delay_seconds": String(delay ?? 0)]
    )
    return delay
  }

  @discardableResult
  public func startDaemonIfAllowed(now: Date = .now) throws -> DaemonLifecycleActionResult {
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

  public func stopDaemonForTermination() throws {
    try launchAgentController.stopDaemon()
    logger.warning("Requested daemon stop for app termination")
  }
}
