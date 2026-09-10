# Contributing to Vapor

Thanks for looking at Vapor. This page is the practical on-ramp.

**[`AGENTS.md`](AGENTS.md) is the authority.** It holds the full
operating rules — safety invariants, performance constraints, test
matrix, naming, release policy — and it binds human and AI contributors
alike. This file summarises the parts you need on day one and points at
the rest. Where the two disagree, `AGENTS.md` wins.

## Before you start

Vapor is **pre-GA and under heavy active development**. Backward
compatibility is not guaranteed for config, state, schema formats, or
internal contracts (`AGENTS.md` §1.1). Prefer a clean implementation
over a migration shim unless the maintainer asks otherwise.

For anything beyond a small fix, **open an issue first**. A lot of the
roadmap is already sequenced in
[`docs/tasks/README.md`](docs/tasks/README.md), which is the
"what should be done next" orchestrator across surfaces — check it
before building something that is already planned or deliberately
deferred.

By contributing you agree your work is licensed under
[GPL-3.0](LICENSE).

## Layout in one minute

- `core/*` — the portable Rust runtime. **All business logic lives
  here.** `daemon`, `providers`, `ipc`, `shared`, `platform`,
  `lifecycle`, `cli`.
- `apps/macos` — SwiftUI app. A UI and OS-integration shim over
  `core/*`; no business logic, no lifecycle policy.
- `core/platform` — traits plus per-OS native implementations. Engine
  code consumes traits, never OS APIs directly.
- `scripts/` — the supported entrypoint for every workflow.
- `docs/` — start at [`docs/README.md`](docs/README.md); every group has
  its own `README.md` entrypoint.

macOS is the only shipping surface today. Linux and Windows must keep
**compiling** — a platform trait may be `unimplemented!()` there, but
`cargo build --workspace` has to succeed on every OS in CI.

## Setup

You need a recent stable Rust toolchain (pinned by
`rust-toolchain.toml`) and, for the macOS app, Xcode with Swift 6.
Current verified baseline:
[`docs/development/toolchain-baseline.md`](docs/development/toolchain-baseline.md).

```sh
git clone https://github.com/neoxelox/vapor.git
cd vapor
./scripts/build.sh          # Rust workspace + Swift app
./scripts/hooks.sh install  # optional: pre-commit runs lint -> test -> build
```

Repo scripts default `VAPOR_DIR` to `./.vapor` and `VAPOR_ENV` to `dev`,
so local runs never touch your real `~/.vapor`. `./scripts/clean.sh`
removes it. See [`.env.example`](.env.example) for every supported
`VAPOR_*` variable.

## Validating a change

Run the wrapper scripts, not raw `cargo`/`swift` commands — they set
required project environment (`AGENTS.md` §8.4). In this order:

```sh
./scripts/format.sh
./scripts/lint.sh
./scripts/test.sh
```

If your change alters behaviour a user would observe through the daemon
or CLI, also run the end-to-end suite:

```sh
./scripts/e2e.sh
```

This drives the real `vapor` and `vapord` binaries black-box, one
disposable sandbox per scenario under `.vapor/e2e/`, and ends every
scenario by comparing the two trees. It never touches `~/.vapor`,
installs no host services, and uses no network. `--only Sxx` runs one
scenario. Run the plain form locally; the macOS CI job runs `--full`,
which additionally installs a real LaunchAgent and is meant for
disposable runners. Full process:
[`docs/development/e2e-verification.md`](docs/development/e2e-verification.md).

`./scripts/test.sh` must stay under **2 minutes locally, 5 on CI**. If
your change pushes it past that, move the slow test to Tier 2
(`scripts/perf.sh`).

## Tests

Testing is not optional — Vapor is largely built autonomously, and the
test suite *is* the feedback loop. Full taxonomy:
[`docs/architecture/testing-strategy.md`](docs/architecture/testing-strategy.md).

Tests must be **deterministic** (no `thread::sleep` for timing
assertions — use the injectable clock), **independent** (own
`TempDir`, parallel-safe), and must never use the network or the user's
runtime directory.

Please **do not** add tests for trivial getters, `Default` impls that
mirror constants, `serde` round-trips of trivial structs, third-party
internals, **UI rendering on any app surface**, or interactive TTY
behaviour. Those are deliberately excluded (`AGENTS.md` §9.3). If a
test's only failure mode is "I typo'd a default value", skip it.

## Things that will get a change sent back

- Heavy work in the fs-watch callback. It may only normalize, filter,
  and record event metadata — no DB, hash, or network work
  (`AGENTS.md` §3).
- Business logic in `apps/*` instead of `core/*`.
- `#[cfg(target_os)]` sprinkled through engine code instead of a
  `core/platform` trait.
- Secrets reached without `core/platform/secrets::SecretStore`, or
  anything that can put a token in a log.
- Silent overwrite in `two-way` mode. Default conflict policy is keep
  both, always.
- Comments referencing task ids, wave numbers, or `docs/tasks/*`
  (`AGENTS.md` §8.9). Stable docs are fine to reference; the work
  schedule is not.
- Hardcoded config keys, env var names, or defaults instead of the
  constants modules (`AGENTS.md` §8.6).

## Commits and pull requests

Use Conventional Commit subjects, `<type>: <why-focused summary>`, with
one dominant type per commit: `feat`, `fix`, `perf`, `refactor`, `docs`,
`test`, `ci`, `build`, `chore`, `release`. Split mixed changes so
release-note grouping stays accurate, and label the PR to match.

**Before committing anything non-trivial**, add release-note lines to
the `Unreleased` section of [`CHANGELOG.md`](CHANGELOG.md).

**Push protection is on.** GitHub rejects a push whose commits contain
anything shaped like a known credential (API keys, OAuth tokens, private
keys). If the match is real, do not bypass: rotate the credential first,
then remove it from history before pushing again. If it is a test
fixture, bypass with *used in tests* and keep the fixture obviously fake
(`ya29.test`, not a realistic-looking token). Secrets belong in the
platform `SecretStore`, never in the tree — see
[`SECURITY.md`](SECURITY.md).

Update docs in the *same* change set: the relevant `docs/` group (plus
its group `README.md` if you add, rename, or remove a file), the
per-surface plan and task list you touched, and the root `README.md`
**Features** / **Configuration** sections if capabilities or config
changed.

Your PR description should answer:

1. What user or reliability problem does this solve?
2. How does it preserve low-impact behaviour?
3. What durability or failure paths were validated?
4. What tests were added or updated?
5. What docs and contracts were updated?

Plus scope and non-goals, risk and rollback, and any
migration/compatibility implications. If you intentionally skipped docs,
say why.

## Reporting bugs

Open an issue with the version (`vapor --version`), platform, what you
expected, what happened, and repro steps. `vapor doctor` output and a
redacted `vapor support-bundle` help a lot.

**For security problems, do not open an issue** — follow
[`SECURITY.md`](SECURITY.md).
