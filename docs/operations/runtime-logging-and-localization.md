# Runtime Logging and Localization

## Runtime root and layout

- Vapor runtime artifacts are rooted at a single directory controlled by `VAPOR_DIR`.
- Default runtime directory:
  - app and daemon runtime: `~/.vapor`
  - local dev plus tests/CI via repository scripts: `./.vapor`
- Runtime layout:
  - config: `<vapor_dir>/vapor.json`
  - logs: `<vapor_dir>/logs/vapor.logs` (app), `<vapor_dir>/logs/vapord.logs`
    (daemon), plus the service manager's `vapord.stdout.log` /
    `vapord.stderr.log` redirects; each rotates at 8 MiB keeping three
    generations
  - durable state: `<vapor_dir>/state/vapor.sqlite` for the implicit
    `default` profile, `<vapor_dir>/state/profiles/<id>/vapor.sqlite`
    per configured profile, and `<vapor_dir>/state/lifecycle.json` for
    crash-loop bookkeeping; a corrupt database is quarantined next to
    itself as `vapor.sqlite.corrupt-<ms>` before a fresh one is created

Runtime directory is not a `vapor.json` option and is resolved by precedence:

1. `VAPOR_DIR` environment override
2. `./.vapor` in tests/CI or when `VAPOR_ENV=dev`
3. `~/.vapor` in normal runtime

- Relative `VAPOR_DIR` overrides are normalized against the current working directory.
- Runtime directories and files should use restrictive local permissions (`0700` for directories, `0600` for config/log/state files).

## Logging behavior

- Runtime log level override: `VAPOR_LOG_LEVEL` (`debug`, `info`, `warning`, `error`).
- Log line format: `{timestamp} [{level}] ({component}): {message}. key=value ...`
- Logging must redact sensitive metadata keys and common inline auth/token patterns.
- Rust daemon logging falls back safely instead of panicking if the log file cannot be opened.
- Default log level behavior:
  - `VAPOR_ENV=dev` -> `debug`
  - `VAPOR_ENV=prod` (or unset) -> `info`

## UI localization

- Source-of-truth user-facing app copy catalogs live at `assets/locales/*.json`.
- Swift build/test/package scripts sync those catalogs into `apps/macos/Sources/VaporCore/Resources/locales/*.json` before bundling.
- Current catalog set includes `en.json` (English).
- Language selection comes from persisted `languageCode` (default `en`), and Vapor falls back to English if that catalog is unavailable.
- If a requested language catalog is unavailable, Vapor falls back to English.
- Logs remain English-only by design.
