# core/cli — `vapor` CLI

Headless-first control plane for the Vapor runtime. The same crate that
ships the binary also exposes per-command logic so unit tests can drive
each command without spawning the binary.

Authoritative reference: `docs/plans/cli.md`.
Tasks: `docs/tasks/cli.md`.

## Wave status

- **Done (Wave 6, phase 1):** crate skeleton, `clap` wiring,
  `vapor --version`, `vapor run`, `vapor config get|set`,
  `vapor version`, `vapor doctor`, and
  `vapor service install|uninstall|start|stop|restart|status`
  (`cli.md` L0-1 … L0-4, L1-1 … L1-4, L2-1 … L2-4). The macOS service
  surface drives `core/lifecycle::DaemonLifecycleManager` over
  `core/platform::NativeServiceInstaller`.
- **Pending (Wave 6, phase 2):** IPC channel + skew matrix tests
  (`cli.md` C5 / `core.md` C5-1 … C5-5).
- **Pending (Wave 7):** `vapor status / pause / resume / flush-now /
  reconcile / timeline / logs` and `vapor auth login|logout|status`
  (`cli.md` L3, L4).

## Install from source

```bash
cargo install --path core/cli
```

Drops the `vapor` binary into `~/.cargo/bin`. Pre-Wave-11 there is no
signed release artifact yet; build-from-source is the supported path.

## Layout

- `src/lib.rs` — library entry point exposing per-command modules.
- `src/main.rs` — `clap` wiring; converts subcommands into library
  calls and renders results.
- `src/commands/<command>.rs` — one module per command. Each owns its
  command-specific types, error enum, and unit tests.

## Scripts

The cross-stack `./scripts/{format,lint,test,build}.sh` cover the CLI
because `cargo` operates on the whole workspace; there are no CLI-only
wrappers. `./scripts/e2e.sh` drives the built `vapor` binary against a
real daemon in a sandbox.
