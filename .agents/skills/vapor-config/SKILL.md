---
name: vapor-config
description: Adds or changes a vapor.json key, a VAPOR_* environment variable, a default value, a runtime path name, or a launch label by walking the constants-first checklist (Rust source of truth, Swift mirror, call sites, CLI typing, live-reload class, README Configuration, .env.example, tests, e2e). Use whenever a change touches core/shared/src/constants.rs or introduces a new setting anywhere.
license: GPL-3.0-only
---

# Add or change a setting

Constants are centralised; a literal `VAPOR_*` string or a default value
outside the constants modules is a bug.

## Checklist

1. **Rust source of truth.** `core/shared/src/constants.rs`: add the
   `KEY_*` (or `VAPOR_*` env name, path name, label, default) to the
   right module, with a doc comment stating what it does and its unit.
   Add config keys to `config::ALL_KEYS` and to exactly one of
   `config::LIVE_RELOAD_KEYS` (a running daemon applies it) or
   `config::RESTART_REQUIRED_KEYS` (takes effect on the next start), or
   leave it out of both only if the daemon never reads it. The
   `every_documented_key_is_classified_exactly_once` test enforces this.
2. **Config model.** `core/shared/src/config.rs`: the `VaporConfig`
   field with its serde name and default, loaded leniently per key
   (`field(...)`), and the `Serialize` derive so `vapor config get` can
   render the default. Profile-capable keys get an `Option` on
   `ProfileConfig` and a merge rule in the daemon.
3. **Swift mirror.** `apps/macos/Sources/VaporCore/VaporConstants.swift`
   mirrors keys, env names, defaults and labels the app could read.
   Keep the two files in the same order.
4. **Call sites.** Consume the constant everywhere; grep for the
   literal to be sure nothing re-declares it.
5. **CLI typing.** `core/cli/src/commands/config.rs::parse_value_for_key`:
   booleans, integers, enums and structured (JSON) keys each have a
   parser that rejects bad input with the expected shape; a new key
   gets one, plus a test.
6. **Live reload.** If the key is live, `core/daemon/src/config_reload.rs`
   `diff` must compare it and `MultiProfileRuntime::apply_config_changes`
   must apply it; add the case to the reloader test.
7. **Docs.** Root `README.md` **Configuration** table (key, type,
   default, behaviour, and whether it applies live), `.env.example` for
   env vars, and the architecture or operations doc that owns the
   behaviour.
8. **Tests and e2e.** Unit tests for the parser and the consumer; extend
   e2e S1 (config round trip) or S22 (live reload) when the key is
   observable through the CLI.
9. **CHANGELOG.** One line under `Unreleased`.

## Rules

- Runtime directory selection is env-only (`VAPOR_DIR`, `VAPOR_ENV`);
  never make it a `vapor.json` key.
- Reverse-DNS identifiers live under `sh.arn.vapor.*`; binaries are
  `vapor`, `vapord`, `Vapor`.
- Pre-GA there is no config migration: change the shape, document it in
  the same change set, and let old files load leniently.
