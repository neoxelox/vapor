# apps/macos

SwiftUI macOS application surface for `vapor`.

Responsibilities:

- onboarding and settings UX
- menubar status and controls
- provider auth orchestration UI
- Keychain integration
- daemon lifecycle and auto-launch controls

Implementation phases map to `docs/plans/vapor-macos-task-list.md`.

Local commands:

- Build: `swift build --package-path apps/macos`
- Test: `swift test --package-path apps/macos`
