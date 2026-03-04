# System Overview

## Components

- `apps/macos`: SwiftUI app for UX, auth orchestration, status, controls.
- `daemon`: Rust background engine for watch/schedule/execute/durability.
- `providers`: Rust cloud adapters behind capability-driven trait.
- `shared`: app/daemon contracts and versioned schema models.

## Boundary rules

- App process should not run heavy sync compute.
- FSEvents callback path must stay lightweight.
- Provider-specific behavior must stay out of core engine scheduling logic.

## Initial module sketch

```text
vapor/
  apps/macos      # SwiftUI shell + settings + menubar + auth UI
  daemon          # Rust engine + queue/state + throttle + reconcile
  providers       # provider_gdrive, provider_s3 (planned)
  shared          # XPC models, error taxonomy, policy models
  docs            # planning, architecture, operations, performance
```

## Planned implementation sequence

1. app + daemon lifecycle and status wiring
2. low-impact local ingest and scheduling
3. provider integration and bidirectional safety
4. durability and upgrade hardening
