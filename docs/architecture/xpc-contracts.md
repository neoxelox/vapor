# XPC Contracts (Placeholder)

This document defines the contract surface between `apps/macos` and `core/daemon`.

## Contract versioning rules

- Every request/response payload carries explicit schema version.
- Additive changes are preferred; removals require migration path.
- App and daemon compatibility must be validated against supported version matrix.

## Planned contract groups

- Status
  - running state, throttle state, reason, queue depth, last sync markers
- Provider/auth
  - auth state, token refresh health, provider capability flags
- Lifecycle
  - auto-launch enabled state, last launch result, crash-loop pause state
- Controls
  - pause/resume, flush-now, excludes update, auto-launch toggle

## Next implementation step

Define concrete Swift/Rust payload types in `core/shared` and add contract tests for version compatibility.
