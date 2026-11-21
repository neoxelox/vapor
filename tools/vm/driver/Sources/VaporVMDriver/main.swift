// The Tier VM driver: reads and acts on the controls of an app through
// the macOS accessibility API, the way a user's pointer does. Every
// command prints one JSON value on standard output.
//
// It needs the Accessibility, Screen Recording, and input event
// permissions, which a test guest grants to commands that its guest
// agent starts. It is not meant to run on a contributor's Mac.
//
//   vapor-vm-driver permissions
//   vapor-vm-driver apps
//   vapor-vm-driver windows
//   vapor-vm-driver tree  <app>
//   vapor-vm-driver find  <app> <selector> [--within <selector>]
//   vapor-vm-driver press <app> <selector> [--within <selector>]
//   vapor-vm-driver click <app> <selector> [--within <selector>]
//   vapor-vm-driver notifications
//   vapor-vm-driver close-notifications
//   vapor-vm-driver allow-screen-capture
//
// <app> is a bundle identifier, `name:<process name>`, or `pid:<n>`.
// <selector> is `<attribute>=<text>` for the whole text of an
// attribute, or `<attribute>~=<text>` for a part of it. The attribute
// is `id`, `title`, `desc`, `value`, `role`, or `subrole`.
import AppKit
import ApplicationServices

// MARK: - Exit codes

enum Exit: Int32 {
  case usage = 2
  case appNotRunning = 3
  case notFound = 4
  case actionFailed = 5
}

func fail(_ code: Exit, _ message: String) -> Never {
  FileHandle.standardError.write(Data("vapor-vm-driver: \(message)\n".utf8))
  exit(code.rawValue)
}

func printJSON(_ value: Any) {
  let options: JSONSerialization.WritingOptions = [.sortedKeys, .withoutEscapingSlashes]
  guard let data = try? JSONSerialization.data(withJSONObject: value, options: options) else {
    fail(.actionFailed, "could not encode the result")
  }
  FileHandle.standardOutput.write(data)
  FileHandle.standardOutput.write(Data("\n".utf8))
}

// MARK: - Accessibility reads

func attribute(_ element: AXUIElement, _ name: String) -> AnyObject? {
  var value: AnyObject?
  let status = AXUIElementCopyAttributeValue(element, name as CFString, &value)
  return status == .success ? value : nil
}

func text(_ element: AXUIElement, _ name: String) -> String? {
  guard let value = attribute(element, name) else { return nil }
  if let string = value as? String { return string.isEmpty ? nil : string }
  if let number = value as? NSNumber { return number.stringValue }
  return nil
}

func frame(_ element: AXUIElement) -> CGRect? {
  guard let position = attribute(element, kAXPositionAttribute),
    let size = attribute(element, kAXSizeAttribute),
    CFGetTypeID(position) == AXValueGetTypeID(), CFGetTypeID(size) == AXValueGetTypeID()
  else { return nil }
  var point = CGPoint.zero
  var extent = CGSize.zero
  // The type check above makes both casts safe.
  AXValueGetValue(position as! AXValue, .cgPoint, &point)
  AXValueGetValue(size as! AXValue, .cgSize, &extent)
  return CGRect(origin: point, size: extent)
}

func children(_ element: AXUIElement) -> [AXUIElement] {
  (attribute(element, kAXChildrenAttribute) as? [AXUIElement]) ?? []
}

func actions(_ element: AXUIElement) -> [String] {
  var names: CFArray?
  guard AXUIElementCopyActionNames(element, &names) == .success else { return [] }
  return (names as? [String]) ?? []
}

/// What one control looks like to a scenario.
func describe(_ element: AXUIElement, depth: Int) -> [String: Any] {
  var node: [String: Any] = [
    "depth": depth,
    "role": text(element, kAXRoleAttribute) ?? "",
  ]
  if let subrole = text(element, kAXSubroleAttribute) { node["subrole"] = subrole }
  if let identifier = text(element, kAXIdentifierAttribute) { node["id"] = identifier }
  if let title = text(element, kAXTitleAttribute) { node["title"] = title }
  if let label = text(element, kAXDescriptionAttribute) { node["desc"] = label }
  if let value = text(element, kAXValueAttribute) { node["value"] = value }
  if let enabled = attribute(element, kAXEnabledAttribute) as? Bool { node["enabled"] = enabled }
  if let rect = frame(element) {
    node["frame"] = [Int(rect.minX), Int(rect.minY), Int(rect.width), Int(rect.height)]
  }
  return node
}

// MARK: - Apps and roots

