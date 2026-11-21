# core/cli: `vapor` CLI

Headless-first control plane for the Vapor runtime. The same crate that
ships the binary also exposes per-command logic so unit tests can drive
each command without spawning the binary.

Authoritative reference: `docs/plans/cli.md`.
Tasks: `docs/tasks/cli.md`.

## Commands

`init`, `status`, `pause`, `resume`, `reconcile`,
`profiles list|add|remove`, `decisions list|show|resolve`, `ignored`,
`trash list|restore|empty`, `config get|set`,
`auth login|logout|status`, `service bootstrap|install|uninstall|start|stop|restart|status|check|acknowledge`,
`timeline`, `diagnostics`, `logs`, `doctor`, `report`, `run`, `version`.
Every command that reports state takes `--json`: keys are camelCase and
values kebab-case, the same shape the daemon's IPC status uses. The
shapes are locked by assertion tests in each command module. `vapor
--help` is the reference.

## Install from source

```bash
cargo install --path core/cli
```

Drops the `vapor` binary into `~/.cargo/bin`. The CLI has no download
of its own: it ships inside the install package of each platform,
whose installer puts it on the `PATH`. Build-from-source is the path
for a host that has no package.

## Layout

- `src/lib.rs`: library entry point exposing per-command modules.
- `src/main.rs`: `clap` wiring; converts subcommands into library
  calls and renders results.
- `src/commands/<command>.rs`: one module per command. Each owns its
  command-specific types, error enum, and unit tests.

## Scripts

The cross-stack `./scripts/{format,lint,test,build}.sh` cover the CLI
because `cargo` operates on the whole workspace; there are no CLI-only
wrappers. `./scripts/e2e.sh` drives the built `vapor` binary against a
real daemon in a sandbox.
