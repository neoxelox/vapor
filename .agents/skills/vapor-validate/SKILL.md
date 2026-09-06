---
name: vapor-validate
description: Runs Vapor's validation ladder in the required order (format, lint, test, then e2e for runtime-affecting changes), explains which tier a change needs, and reads a red run. Use before committing any change under core/*, apps/*, or scripts/*, and whenever a script run fails and the failure needs classifying.
license: GPL-3.0-only
---

# Validate a change

## The order

Always the wrapper scripts, never the raw tools; they set `VAPOR_DIR`
(`./.vapor`), `VAPOR_ENV=dev`, and the other environment the suite
relies on.

```
./scripts/format.sh   # applies rustfmt and swift-format
./scripts/lint.sh     # clippy -D warnings, swift-format lint, format check, version sync
./scripts/test.sh     # Tier 1: Rust + Swift + version checks
./scripts/e2e.sh      # Tier E2E: real vapor + vapord, one sandbox per scenario (see below)
```

`format` first because `lint` fails on unformatted code. Run all of them
again after the last edit; a green run from before an edit proves
nothing.

## Which tier a change needs

| Change | Tiers |
|---|---|
| Docs only, `assets/locales` only | none required, but `lint` is cheap and catches broken locale JSON |
| Tests only | `format`, `lint`, `test` |
| Anything under `core/*` or `scripts/*` that a user could observe through the daemon or CLI, including startup, shutdown, IPC, schema, config and build changes to the shipping binaries | `format`, `lint`, `test`, `e2e` |
| `apps/macos` logic | `format`, `lint`, `test`; UI rendering is never tested, hand the owner a manual checklist |
| Performance-sensitive engine changes | the above plus `./scripts/perf.sh` (Tier 2, release gate; not a PR gate: one release-profile soak cell with the SLO checks) |
| Changes to the executor, reconcile, deletion, or conflict paths | the above plus a soak (`vapor-soak` skill); quote its report |

## Tier 1 rules

- Budget: under 2 minutes locally, under 5 minutes on CI; the `test`
  workflow sets `VAPOR_TEST_MAX_SECONDS=300` and `scripts/test.sh` fails a
  slower green run. A change that blows the budget moves its slow tests
  to Tier 2, it does not raise the budget.
- Deterministic: no `thread::sleep` for timing assertions (inject a
  clock), no retry decorators, every integration test on its own
  `TempDir`, no network, never `~/.vapor`.
- A flaky test blocks merging until fixed or removed; removing one needs
  an issue naming the invariant it covered.
- Every `vapor … --json` command keeps an explicit shape test.

## Tier E2E rules

- `./scripts/e2e.sh` is host-safe: everything lives under
  `.vapor/e2e/`, no service install, no app launch, no network.
- Never run `--full` locally: it installs a real LaunchAgent and is for
  disposable CI runners.
- A change that adds e2e-observable behaviour adds a scenario under
  `tools/e2e/src/scenarios/` in the same change set, run once against
  the base commit (must fail) and once after (must pass). A green run
  of old scenarios proves non-regression, not the new feature.
- Every scenario ends with the tree oracle (local root equals cloud
  root) and log hygiene (no ERROR, only declared warnings, no failed
  intents); a change that makes either fail is a finding, not noise.
- `--only Sxx` runs one scenario; `--json` writes the report; a
  `KNOWN-GAP` line is an expected failure, a `FIXED?` line means a
  marker must be removed and makes the run red.
- The harness sets `VAPOR_THROTTLE_INPUTS=static` so a developer at
  the keyboard does not hold the daemon at `Throttled`.
- Details and the manual sandbox: the `vapor-e2e` skill.

## Reading a red run

- `format.sh check` diff: run `./scripts/format.sh` and re-run lint.
- Clippy: fix the lint, do not `allow` it; `#[allow]` needs a comment
  stating why the lint is wrong here.
- A failing test: read the assertion message and the test body before
  the code; the tests are the specification. If the test is wrong, say
  so in the commit.
- `version.sh check-sync`: `VERSION`, `[workspace.package] version` and
  every member's lockfile entry disagree; run `./scripts/version.sh sync`
  only as part of a release, otherwise fix the stray edit.
- e2e `FAIL Sxx`: the harness prints status, diagnostics, queue rows,
  and log tails for every daemon in the scenario and keeps the sandbox
  (`.vapor/e2e/run-<id>/Sxx/`); read that output, then use the
  `vapor-debug` skill on the preserved directory. `FIXED?` means a
  known-gap marker is stale: remove it in the same change.

## Report

Quote the last line of each script you ran. If a step was skipped, say
which and why. A red step is never summarised as "mostly green".