func pid(of app: String) -> pid_t {
  if app.hasPrefix("pid:"), let number = Int32(app.dropFirst(4)) { return number }
  let running = NSWorkspace.shared.runningApplications
  if app.hasPrefix("name:") {
    let name = String(app.dropFirst(5))
    if let match = running.first(where: {
      $0.localizedName == name || $0.executableURL?.lastPathComponent == name
    }) {
      return match.processIdentifier
    }
    fail(.appNotRunning, "no running app with the name \(name)")
  }
  if let match = running.first(where: { $0.bundleIdentifier == app }) {
    return match.processIdentifier
  }
  fail(.appNotRunning, "no running app with the bundle identifier \(app)")
}

/// The roots of an app: its windows and menus, and the menu bar that
/// holds its status items. The second one is not a child of the first.
func roots(of pid: pid_t) -> [AXUIElement] {
  let app = AXUIElementCreateApplication(pid)
  var result = [app]
  if let extras = attribute(app, "AXExtrasMenuBar"), CFGetTypeID(extras) == AXUIElementGetTypeID() {
    // The type check above makes the cast safe.
    result.append(extras as! AXUIElement)
  }
  return result
}

// MARK: - Search

struct Selector {
  let attribute: String
  let expected: String
  /// `~=`: the attribute contains the text, not equals it.
  let contains: Bool

  init(_ raw: String) {
    guard let separator = raw.firstIndex(of: "=") else {
      fail(.usage, "a selector is <attribute>=<text>, got \(raw)")
    }
    var key = String(raw[..<separator])
    contains = key.hasSuffix("~")
    if contains { key.removeLast() }
    let names = [
      "id": kAXIdentifierAttribute, "title": kAXTitleAttribute, "desc": kAXDescriptionAttribute,
      "value": kAXValueAttribute, "role": kAXRoleAttribute, "subrole": kAXSubroleAttribute,
    ]
    guard let name = names[key] else {
      fail(.usage, "unknown selector attribute \(key)")
    }
    attribute = name
    expected = String(raw[raw.index(after: separator)...])
  }

  func matches(_ element: AXUIElement) -> Bool {
    guard let actual = text(element, attribute) else { return false }
    return contains ? actual.contains(expected) : actual == expected
  }
}

let maxDepth = 60
let maxNodes = 20_000

/// Depth-first walk. `visit` returns `false` to stop the whole walk.
@discardableResult
func walk(
  _ element: AXUIElement, depth: Int, count: inout Int, visit: (AXUIElement, Int) -> Bool
) -> Bool {
  count += 1
  if count > maxNodes || depth > maxDepth { return true }
  if !visit(element, depth) { return false }
  for child in children(element) where !walk(child, depth: depth + 1, count: &count, visit: visit) {
    return false
  }
  return true
}

func first(_ selector: Selector, under searchRoots: [AXUIElement]) -> AXUIElement? {
  var found: AXUIElement?
  var count = 0
  for root in searchRoots {
    walk(root, depth: 0, count: &count) { element, _ in
      if selector.matches(element) {
        found = element
        return false
      }
      return true
    }
    if found != nil { break }
  }
  return found
}

/// Resolves `<app> <selector> [--within <selector>]` to one control.
func locate(_ arguments: [String]) -> AXUIElement {
  guard arguments.count >= 2 else {
    fail(.usage, "expected <app> <selector> [--within <selector>]")
  }
  var searchRoots = roots(of: pid(of: arguments[0]))
  if let flag = arguments.firstIndex(of: "--within"), flag + 1 < arguments.count {
    guard let scope = first(Selector(arguments[flag + 1]), under: searchRoots) else {
      fail(.notFound, "no control matches the scope \(arguments[flag + 1])")
    }
    searchRoots = [scope]
  }
  guard let element = first(Selector(arguments[1]), under: searchRoots) else {
    fail(.notFound, "no control matches \(arguments[1])")
  }
  return element
}

// MARK: - Notifications of macOS

let notificationCenter = "name:NotificationCenter"

func notificationAlerts() -> [AXUIElement] {
  var alerts: [AXUIElement] = []
  var count = 0
  for root in roots(of: pid(of: notificationCenter)) {
    walk(root, depth: 0, count: &count) { element, _ in
      let subrole = text(element, kAXSubroleAttribute)
      if subrole == "AXNotificationCenterAlert" || subrole == "AXNotificationCenterBanner" {
        alerts.append(element)
      }
      return true
    }
  }
  return alerts
}

func childText(_ element: AXUIElement, identifier: String) -> String? {
  var result: String?
  var count = 0
  walk(element, depth: 0, count: &count) { child, _ in
    if text(child, kAXIdentifierAttribute) == identifier {
      result = text(child, kAXValueAttribute)
      return false
    }
    return true
  }
  return result
}

// MARK: - Commands

