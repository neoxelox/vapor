# core/cli — `vapor` CLI

Headless-first control plane for the Vapor runtime. The same crate that
ships the binary also exposes per-command logic so unit tests can drive
each command without spawning the binary.

Authoritative reference: `docs/plans/cli.md`.
Tasks: `docs/tasks/cli.md`.

## Commands

`run`, `service install|uninstall|bootstrap|start|stop|restart|status|check|acknowledge`,
`config get|set`, `auth login|logout|status`, `status`, `pause`, `resume`,
`flush-now`, `reconcile`, `sync-now`, `timeline`, `logs`, `diagnostics`,
`conflicts list|resolve`, `support-bundle`, `doctor`, `version`. Every
command that reports state takes `--json`; the shapes are locked by
assertion tests in each command module. `vapor --help` is the reference.

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
