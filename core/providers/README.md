# providers

Rust provider modules implementing cloud integrations.

Current plan:

- `provider_gdrive` first (bidirectional MVP)
- `provider_s3`/R2 next (capability-aware behavior)

All providers implement a shared trait and map errors into core engine taxonomy.

Logging:

- Provider modules can use shared structured logging from `vapor-shared`.
- Default provider logs target `<vapor_dir>/logs/vapord.logs` where `vapor_dir` comes from `VAPOR_DIR`.