let arguments = Array(CommandLine.arguments.dropFirst())
guard let command = arguments.first else {
  fail(.usage, "expected a command; see the header of main.swift")
}
let rest = Array(arguments.dropFirst())

switch command {
case "permissions":
  printJSON([
    "accessibility": AXIsProcessTrusted(),
    "screenCapture": CGPreflightScreenCaptureAccess(),
    "postEvent": CGPreflightPostEventAccess(),
  ])

case "apps":
  printJSON(
    NSWorkspace.shared.runningApplications.map { app -> [String: Any] in
      [
        "pid": Int(app.processIdentifier),
        "bundle": app.bundleIdentifier ?? "",
        "name": app.localizedName ?? "",
      ]
    })

case "windows":
  let options: CGWindowListOption = [.optionOnScreenOnly, .excludeDesktopElements]
  let list = (CGWindowListCopyWindowInfo(options, kCGNullWindowID) as? [[String: Any]]) ?? []
  printJSON(
    list.map { window -> [String: Any] in
      let bounds = window[kCGWindowBounds as String] as? [String: Any] ?? [:]
      return [
        "pid": window[kCGWindowOwnerPID as String] as? Int ?? 0,
        "owner": window[kCGWindowOwnerName as String] as? String ?? "",
        "layer": window[kCGWindowLayer as String] as? Int ?? 0,
        "title": window[kCGWindowName as String] as? String ?? "",
        "frame": [
          bounds["X"] as? Int ?? 0, bounds["Y"] as? Int ?? 0,
          bounds["Width"] as? Int ?? 0, bounds["Height"] as? Int ?? 0,
        ],
      ]
    })

case "tree":
  guard let app = rest.first else { fail(.usage, "expected <app>") }
  var nodes: [[String: Any]] = []
  var count = 0
  for root in roots(of: pid(of: app)) {
    walk(root, depth: 0, count: &count) { element, depth in
      nodes.append(describe(element, depth: depth))
      return true
    }
  }
  printJSON(nodes)

case "find":
  let element = locate(rest)
  var node = describe(element, depth: 0)
  node["actions"] = actions(element)
  printJSON(node)

case "press":
  let element = locate(rest)
  let node = describe(element, depth: 0)
  let status = AXUIElementPerformAction(element, kAXPressAction as CFString)
  if status != .success {
    fail(.actionFailed, "the press failed with the accessibility error \(status.rawValue)")
  }
  printJSON(node)

case "click":
  let element = locate(rest)
  guard let rect = frame(element), rect.width > 0, rect.height > 0 else {
    fail(.actionFailed, "the control has no area on the screen")
  }
  let point = CGPoint(x: rect.midX, y: rect.midY)
  for type in [CGEventType.leftMouseDown, .leftMouseUp] {
    guard
      let event = CGEvent(
        mouseEventSource: nil, mouseType: type, mouseCursorPosition: point, mouseButton: .left)
    else {
      fail(.actionFailed, "could not make the pointer event")
    }
    event.post(tap: .cghidEventTap)
    usleep(60_000)
  }
  var node = describe(element, depth: 0)
  node["clicked"] = [Int(point.x), Int(point.y)]
  printJSON(node)

case "notifications":
  printJSON(
    notificationAlerts().map { alert -> [String: Any] in
      [
        "title": childText(alert, identifier: "title") ?? "",
        "body": childText(alert, identifier: "body") ?? "",
        "desc": text(alert, kAXDescriptionAttribute) ?? "",
      ]
    })

case "close-notifications":
  var closed = 0
  for alert in notificationAlerts() {
    // The close action has a name made of several lines; its first
    // line is "Name:Close".
    guard let close = actions(alert).first(where: { $0.contains("Close") }) else { continue }
    if AXUIElementPerformAction(alert, close as CFString) == .success { closed += 1 }
  }
  if closed > 0 {
    // A closed notification fades out; a screenshot waits for it.
    for _ in 0..<20 where !notificationAlerts().isEmpty { usleep(100_000) }
    usleep(400_000)
  }
  printJSON(["closed": closed])

case "allow-screen-capture":
  // macOS asks once in a while whether a tool may capture the screen
  // with no picker. The dialog belongs to UserNotificationCenter.
  let running = NSWorkspace.shared.runningApplications
  guard
    let center = running.first(where: { $0.localizedName == "UserNotificationCenter" }),
    let button = first(
      Selector("id=action-button-1"), under: roots(of: center.processIdentifier))
  else {
    printJSON(["pressed": false])
    break
  }
  let status = AXUIElementPerformAction(button, kAXPressAction as CFString)
  printJSON(["pressed": status == .success])

default:
  fail(.usage, "unknown command \(command)")
}
