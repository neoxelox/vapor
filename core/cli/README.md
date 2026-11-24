# core/cli: `vapor` CLI

Headless-first control plane for the Vapor runtime. The same crate that
ships the binary also exposes per-command logic so unit tests can drive
each command without spawning the binary.

Authoritative reference: `docs/plans/cli.md`.
Tasks: `docs/tasks/cli.md`.

## Commands

`init`, `status`, `pause`, `resume`, `reconcile`,
`profiles list|add|enable|disable|rename|remove`, `decisions list|show|resolve`, `ignored`,
`trash list|restore|empty`, `config get|set`,
`auth login|logout|status`, `service bootstrap|install|uninstall|start|stop|restart|status|check|acknowledge`,
`timeline`, `diagnostics`, `logs`, `doctor`, `report`, `run`, `version`.
Every command that reports state takes `--json`: keys are camelCase and
values kebab-case, the same shape the daemon's IPC status uses. The
shapes are locked by assertion tests in each command module. `vapor
--help` is the reference.

## Exit codes

Three codes, the same for every command. A script and an app read the
code first and the output second.

| Code | Meaning | Output |
|---|---|---|
| `0` | The command did what it was asked. "Nothing to do" counts: an empty list, a profile that is enabled already. | The result on stdout. |
| `1` | The command could not do it, or the answer is no. | For an error, one line on stderr that starts with `vapor: `, and nothing on stdout, also with `--json`. For an answer that is a no, with `--json` the answer is on stdout as for code 0. In text form, a request that the daemon refused (`pause`, `resume`, `reconcile`) is one line on stderr, `<command>: not accepted; <why>`, and an install that still waits for a person or has a failed step (`init`) and a failed check (`doctor`) print their report on stdout. |
| `2` | The command line is wrong: an unknown command, an unknown option, a missing value. | The usage message on stderr. |

A command that needs the daemon exits with `1` within seconds when no
daemon answers; it never waits for one. `core/cli/tests/binary.rs`
asserts the codes on the commands that need no service of the host
(`service`, `report`, and `run` are not among them), and
`core/cli/tests/fake_daemon.rs` asserts the commands that talk to the
daemon against a stand-in for it.

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
