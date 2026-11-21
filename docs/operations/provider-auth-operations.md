# Provider Auth Operations

## Scope

Operational policy for cloud-provider authentication and token
lifecycle. Google Drive is the first real provider; the policies here
are provider-neutral unless a section says otherwise.

## OAuth flow baseline (Google Drive)

- OAuth 2.0 with PKCE (`S256` code challenge, RFC 7636).
  `vapor auth login` runs the whole flow: it starts a
  loopback listener on `127.0.0.1`, opens the consent page in the
  default browser, receives the redirect, and exchanges the code. It
  waits up to five minutes for the consent. It runs the browser flow
  only from a terminal; `--browser` runs it without one (the app's
  Sign In button). A token made elsewhere goes in on stdin (`<command
  that prints the token> | vapor auth login --token -`); a token given
  as an argument is refused and not stored, because an argument shows
  in the process list and stays in the shell history.
- `vapor init` signs in before the profile exists, so its folder step
  can look into the account. The token is stored under the id that the
  first profile gets (`default`) and the provider. It stays when the
  user stops before the folders: the next `vapor init` uses it, and
  `vapor auth logout` removes it. When the profile is then written for
  another provider, the sign-in of the first one is removed.
- No backend takes part. PKCE and the loopback redirect protect the
  code exchange, and the app talks to Google directly.
- Requested scope is `https://www.googleapis.com/auth/drive` (full
  Drive): Vapor syncs a user-chosen folder that other tools may also
  write, which the per-app `drive.file` scope cannot see.
- Tokens are stored only in the platform `SecretStore`
  (`core/platform/secrets`; Keychain on macOS) under the
  profile-namespaced key `auth.{profile_id}.{provider}.token`, as a
  JSON document `{accessToken, refreshToken, expiresAtMs}`.

## The OAuth client

- One OAuth client of type "Desktop app" serves every install. Its id
  and secret are compiled into `vapor` and `vapord` from the build
  environment's `VAPOR_GDRIVE_CLIENT_ID` and
  `VAPOR_GDRIVE_CLIENT_SECRET`
  (`core/providers/src/gdrive/oauth.rs`, `client_credentials`).
- The secret ships in the binary on purpose. Google documents that an
  installed app cannot keep a secret and does not treat it as one, yet
  its token endpoint still asks a Desktop client for it.
- The values never enter the repository. It is public, and Google's
  API terms ask a developer to stop other apps from reusing its
  credentials. Release builds read them from the `release-<platform>`
  environment secrets; a stable tag refuses to package without them
  and a prerelease ships without Drive sign-in and says so
  (`release-process.md`). Local builds read them from the gitignored
  `.env` through `scripts/dotenv.sh`, which `scripts/rust/build.sh`
  and `apps/macos/scripts/package.sh` source; plain `cargo build` sees
  only the shell's environment.
- The same variables at run time override the built-in client. The id
  and secret always come from one place: pairing an overriding id with
  the built-in secret would fail every token request with
  `invalid_client`.
- `vapor doctor` names the client a binary uses in its
  `gdrive-oauth-client` row: built in, from the environment, or none.
  It prints the id, which is public, and never the secret.
- The e2e harness and the soak build their binaries without a client
  and refuse a filesystem-provider run over binaries that carry one. A
  sandbox has a secret scope of its own (see below), so it cannot read
  the sign-in of the real install; the refusal is the second barrier,
  and it keeps a test binary from ever starting a real sign-in.

## Consent screen status and token lifetime

Google's rules for the consent screen decide how long a sign-in lasts:

- **Testing.** Only test users added by hand (up to 100) can sign in,
  and Google expires every refresh token after 7 days. The profile
  then signs out once a week; the sign-in hold below keeps that to a
  click.
- **In production, unverified.** Refresh tokens stop expiring. Every
  user sees a "Google hasn't verified this app" screen once at
  consent, and the client is capped at 100 users.
- **In production, verified.** The full `drive` scope is a restricted
  scope: Google requires app verification (a verified domain, a
  homepage, a privacy policy, a demo video) and a yearly third-party
  security assessment before the client serves the public.

