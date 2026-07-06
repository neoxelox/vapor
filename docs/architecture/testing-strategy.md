# Testing strategy

Authoritative reference for how Vapor is tested. The policy summary lives
in `AGENTS.md §9`; this document is the full taxonomy, per-module
expectations, and discipline rules.

## Why testing matters here

Vapor is coded autonomously. The coding agent's feedback loop is
whatever `./scripts/test.sh` tells it. If tests are slow, flaky, or
miss the invariants that matter, the agent loses signal and regresses
silently. So:

- Tests must be **fast** enough that the agent runs them every change.
- Tests must be **deterministic** so a failure means a bug, never "try
  again".
- Tests must cover **real behavior**, not trivial restatements of
  code. A thousand trivial tests are worse than a hundred well-chosen
  ones.

The bar is quality over quantity — but in this project quality also
means breadth, because the engine has complex concurrent behavior
that silently-wrong code can introduce.

## Taxonomy

### Unit tests (Tier 1; every PR)

Module-colocated tests for pure logic. One or two tests **per
invariant**, not per function. Use `tempfile::TempDir` for filesystem
work; never touch the user's `VAPOR_DIR`. Use test-injectable clocks;
never `thread::sleep` for timing.

Target modules with heavy coverage expected in `core/daemon`:

- `debounce.rs` — quiet-window behavior per extension class; coalescing
  under burst; tick cadence.
- `scheduler.rs` — superseding semantics; dirty-while-running;
  completion disposition.
- `throttle.rs` — input-to-state mapping; per-state caps; hysteresis
  and min-dwell once C2-4 lands.
- `workgate.rs` — permit issuance and release; permit-id allocation
  (including wrap-around once C2-2 lands); reconfigure semantics.
- `retry.rs` — backoff math; jitter bounds; rate-limit floor honoring.
- `storm.rs` — threshold detection; defer window; compaction.
- `state_db.rs` — schema versioning; lease recovery; retry-slowdown
  persistence; corruption guards; bounded durable fields.
- `reconcile.rs` — idle-bias; slice yielding; boundary clearing.
- `executor.rs` — stage transitions; permit handoff; admission capacity
  math.
- `path_filter.rs` — gitignore/vaporignore precedence; nested discovery;
  heavy-dir skip.
- `event_intents.rs` — compaction; deferred markers; bounded caps.
- `fs_events.rs` — callback discipline; symlink-escape rejection; path
  normalization invariants.

In `core/shared`:

- `runtime_paths.rs` — `VAPOR_DIR` resolution order; permission
  application on Unix; Windows fallback once C1-1/C1-2 land.
- `logging.rs` — log level parsing; sensitive-key redaction; inline-
  secret redaction; graceful fallback on log-file failure.

Planned crates inherit the same expectation: `core/platform`,
`core/lifecycle`, `core/cli` each ship tests with their logic.

### Integration tests (Tier 1; every PR)

Compose multiple modules and drive them through the tick-based test
harness that already exists in `DaemonRuntime` (see `runtime.rs` test
module — it is the template).

Priority scenarios:

- Local write propagates through debounce → scheduler → durable queue
  → staged executor → provider upload against the real filesystem
  provider (the `BidirectionalFixture` manual-feed harness in
  `runtime.rs` is the template).
- Remote create/modify/delete propagates through poll → durable intent
  → download/apply, including keep-both conflict copies and the
  deletion-preservation guard.
- Restart recovery with pending intents and in-flight leases; no
  duplication, no loss.
- Throttle transitions during real work (`Suspended` halts new
  admission; running work completes cleanly or yields at the next
  slice checkpoint; chunked transfers hold at their checkpoint).
- Storm-scale event bursts stay bounded; compaction triggers and the
  reconcile marker takes over.
- Reconcile is interruptible at slice boundaries and on throttle
  changes.
- Retry slowdown is restored from durable state after restart.
- Self-write-cache suppresses echo uploads of our own remote writes
  (and echo downloads of our own uploads).
- Sync-mode gates: pull-only reverts local divergence instead of
  uploading; push-only restores remote divergence instead of applying.
- Multi-profile runtime preserves isolation (own DB per profile,
  shared workgate caps, blast-radius containment on a panicking
  profile).
- Safeguards: mass-deletion storm pauses + alerts + `resume` re-arms;
  code-churn heuristic flips throttle to user-active; flush boost
  re-polls the remote feed.
- Config reload mid-work does not lose in-flight intents.

