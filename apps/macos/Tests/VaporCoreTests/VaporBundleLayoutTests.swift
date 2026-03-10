import Foundation
import Testing

@testable import VaporCore

@Test
func bundledDaemonExecutableUsesSiblingOfRunningExecutable() {
  let bundleURL = URL(fileURLWithPath: "/Applications/Vapor.app", isDirectory: true)
  let executableURL =
    bundleURL
    .appendingPathComponent("Contents", isDirectory: true)
    .appendingPathComponent("MacOS", isDirectory: true)
    .appendingPathComponent("Vapor")

  let daemonURL = VaporBundleLayout.bundledDaemonExecutableURL(
    bundleURL: bundleURL,
    executableURL: executableURL
  )

  #expect(daemonURL.path == "/Applications/Vapor.app/Contents/MacOS/vapord")
}

@Test
func bundledDaemonExecutableFallsBackToBundleLayoutWhenExecutableURLMissing() {
  let bundleURL = URL(fileURLWithPath: "/Applications/Vapor.app", isDirectory: true)

  let daemonURL = VaporBundleLayout.bundledDaemonExecutableURL(
    bundleURL: bundleURL,
    executableURL: nil
  )

  #expect(daemonURL.path == "/Applications/Vapor.app/Contents/MacOS/vapord")
}

@Test
func validateRequiredExecutablesRejectsMissingDaemonBinary() throws {
  let fileManager = FileManager.default
  let bundleURL = try makeBundleRoot(fileManager: fileManager)
  defer { try? fileManager.removeItem(at: bundleURL.deletingLastPathComponent()) }

  try makeExecutable(
    at: bundleURL.appendingPathComponent(VaporBundleLayout.appExecutableRelativePath),
    fileManager: fileManager
  )

  #expect(
    throws: VaporBundleLayout.ValidationError.missingExecutable(
      path: bundleURL.appendingPathComponent(VaporBundleLayout.daemonExecutableRelativePath).path
    )
  ) {
    try VaporBundleLayout.validateRequiredExecutables(in: bundleURL, fileManager: fileManager)
  }
}

@Test
func validateRequiredExecutablesRejectsNonExecutableDaemonBinary() throws {
  let fileManager = FileManager.default
  let bundleURL = try makeBundleRoot(fileManager: fileManager)
  defer { try? fileManager.removeItem(at: bundleURL.deletingLastPathComponent()) }

  try makeExecutable(
    at: bundleURL.appendingPathComponent(VaporBundleLayout.appExecutableRelativePath),
    fileManager: fileManager
  )
  try makeFile(
    at: bundleURL.appendingPathComponent(VaporBundleLayout.daemonExecutableRelativePath),
    permissions: 0o644,
    fileManager: fileManager
  )

  #expect(
    throws: VaporBundleLayout.ValidationError.executableNotExecutable(
      path: bundleURL.appendingPathComponent(VaporBundleLayout.daemonExecutableRelativePath).path
    )
  ) {
    try VaporBundleLayout.validateRequiredExecutables(in: bundleURL, fileManager: fileManager)
  }
}

@Test
func validateRequiredExecutablesAcceptsAppBundleWithBothBinaries() throws {
  let fileManager = FileManager.default
  let bundleURL = try makeBundleRoot(fileManager: fileManager)
  defer { try? fileManager.removeItem(at: bundleURL.deletingLastPathComponent()) }

  try makeExecutable(
    at: bundleURL.appendingPathComponent(VaporBundleLayout.appExecutableRelativePath),
    fileManager: fileManager
  )
  try makeExecutable(
    at: bundleURL.appendingPathComponent(VaporBundleLayout.daemonExecutableRelativePath),
    fileManager: fileManager
  )

  try VaporBundleLayout.validateRequiredExecutables(in: bundleURL, fileManager: fileManager)
}

private func makeBundleRoot(fileManager: FileManager) throws -> URL {
  let rootURL = fileManager.temporaryDirectory
    .appendingPathComponent("vapor-bundle-layout-tests")
    .appendingPathComponent(UUID().uuidString, isDirectory: true)
  let bundleURL = rootURL.appendingPathComponent("Vapor.app", isDirectory: true)
  try fileManager.createDirectory(at: bundleURL, withIntermediateDirectories: true)
  return bundleURL
}

private func makeExecutable(at url: URL, fileManager: FileManager) throws {
  try makeFile(at: url, permissions: 0o755, fileManager: fileManager)
}

private func makeFile(at url: URL, permissions: Int16, fileManager: FileManager) throws {
  try fileManager.createDirectory(
    at: url.deletingLastPathComponent(),
    withIntermediateDirectories: true
  )
  try Data("#!/bin/sh\n".utf8).write(to: url)
  try fileManager.setAttributes(
    [.posixPermissions: NSNumber(value: permissions)], ofItemAtPath: url.path)
}