## One scope for each runtime directory

A secret belongs to the runtime directory that stored it. The default
directory (`~/.vapor`) uses the plain name
(`auth.default.gdrive.token`). Any other directory (`VAPOR_DIR`, a dev
checkout's `./.vapor`, an e2e sandbox) puts `dir-<hash of the
directory>.` before the name, and its commands list, read, and remove
only names with that prefix. A sandbox therefore cannot use or remove
the sign-in of the real install, and a sign-in made in one directory
is not visible in another: sign in again there.

## Where tokens live on macOS

- Each secret is one generic-password item in the user's login
  keychain. The service attribute is `sh.arn.vapor` and the account
  attribute is the secret name (`auth.default.gdrive.token` for the
  default profile), so Keychain Access lists every Vapor entry under
  one search term and a user can remove them by hand.
- The item is created with an access list naming the binary that
  wrote it plus its companions (`vapor` and `vapord`, side by side in
  a build directory or at `Contents/Helpers/vapor` and
  `Contents/MacOS/vapord` inside the bundle). The daemon therefore
  reads a token the CLI stored without a keychain prompt. A store over
  an item that exists removes the item and adds it again, because only
  the creation writes the access list: a sign-in after an update gives
  the item the list of the binaries that run now.
- The daemon never opens a keychain dialog. It reads with the
  interaction turned off, so a read that would need the user's consent
  fails, and the profile holds for a sign-in (below). A dialog from a
  background process would sit unseen and block the read.
- Development builds and prerelease builds are not signed with a
  Developer ID, and macOS identifies such a binary by its content
  hash. A rebuilt `vapord`, or an updated prerelease app, is a new
  identity that the item's access list does not name. The daemon's
  read then fails, the profile asks for a sign-in, and `vapor auth
  login` stores the item again for the new binaries. A release signed
  with a Developer ID is identified by its code requirement and keeps
  its access across updates.
- Without a GUI session (SSH, CI) the keychain refuses any operation
  that would need a prompt with `errSecInteractionNotAllowed`; the
  error surfaces verbatim in `vapor auth` output and in the daemon's
  `Authentication` state.

## Where tokens live on Linux

- With `VAPOR_SECRETS_COMMAND` set, every secret goes through that
  program: `<command> get <name>` prints it, `<command> set <name>`
  reads it on stdin, `<command> delete <name>` removes it, `<command>
  list` prints one name per line, exit 1 means not found. A `pass`
  wrapper is a few lines of shell; the daemon and the CLI both call
  it, so it must work without a terminal. This is the headless
  choice, and it wins over the desktop store when set.
- Otherwise, on a desktop with a session bus and `secret-tool`
  (libsecret's CLI, package `libsecret-tools` on Debian and Ubuntu),
  each secret is a Secret Service item with the attributes `service =
  sh.arn.vapor` and `name = <secret name>`, so Seahorse or any keyring
  UI lists every Vapor entry under one search.
- With neither, `vapor auth login` warns with the variable to set and
  keeps the token in process memory only; a token is never written to
  a plaintext file. `vapor doctor` names the backend in use.
- Windows has no native store yet; the CLI warns the same way.

## Token lifecycle policy

- Access tokens refresh proactively 60 seconds before `expiresAtMs`.
- A `401` on an API call triggers one forced refresh + retry before the
  failure is surfaced.
- Google usually omits the refresh token in refresh responses; the
  previous refresh token is retained so the chain never breaks.
- Failure classification (`ProviderErrorKind`):
  - `invalid_grant`, `invalid_client`, `unauthorized_client` →
    `Authentication` (user-action-required; the message names the
    exact command: `vapor auth login`).
  - `temporarily_unavailable`, `server_error`, `slow_down`,
    `rate_limit_exceeded`, any 5xx or 429, or a transport failure →
    `Transient` (bounded retries with backoff through the normal retry
    policy). A busy token endpoint is not a lost sign-in.
  - Any other error code → `Authentication`.
  - The daemon logs a rejected token request as a WARNING with the
    code under `error_code`, never the body.
- One refresh runs at a time for a profile: two calls that find the
  token expired share one request, and so do the calls that Drive
  answered with `401` for the same token. A dead token endpoint is
  asked one time, not one time for each worker.
- A daemon with no OAuth client (none built in, none in the
  environment) cannot build the provider: the profile is suspended
  with the reason in `vapor status`, and the other profiles keep
  running. That is a build problem, not a sign-in, so it does not use
  the hold below.

## Failure behavior: the sign-in hold

A refused sign-in is a condition of the whole profile, never of one
file, so it holds the profile instead of failing work
(`core/daemon/src/runtime.rs`, `mark_sign_in_required`).

- **What counts.** Any provider call that fails with
  `ProviderErrorKind::Authentication`: the token endpoint answers
  `invalid_grant` or `invalid_client`, a request still gets `401` after
  its one forced refresh, or no token is stored for the profile. A
  `403` never counts; Drive's 403s are rate limits, quota, or
  per-file permissions.
- **While held.** Nothing is leased and the changes feed is not
  polled; ingest keeps recording local changes durably. An intent whose
  transfer hit the refusal goes back to the queue without spending a
  retry attempt.
- **What the user sees.** The profile's `signInRequired` is `true`
  in `vapor status --json`, its run state is `error` with the reason
  `Vapor needs to sign in to gdrive again: sync is on hold until then
  (vapor auth login --profile <id>)`, `vapor status` prints a
  `Sign-in required` line, and the timeline records a `sign-in` entry.
  The daemon logs one WARNING when the hold starts and an INFO line
  when it lifts. In the macOS app the menu bar mark turns orange, and
  the Dashboard and the menu show the notice with a Sign In button,
  which runs `vapor auth login --profile <id> --browser` from
  the bundled CLI.
- **Recovery.** `vapor auth login` replaces the stored token set. The
  provider dropped its cached tokens when Google refused them, so the
  daemon's next root check (every 15 seconds) reads the secret store
  again, gets through, and lifts the hold. The queue resumes as it
  stood, deletions included, with no restart. The provider also
  remembers the refused refresh token: while the store still holds it,
  a check fails without another token request.
- **Signed out at start.** A daemon that cannot probe the cloud root at
  start treats the root as not yet ensured. The first probe that gets
  through after the sign-in runs the recovery a returning root runs:
  it drops the queued deletions and schedules a whole-scope reconcile,
  which re-derives them from the sync index.
- `vapor auth logout --profile <id>` removes every credential the
  profile holds; `vapor auth status` reports each profile as
  `signed-in`, `signed-out`, or `not-needed` without ever revealing
  token values.
- `--profile` names the profile; its provider decides what signing in
  means. With one profile the flag is optional; with several, a command
  that acts on one profile refuses without it and lists the ids.

## Security requirements

- Never log access/refresh tokens or auth headers (the structured
  logger redacts by policy; OAuth error responses log only the error
  code, never the body verbatim).
- Redact sensitive identifiers in auth and provider logs.
- Least-privilege scopes where the product model allows it (see the
  scope rationale above).
- Tokens never enter `vapor.json`, the durable state DB, or support
  bundles. `vapor report` also replaces file and folder names with
  stable codes unless `--keep-paths` is given.
- A token never goes on a command line.

## Operational checklist (per deployment)

- In the Google Cloud console, under the account that owns Vapor's
  client: create a project, enable the Google Drive API, configure the
  consent screen (External; add the test users while it is in
  Testing), and create an OAuth client of type "Desktop app".
- Store the id and secret as the `VAPOR_GDRIVE_CLIENT_ID` and
  `VAPOR_GDRIVE_CLIENT_SECRET` secrets of every `release-<platform>`
  environment, and in your local `.env` for local builds.
- Check a build: `vapor doctor` shows `gdrive-oauth-client: built into
  this build: <id>`.
- Loopback redirects (`http://127.0.0.1:<port>`) need no registration:
  Desktop-app clients accept them.
- Test revoke and re-auth: revoke the grant in the Google account,
  confirm the profile reports `signInRequired` and its queue stays
  put, sign in again, and confirm sync resumes within a root check
  without losing an intent.