Cloud-provider logic (Google Drive) is tested entirely offline through
the injectable `HttpTransport` seam: `ScriptedHttpTransport` scripts
exact HTTP responses (including rate limits, 401s, resumable-upload
`Content-Range` handshakes, and changes-feed pages) and records
requests for assertion. No Tier 1 test may contact the network.

Integration tests must stay fast. Each test uses its own `TempDir`,
cleans up automatically, and avoids real network. Fixtures with
thousands of files belong to Tier 2 perf runs, not Tier 1.

### Property tests (Tier 1; every PR, bounded case count)

Via `proptest`. High-value invariants where random inputs catch the
edge cases humans miss. Each property runs **64–256 cases on CI** —
enough to catch bugs, fast enough to not bog down the suite.

High-priority properties to add:

- **Path normalization** — any generated path (including `../`, `./`,
  multi-byte chars, traversal sequences) produces either an output
  within the watch root or a rejection; never silently escapes.
- **Scheduler superseding** — any sequence of upserts collapses to one
  pending intent per path, and the intent's kind matches the latest
  upsert.
- **Throttle monotonicity** — monotonic input pressure produces
  monotonic state transitions; the controller never moves from
  `Throttled` to `IdleDrain` while CPU pressure rises.
- **Retry backoff monotonicity** — the computed delay is
  non-decreasing in attempt count and always `<= RETRY_MAX_DELAY_MILLIS`.
- **Conflict suffix determinism** — identical `(path, device_id,
  timestamp_ms)` inputs produce an identical conflict suffix across
  runs.
- **Durable queue FIFO** — enqueue-then-lease within the same priority
  returns FIFO order.
- **Ignore-rule precedence** — given overlapping rules, the decision
  is deterministic per the documented precedence.
- **IPC handshake** — any `(app_version, daemon_version)` pair in
  `|N - M| <= 1` succeeds; `|N - M| >= 2` fails with
  `IncompatibleVersion`.

### Platform trait contract tests (Tier 1; macOS CI + each shipping OS)

When `core/platform` lands (wave 4), every trait gets a
**parameterized contract suite** run against both the in-memory fake
and the real native implementation on each shipping OS. This catches
fake-vs-native drift — the single most likely source of "works in
tests, breaks in prod".

Structure:

```rust
fn contract_tests<F: FsWatcher>(factory: impl Fn() -> F) {
    // invariants
}

#[test] fn fake_fs_watcher_contract() { contract_tests(FakeFsWatcher::new); }
#[test] fn macos_fs_watcher_contract() { contract_tests(MacosFsWatcher::new); }
```

Trait-specific invariants:

- `FsWatcher` — create → modify → remove for a file; rename pairs;
  symlink escape dropped; callback discipline preserved.
- `ServiceInstaller` — install → start → status=`Running` → stop →
  uninstall round-trip; status transitions are observable.
- `SecretStore` — get-after-set returns the value; delete removes; list
  returns the keys we wrote.
- `PlatformMetricsSampler` — returned values are within their declared
  bounds; monotonic inputs produce monotonic outputs where the trait
  documents that.
- `IdleNotifier` — `user_active` flips correlates with synthetic
  activity.
- `FilesystemCapabilities` — `supports_xattr` is consistent with the
  underlying FS; xattr round-trip works when claimed to be supported;
  side-file fallback fires when not.
- `ProcessSupervisor` — shutdown handler fires on SIGTERM (or the
  platform equivalent).

### Concurrency tests

Most of Vapor's concurrency is tick-based, not free-running. The tick
abstraction (`DaemonRuntime::tick_with_inputs`) makes concurrency
testable deterministically **without** `loom` for 90% of the interesting
cases. Use it first.

Reserve `loom`-backed tests for the remaining corners where atomics
and memory ordering actually matter:

- `ThrottleWorkgate` permit allocation under contention.
- `BoundedFsEventRecorder` drop-count semantics under callback
  backpressure.

Loom tests are slow. Keep them small, keep them few, and run them in
**Tier 2** (release gate), not on every PR.

### Performance regression tests (Tier 2; release gate)

The SLO suite in `docs/performance/acceptance-budgets-and-benchmark-harness.md`
runs via `scripts/perf.sh`. Release gate only; not a PR gate.

Tier 1 keeps a small number of cheap **guard-rail** timing tests
— the kind already present in `fs_events.rs` (5 000-event callback
burst < 2 s) and `runtime.rs` (150-event composed tick < 3 s). These
are not SLO tests; they exist to catch "someone accidentally made the
callback 100× slower" before it reaches the release pipeline.

