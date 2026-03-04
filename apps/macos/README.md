# apps/macos

SwiftUI macOS application surface for `vapor`.

Responsibilities:

- onboarding and settings UX
- menubar status and controls
- provider auth orchestration UI
- Keychain integration
- daemon lifecycle and auto-launch controls

Current implementation notes:

- `VaporCore` includes `DaemonLifecycleManager` for default auto-launch policy, toggle semantics, crash-loop relaunch backoff, and optional login-item registration.
- `VaporCore` includes a concrete `LaunchAgentController` that writes `~/Library/LaunchAgents/<label>.plist` and manages lifecycle with `launchctl`.
- `AppShellViewModel` uses lifecycle defaults backed by `LaunchAgentController` and can optionally enable `SMAppService` login-item integration when `VAPOR_LOGIN_ITEM_IDENTIFIER` is set.

Implementation phases map to `docs/plans/vapor-macos-task-list.md`.

Local commands:

- Build: `swift build --package-path apps/macos`
- Test: `swift test --package-path apps/macos`
