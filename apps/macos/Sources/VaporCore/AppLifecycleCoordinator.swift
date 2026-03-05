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

  public func handleQuitFromMenuBar() {
    do {
      try daemonLifecycleManager.stopDaemonForTermination()
    } catch {
      logger.error(
        "Daemon stop request failed during quit",
        metadata: ["error": String(describing: error)]
      )
    }

    runtimeController.terminateApplication()
    logger.warning("Quit requested from menubar; app termination requested")
  }
}