### Snapshot tests for CLI (Tier 1)

Once `core/cli` lands (wave 6), every `vapor … --json` command gets a
snapshot test via `insta`. Snapshots live next to the test file. Wire-
format drift fails the PR; intentional changes are reviewed with
`cargo insta review`.

Commands to snapshot:

- `vapor status --json`
- `vapor version --json`
- `vapor doctor --json`
- `vapor service status --json`
- `vapor timeline --json` (fixed input)
- `vapor config get <key> --json`
- Every other `--json` command that exists at the time.

### End-to-end tests (Tier E2E; runtime-affecting changes)

`./scripts/e2e.sh` runs the real `vapor` + `vapord` binaries black-box
through the CLI against a disposable sandbox under the repo-local
`.vapor/e2e/` directory — real process boundaries, real FSEvents, real
durable DB, real IPC socket, real signals. It is how an autonomous
agent verifies "the product actually works", not just "the modules are
correct". Fully sandboxed: never `~/.vapor`, never a host service
install, never the macOS app, no network.

Required after Tier 1 passes for any feature or fix that changes
behavior a user would observe through the daemon or CLI; when a change
adds e2e-observable behavior, the harness gains a scenario for it in
the same change set. Full process, scenario catalog, and extension
discipline: `docs/development/e2e-verification.md`; policy summary:
`AGENTS.md §9.8`.

### Fuzz tests (Tier 2; release gate)

Via `cargo-fuzz`. Target the parsers and format handlers that accept
external input:

- Ignore-rule parser (`path_filter.rs`).
- IPC frame parser (once wave 6 lands).
- JSON config loader (`VaporConfiguration` decoding — Swift side — and
  the Rust-side equivalent when lifecycle moves over).
- Path normalization (`fs_events.rs`).

Short corpora live in-tree. Long fuzz runs ride the Tier-2 release gate
alongside the perf suite. Fuzzing is **not** a PR gate.

## What we deliberately do NOT test

This list is as important as the "do test" list. A thousand trivial
tests is worse than a hundred well-chosen ones.

- **Trivial getters / setters** that return an inner field.
- **`Default` impls** whose values are constants mirrored 1:1 from the
  struct definition. (The `Default` *behavior* may be worth testing if
  it does something — e.g., throttle default state selection — but
  not the rote mapping.)
- **`Debug` / `Display` impls** unless the output is a wire format
  (e.g., structured log lines qualify; ad-hoc `Debug` does not).
- **`serde` derive round-trips** of trivial structs. The derive is
  correct by construction; testing it tests serde, not us.
- **Generated code** from `build.rs`.
- **UI rendering** on the macOS app — SwiftUI views, menubar layout,
  Dock transitions, keyboard focus, animation timing. UI correctness
  is verified by the project owner manually. This applies to every
  future GUI surface (Windows, Linux).
- **Interactive TTY behavior** on the CLI — cursor positioning, color
  codes, terminal resize, ncurses interactions.
- **Third-party crate internals** (e.g., do not test that `rusqlite`
  actually persists rows; test that *our* schema + query code does
  what we expect).
- **Code that just restates a policy from `constants.rs`** (e.g., a
  test asserting that `LIGHT_UPLOAD_CONCURRENCY == 2` is not a test,
  it is a duplicate).

A useful question before writing a test: **"if this test fails, what
bug did I catch?"** If the honest answer is "I typo'd a default
value", the test is not worth writing.

## Discipline rules

1. **Fast.** Tier 1 must finish `./scripts/test.sh` in under **2
   minutes** on a contemporary dev machine (Apple M-series or
   equivalent) and under **5 minutes** on CI runners. If a change
   pushes Tier 1 past the budget, split the slow tests out to Tier 2
   or make them faster.
2. **Deterministic.** No `thread::sleep` for timing-dependent
   assertions. Use the test-injectable clock abstractions (`Instant`
   wrapper in C2-3 and the existing `timestamp_ms`-style helpers in
   `state_db.rs` / `runtime.rs` tests). No "run 10×, pass if 9 pass"
   retry decorators.
3. **Independent.** Tests run in any order and in parallel (`cargo
   test` default behavior). No shared mutable state. Each integration
   test uses its own `TempDir`.
4. **Scoped.** A test hits one invariant. If the name has "and" in it,
   it is probably two tests. Keep each test under ~50 lines of body
   where possible.
