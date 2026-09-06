# core/platform

Trait catalog and per-OS native implementations for the Vapor runtime's
platform-sensitive seams. The engine code in `core/daemon`,
`core/providers`, `core/lifecycle`, and `core/cli` consumes these traits
instead of reaching for OS APIs directly.

Authoritative reference: `docs/architecture/platform-abstractions.md`.
Governing plan: `docs/plans/core.md` §3.
Tasks: `docs/tasks/core.md` Phase C3.

## Module layout

- `fs_watch` — recursive filesystem watcher (FSEvents on macOS;
  ReadDirectoryChangesW on Windows; inotify / fanotify on Linux).
- `service` — install / start / stop the daemon as a platform-native
  background service (LaunchAgent on macOS; Task Scheduler / SCM on
  Windows; systemd on Linux).
- `secrets` — per-provider OAuth tokens and other credentials. Keychain
  Services on macOS (shipped); Credential Manager on Windows and
  libsecret / age-encrypted file on Linux (stubs).
- `metrics` — `PlatformMetricsSampler` returning a fresh
  `ThrottleInputsSnapshot` every tick.
- `idle` — user-idle duration source.
- `fs_caps` — filesystem capabilities (xattr / ADS support, case
  sensitivity).
- `process` — graceful-shutdown signal handler registration.

Each module exports the trait, an `InMemory*` test fake that compiles on
every OS, and a `Native*` per-OS implementation selected at compile time
via `#[cfg(target_os = "...")]`. Wave 4 (`core/tasks/core.md` C3) ships
the trait surface plus a macOS skeleton; Linux and Windows native impls
are intentionally `Unsupported` until Waves 12 / 13 land.

## Why this crate exists

Three reasons, in order:

1. **One place for OS specifics.** Engine code stays portable; the
   `#[cfg]` machinery lives in this crate only.
2. **Test injectability.** Every trait has an in-memory fake so unit
   tests run on any host without touching OS APIs.
3. **Native-optimal implementations.** When a portable wrapper crate
   leaves performance on the table, the per-OS module replaces it with a
   direct native call behind the same trait — no engine changes needed.

## Adding a new trait

1. Create `src/<trait>.rs` with the trait definition + in-memory fake.
2. Optionally split the per-OS native impl into `src/<trait>/<os>.rs`
   when the implementation is non-trivial (see `service/macos.rs`).
3. Re-export the public surface from `src/lib.rs`.
4. Update `docs/architecture/platform-abstractions.md` with the new
   trait, its contract, and the per-OS native APIs.
5. Add unit tests for the in-memory fake. The macOS native impl gets a
   parity test once the C3 contract-test harness lands.

## Adding a new OS

Open `docs/architecture/platform-abstractions.md` §"Adding a new
platform" and follow the checklist — every trait module needs an
`<os>.rs` sibling, the parity tests must pass, and the Cargo CI matrix
needs the new target triple.
