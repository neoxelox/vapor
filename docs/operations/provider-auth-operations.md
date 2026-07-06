# Provider Auth Operations

## Scope

Operational policy for cloud-provider authentication and token
lifecycle. Google Drive is the first real provider; the policies here
are provider-neutral unless a section says otherwise.

## OAuth flow baseline (Google Drive)

- OAuth 2.0 with PKCE (`S256` code challenge, RFC 7636).
  `vapor auth login gdrive` runs the whole flow: it starts a
  loopback listener on `127.0.0.1`, opens the consent page in the
  default browser, receives the redirect, and exchanges the code.
- Client credentials are per-deployment, supplied via environment:
  - `VAPOR_GDRIVE_CLIENT_ID` (required)
  - `VAPOR_GDRIVE_CLIENT_SECRET` (optional; Google issues one even for
    installed apps, where it is not treated as confidential)
- Requested scope is `https://www.googleapis.com/auth/drive` (full
  Drive): Vapor syncs a user-chosen folder that other tools may also
  write, which the per-app `drive.file` scope cannot see.
- Tokens are stored only in the platform `SecretStore`
  (`core/platform/secrets`; Keychain on macOS) under the
  profile-namespaced key `auth.{profile_id}.{provider}.token`, as a
  JSON document `{accessToken, refreshToken, expiresAtMs}`.

## Token lifecycle policy

- Access tokens refresh proactively 60 seconds before `expiresAtMs`.
- A `401` on an API call triggers one forced refresh + retry before the
  failure is surfaced.
- Google usually omits the refresh token in refresh responses; the
  previous refresh token is retained so the chain never breaks.
- Failure classification (`ProviderErrorKind`):
  - `invalid_grant` / `invalid_client` → `Authentication`
    (user-action-required; the message names the exact command:
    `vapor auth login gdrive`).
  - Token-endpoint 5xx or transport failure → `Transient` (bounded
    retries with backoff through the normal retry policy).
- Missing client credentials surface as an actionable `Authentication`
  error at cloud-root ensure time; the engine blocks sync (C8-50) while
  continuing to capture intent state durably.

## Failure behavior

- Repeated refresh failures degrade to a blocked-sync state with the
  reason visible in `vapor status` / the timeline; queued work is
  preserved, never dropped.
- Re-auth (`vapor auth login gdrive --profile <id>`) replaces the
  stored token set; the daemon picks it up on its next provider call.
- `vapor auth logout <provider> --profile <id>` removes the stored
  token; `vapor auth status` reports bound/not-bound without ever
  revealing token values.

## Security requirements

- Never log access/refresh tokens or auth headers (the structured
  logger redacts by policy; OAuth error responses log only the error
  code, never the body verbatim).
- Redact sensitive identifiers in auth and provider logs.
- Least-privilege scopes where the product model allows it (see the
  scope rationale above).
- Tokens never enter `vapor.json`, the durable state DB, or support
  bundles.

## Operational checklist (per deployment)

- Create the OAuth client (type "Desktop app") in the Google Cloud
  console; enable the Drive API on the project.
- Provide `VAPOR_GDRIVE_CLIENT_ID` (+ secret if issued) to the daemon
  and CLI environment (see `.env.example`).
- Verify redirect (`http://127.0.0.1:<port>`) is acceptable for the
  client type (Desktop-app clients allow loopback redirects by
  default).
- Test revoke/re-auth: revoke the grant in the Google account, confirm
  the daemon degrades to the actionable `Authentication` state, re-run
  login, confirm sync resumes without intent loss.