5. **Clear failure.** Assertion messages identify what was expected.
   Prefer `assert_eq!(actual, expected, "context: ...")` over naked
   `assert!(x)` without a message.
6. **No network.** Never contact the real Internet from tests. Any
   HTTPS target goes through a local fixture or a mock.
7. **No real `VAPOR_DIR`.** Never touch `~/.vapor`. Tests that need a
   runtime dir use `tempfile::TempDir` and set `VAPOR_DIR`
   accordingly.
8. **Named by behavior, not by function.**
   `debounce_coalesces_events_within_quiet_window` >
   `test_debounce_1`.

## Flaky-test policy

- A test that fails intermittently on the same input is flaky.
- Flaky tests block merges until fixed or removed. Retry decorators
  are banned.
- If a test is flagged flaky twice in two weeks, it is either fixed or
  removed.
- When removing a flaky test, open an issue describing the invariant
  that is no longer covered and, if it matters, add a replacement.

## CI tier execution

- **Tier 1** — `lint.yml`, `test.yml`, `workflow_call`. Runs unit +
  integration + platform-trait contract + property + snapshot tests.
  Runs on every PR. Required check on `main`. Target budget: under
  **5 minutes** per OS in the matrix.
- **Tier 2** — `perf.yml`. Runs performance SLO tests
  (`scripts/perf.sh`), long-running property cases (higher case
  counts), fuzz corpora, and any `loom`-backed tests. Release gate
  only: `perf.yml` has no standalone triggers and is invoked solely by
  `release.yml`.
- **Tier E2E** — `scripts/e2e.sh`, at the end of `test.yml`'s
  macOS job (every PR; part of the required `test` check on `main`).
  Also part of the local contributor validation loop for
  runtime-affecting changes (`AGENTS.md §9.8`). Runs the shipped
  binaries sandboxed under `.vapor/e2e/`; budget ~60 s after the
  build. macOS only — it exercises the native FSEvents watcher on the
  shipping surface. CI invokes it with `--full`, which appends the
  black-box `vapor service` round-trip (install → start → status →
  crash-loop supervision → acknowledge → stop → uninstall) against
  real `launchd`. That phase installs a real LaunchAgent —
  host-mutating by design — so it is opt-in: contributors run the
  default (host-safe) suite; only disposable CI runners pass `--full`.

## Per-surface scope

### `core/*` (Rust runtime, CLI, platform layer)

**Heavy testing.** Every non-trivial module has unit tests. Every
composed behavior has an integration test. Every trait has contract
tests. Every key invariant has a property test where random inputs
add value. No UI, so nothing is excluded.

### `apps/macos` (Swift)

**Logic tests only.** Configuration parsing and normalization,
lifecycle coordinator state transitions, view-model state mapping,
localization fallback, logger redaction — all tested. The existing
suite under `apps/macos/Tests/` is the template and the correct
shape.

**No UI tests.** No SwiftUI view rendering tests, no menubar layout
tests, no Dock/window-state snapshot tests, no keyboard-focus tests.
UI correctness is verified by the project owner manually.

Lifecycle policy lives in `core/lifecycle` and is tested there; the
Swift-side `DaemonLifecycleManagerTests` covers only what Swift still
owns (delegation order, outcome mapping, login-item coupling), never a
second copy of the Rust policy.

### `core/cli` (Rust, `vapor` binary)

**Logic + snapshot + integration tests.** Every command with `--json`
output has a snapshot. `vapor service install` / `run` / `status`
round-trips are automated in CI. IPC client correctness runs against
a fake daemon.

**No interactive TTY tests.** No color-code assertions, no
cursor-position assertions, no terminal-resize simulations.

### Future GUI apps (Windows, Linux)

Same rule: logic yes, UI no. When those surfaces land, each gets a
test section in its plan (`docs/plans/windows.md`,
`docs/plans/linux.md`) and inherits the same carve-out.

## Adding a test — checklist

Before writing a test, ask:

- [ ] What bug does a failure catch? (If the answer is trivial, skip.)
- [ ] Is the logic non-trivial enough to warrant a test?
- [ ] Is the test deterministic? (No sleeps, no network, no real
      `~/.vapor`.)
- [ ] Does the name describe the behavior?
- [ ] Does it run in under a second (Tier 1)?
- [ ] Is it a UI rendering or TTY interaction test? If yes — stop;
      we do not test those.

If you answered yes to the first five and no to the last, write the
test.
