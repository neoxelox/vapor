import Foundation

@MainActor
public protocol AppRuntimeControlling {
  func setDockVisible(_ isVisible: Bool)
  func terminateApplication()
}

@MainActor
public struct NoopAppRuntimeController: AppRuntimeControlling {
  public init() {}

  public func setDockVisible(_: Bool) {}

  public func terminateApplication() {}
}

@MainActor
public final class AppLifecycleCoordinator {
  private let daemonLifecycleManager: DaemonLifecycleManager
  private let runtimeController: any AppRuntimeControlling
  private let logger: StructuredLogger

  public init(
    daemonLifecycleManager: DaemonLifecycleManager,
    runtimeController: any AppRuntimeControlling,
    logger: StructuredLogger = StructuredLogger(component: "app-lifecycle-coordinator")
  ) {
    self.daemonLifecycleManager = daemonLifecycleManager
    self.runtimeController = runtimeController
    self.logger = logger
  }

  public func handleMainWindowClosed() {
    runtimeController.setDockVisible(false)
    logger.info("Main window closed; switched to menubar-only surface")
  }

  public func handleOpenFromMenuBar() {
    runtimeController.setDockVisible(true)
    logger.info("Open Vapor requested from menubar")
  }

  /// Stops the daemon and then terminates the app. The stop runs on
  /// `queue` — the caller's single lifecycle-operation queue — so it
  /// serializes behind any pending auto-launch toggle (a queued
  /// `installAndEnable` must not start the daemon *after* this stop) and
  /// never blocks the main actor; the whole path is bounded by the CLI's
  /// own subprocess timeout. `terminateApplication()` runs afterward on
  /// the main actor, so quit is always the last lifecycle operation.
  public func handleQuitFromMenuBar(serializingOn queue: DispatchQueue) async {
    let manager = daemonLifecycleManager
    let logger = self.logger
    await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
      queue.async {
        do {
          try manager.stopDaemonForTermination()
        } catch {
          logger.error(
            "Daemon stop request failed during quit",
            metadata: ["error": String(describing: error)]
          )
        }
        continuation.resume()
      }
    }

    runtimeController.terminateApplication()
    logger.warning("Quit requested from menubar; app termination requested")
  }
}
