# Runtime Logging and Localization

## Runtime root and layout

- Vapor runtime artifacts are rooted at a single directory controlled by `VAPOR_DIR`.
- Default runtime directory:
  - app and daemon runtime: `~/.vapor`
  - local dev plus tests/CI via repository scripts: `./.vapor`
- Runtime layout:
  - config: `<vapor_dir>/vapor.json`
  - logs: `<vapor_dir>/logs/vapor.log` (app), `<vapor_dir>/logs/vapord.log`
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

## The support report

`vapor report` copies `vapor.json`, the logs, and the daemon's live
state into `vapor_dir/reports/` (the five newest stay). The logs hold
no credential, and they do hold the user's folder and file names, so by
default the report replaces every name by a stable code
(`core/cli/src/commands/redact.rs`). One name gives one code in every
file of every report, so a maintainer can still follow one file through
the logs.

- The report is read twice. The first pass learns the names: every
  component of every path it finds. The second pass replaces the sync
  roots, the home directory, and the device id by a tag
  (`<local-root:default>`), the whole value of each field that holds a
  path or a name, each word with a path separator in it, each learned
  name where it stands alone in a sentence (a question that names a
  file), and each ignore rule that is not one Vapor ships with.
- What a maintainer needs stays readable: a count (`2/2`), a byte
  range, a rate (`kbit/s`), the build of the daemon (`vapord/<version>`),
  and the host and the fixed words of a URL. The rest of a URL's path
  becomes a code and its query goes.
- What can remain: a name that stands alone in a sentence when it is
  one plain lowercase word (`notes`), which cannot be told from the
  sentence's own words, or when no path in the report holds it. The
  manifest and the command's own output say so, and the user reads the
  report before sharing it.
- `--keep-paths` writes the names as they are, for a report that is
  not shared in public. The manifest says which of the two it is.
- The redaction of drive-letter and backslash paths is open work for
  the Windows surface (`docs/tasks/core.md`).

## UI localization

- Source-of-truth user-facing app copy catalogs live at `assets/locales/*.json`.
- Swift build/test/package scripts sync those catalogs into `apps/macos/Sources/VaporCore/Resources/locales/*.json` before bundling.
- Current catalog set includes `en.json` (English).
- Language selection comes from persisted `language` (default `en`), and Vapor falls back to English if that catalog is unavailable.
- `./scripts/lint.sh` checks that every catalog parses as JSON.
- Logs remain English-only by design.
