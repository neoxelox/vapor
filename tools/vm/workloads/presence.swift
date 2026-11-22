// presence: posts a small pointer move every few seconds, so macOS counts
// a user at the device. Usage: presence <seconds> [interval]
import CoreGraphics
import Foundation

let total = Double(CommandLine.arguments.dropFirst().first ?? "60") ?? 60
let interval = Double(CommandLine.arguments.dropFirst(2).first ?? "2") ?? 2
let end = Date().addingTimeInterval(total)
var x = 700.0
while Date() < end {
  x = x == 700.0 ? 704.0 : 700.0
  if let event = CGEvent(
    mouseEventSource: nil, mouseType: .mouseMoved,
    mouseCursorPosition: CGPoint(x: x, y: 500), mouseButton: .left)
  {
    event.post(tap: .cghidEventTap)
  }
  Thread.sleep(forTimeInterval: interval)
}
