# macOS operations

macOS-specific operations documents. Common (platform-agnostic)
operations live one level up in `docs/operations/`. Read those first for
the shared release and logging policy; this directory is where the
macOS-specific signing, notarization, and `launchd` policy are recorded.

## How to use this group

- **Cutting a macOS release?** Follow `docs/operations/release-process.md`
  (common), then apply `distribution-trust-chain.md` for the macOS-
  specific signing / hardened runtime / notarization requirements.
- **Changing LaunchAgent behavior or touching the crash-loop contract?**
  `launchagent-policy.md` is the authoritative policy; any code change
  must stay aligned with it. Crash-loop policy itself lives in
  `core/lifecycle` — this doc describes how macOS is expected to
  interact with that policy via `launchd`.
- **Debugging an unclean daemon exit?** `launchagent-policy.md` §
  Validation lists the scenarios and expected behavior
  (SIGKILL, crash-loop pause, clean shutdown, plist audit).

## Documents

- `launchagent-policy.md` — `launchd` plist policy for the `vapord`
  per-user LaunchAgent (`sh.arn.vapor.daemon`), interaction contract
  with the Rust-backed `CrashLoopGuard`, validation scenarios for M1-6.
- `distribution-trust-chain.md` — required controls for macOS release
  artifacts: code signing (Developer ID), hardened runtime,
  notarization, entitlement review; `release-macos` GitHub Environment
  secret policy.
