# providers

Rust provider modules implementing cloud integrations.

Current plan:

- `provider_gdrive` first (bidirectional MVP)
- `provider_s3`/R2 next (capability-aware behavior)

All providers implement a shared trait and map errors into core engine taxonomy.
