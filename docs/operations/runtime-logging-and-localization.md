# Runtime Logging and Localization

## Runtime root and layout

- Vapor runtime artifacts are rooted at a single directory controlled by `VAPOR_DIR`.
- Default runtime directory:
  - app and daemon runtime: `~/.vapor`
  - local dev plus tests/CI via repository scripts: `./.vapor`
- Runtime layout:
  - config: `<vapor_dir>/vapor.json`
  - logs: `<vapor_dir>/logs/vapor.logs`, `<vapor_dir>/logs/vapord.logs`
  - state/db reserved path: `<vapor_dir>/state/vapor.sqlite`

Runtime directory is not a `vapor.json` option and is resolved by precedence:

1. `VAPOR_DIR` environment override
2. `./.vapor` in tests/CI or when `VAPOR_ENV=dev`
3. `~/.vapor` in normal runtime

## Logging behavior

- Runtime log level override: `VAPOR_LOG_LEVEL` (`debug`, `info`, `warning`, `error`).
- Log line format: `{timestamp} [{level}] ({component}): {message}. key=value ...`
- Default log level behavior:
  - `VAPOR_ENV=dev` -> `debug`
  - `VAPOR_ENV=prod` (or unset) -> `info`

## UI localization

- Source-of-truth user-facing app copy catalogs live at `assets/locales/*.json`.
- Swift build/test/package scripts sync those catalogs into `apps/macos/Sources/VaporCore/Resources/locales/*.json` before bundling.
- Current catalog set includes `en.json` (English).
- Language resolution order: `preferredLanguageCode` override (if set), then device preferred languages, then English fallback.
- If a requested language catalog is unavailable, Vapor falls back to English.
- Logs remain English-only by design.
