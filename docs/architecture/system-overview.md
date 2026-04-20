# System Overview

## Components

- `apps/macos`: SwiftUI app for UX, auth orchestration, status, controls.
- `core/daemon`: Rust background engine for watch/schedule/execute/durability.
- `core/providers`: Rust cloud adapters behind capability-driven trait.
- `core/shared`: app/daemon contracts and versioned schema models.

## Boundary rules

- App process should not run heavy sync compute.
- FSEvents callback path must stay lightweight.
- Provider-specific behavior must stay out of core engine scheduling logic.

## Initial module sketch

```text
vapor/
  apps/macos      # SwiftUI shell + settings + menubar + auth UI
  core/daemon     # Rust engine + queue/state + throttle + reconcile
  core/providers  # provider_filesystem (pre-GA reference and integration-test provider), provider_gdrive (deferred to a later milestone), additional providers (future)
  core/shared     # XPC models, error taxonomy, policy models
  docs            # planning, architecture, operations, performance
```

## Planned implementation sequence

1. app + daemon lifecycle and status wiring
2. low-impact local ingest and scheduling
3. filesystem reference provider and bidirectional runtime shell (provider-neutral mechanics validated against a loopback local provider)
4. conflict/tombstone safety, profile model, and XPC/diagnostics
5. external cloud provider integration (Google Drive, deferred) and durability/upgrade hardening
