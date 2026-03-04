import Darwin
import Foundation

public struct LaunchAgentConfiguration: Equatable, Sendable {
  public var label: String
  public var plistURL: URL
  public var daemonExecutableURL: URL
  public var daemonArguments: [String]
  public var runAtLoad: Bool
  public var keepAlive: Bool
  public var workingDirectoryURL: URL?
  public var environment: [String: String]

  public init(
    label: String,
    plistURL: URL,
    daemonExecutableURL: URL,
    daemonArguments: [String] = [],
    runAtLoad: Bool = true,
    keepAlive: Bool = false,
    workingDirectoryURL: URL? = nil,
    environment: [String: String] = [:]
  ) {
    self.label = label
    self.plistURL = plistURL
    self.daemonExecutableURL = daemonExecutableURL
    self.daemonArguments = daemonArguments
    self.runAtLoad = runAtLoad
    self.keepAlive = keepAlive
    self.workingDirectoryURL = workingDirectoryURL
    self.environment = environment
  }

  public static func defaultPlistURL(
    label: String, homeDirectoryURL: URL = FileManager.default.homeDirectoryForCurrentUser
  ) -> URL {
    homeDirectoryURL
      .appendingPathComponent("Library")
      .appendingPathComponent("LaunchAgents")
      .appendingPathComponent("\(label).plist")
  }
}

public struct LaunchctlCommandResult: Equatable, Sendable {
  public var exitCode: Int32
  public var standardError: String

  public init(exitCode: Int32, standardError: String) {
    self.exitCode = exitCode
    self.standardError = standardError
  }
}

public protocol LaunchctlCommandRunning {
  func run(arguments: [String]) throws -> LaunchctlCommandResult
}

public enum LaunchAgentControllerError: Error, Equatable {
  case launchctlFailed(arguments: [String], exitCode: Int32, standardError: String)
  case plistEncodingFailed
}

public struct ProcessLaunchctlRunner: LaunchctlCommandRunning {
  public init() {}

  public func run(arguments: [String]) throws -> LaunchctlCommandResult {
    let process = Process()
    process.executableURL = URL(fileURLWithPath: "/bin/launchctl")
    process.arguments = arguments

    let errorPipe = Pipe()
    process.standardError = errorPipe

    try process.run()
    process.waitUntilExit()

    let data = errorPipe.fileHandleForReading.readDataToEndOfFile()
    let message = String(data: data, encoding: .utf8) ?? ""

    return LaunchctlCommandResult(exitCode: process.terminationStatus, standardError: message)
  }
}

public final class LaunchAgentController: LaunchAgentControlling {
  private let configuration: LaunchAgentConfiguration
  private let runner: any LaunchctlCommandRunning
  private let fileManager: FileManager
  private let userID: UInt32

  public init(
    configuration: LaunchAgentConfiguration,
    runner: any LaunchctlCommandRunning = ProcessLaunchctlRunner(),
    fileManager: FileManager = .default,
    userID: UInt32 = getuid()
  ) {
    self.configuration = configuration
    self.runner = runner
    self.fileManager = fileManager
    self.userID = userID
  }

  public func installAndEnable() throws {
    try writeLaunchAgentPlist()

    _ = try? runBestEffort(arguments: ["bootout", domainTarget, configuration.plistURL.path])
    try runRequired(arguments: ["bootstrap", domainTarget, configuration.plistURL.path])
    _ = try? runBestEffort(arguments: ["enable", serviceTarget])
  }

  public func disableAndUninstall() throws {
    _ = try? runBestEffort(arguments: ["disable", serviceTarget])
    _ = try? runBestEffort(arguments: ["bootout", domainTarget, configuration.plistURL.path])

    guard fileManager.fileExists(atPath: configuration.plistURL.path) else {
      return
    }

    try fileManager.removeItem(at: configuration.plistURL)
  }

  public func startDaemon() throws {
    try runRequired(arguments: ["kickstart", "-k", serviceTarget])
  }

  public func stopDaemon() throws {
    _ = try? runBestEffort(arguments: ["kill", "TERM", serviceTarget])
  }

  private var domainTarget: String {
    "gui/\(userID)"
  }

  private var serviceTarget: String {
    "\(domainTarget)/\(configuration.label)"
  }

  private func writeLaunchAgentPlist() throws {
    let launchAgentsURL = configuration.plistURL.deletingLastPathComponent()
    try fileManager.createDirectory(at: launchAgentsURL, withIntermediateDirectories: true)

    guard let plistData = try launchAgentPlistData() else {
      throw LaunchAgentControllerError.plistEncodingFailed
    }

    try plistData.write(to: configuration.plistURL, options: .atomic)
  }

  private func launchAgentPlistData() throws -> Data? {
    let programArguments = [configuration.daemonExecutableURL.path] + configuration.daemonArguments

    var plist: [String: Any] = [
      "Label": configuration.label,
      "ProgramArguments": programArguments,
      "RunAtLoad": configuration.runAtLoad,
      "KeepAlive": configuration.keepAlive,
    ]

    if let workingDirectoryURL = configuration.workingDirectoryURL {
      plist["WorkingDirectory"] = workingDirectoryURL.path
    }

    if !configuration.environment.isEmpty {
      plist["EnvironmentVariables"] = configuration.environment
    }

    return try PropertyListSerialization.data(fromPropertyList: plist, format: .xml, options: 0)
  }

  @discardableResult
  private func runRequired(arguments: [String]) throws -> LaunchctlCommandResult {
    let result = try runner.run(arguments: arguments)
    guard result.exitCode == 0 else {
      throw LaunchAgentControllerError.launchctlFailed(
        arguments: arguments,
        exitCode: result.exitCode,
        standardError: result.standardError
      )
    }

    return result
  }

  @discardableResult
  private func runBestEffort(arguments: [String]) throws -> LaunchctlCommandResult {
    try runner.run(arguments: arguments)
  }
}
