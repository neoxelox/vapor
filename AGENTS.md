# AGENTS.md

Operating rules for contributors, human and AI, working on `vapor`. This
file holds the durable rules: what the product is, where the boundaries
are, which invariants never move, and what "done" means. Step-by-step
procedures live in skills under `.agents/skills/` (index in §13); each
section below that used to spell out a procedure now points at its skill.

## 0) Start here

Reading order for a new session:

1. §1 to §6 of this file: intent, boundaries, invariants.
2. `docs/product/status-and-goals.md`, then `docs/architecture/README.md`
   and `docs/architecture/data-flow.md` for how the runtime works.
3. `docs/tasks/README.md` for what is next; the surface task file
   (`docs/tasks/{core,macos,cli}.md`) for the item you are touching;
   `docs/specs/` for the decisions behind the part you are changing.
4. The skill for the job (§13). `vapor-design` before building a
   feature; `vapor-validate` before every commit; `unslop` for every
   sentence you write.
5. `docs/development/runbook.md` for the scripts and the local loop.

Reports and replies to the project owner use Simplified Technical
English (§8.11). What an agent does without asking, and what waits for
the owner: §10.1.

## 1) Product intent and non-negotiables

- `vapor` is an invisible-first background sync product. The shipping surfaces today are the macOS app and the `vapor` CLI; Windows and Linux apps follow and consume the same portable Rust runtime.
- A user installs one thing and updates one thing. Each platform ships install packages that hold the app, the `vapor` CLI, and the `vapord` daemon, and the installer puts `vapor` on the `PATH`. Vapor ships no standalone CLI and no standalone runtime (owner, 2026-11-21; `docs/specs/2026-11-21-platform-release-assets.md`).
- The Rust core (`core/*`) is the single portable runtime that powers every surface. Apps under `apps/*` and the `vapor` CLI are UI + OS-integration shims over that runtime; no business logic lives in them.
- Platform-specific code is allowed and encouraged inside `core/platform` when it unlocks native performance; the engine consumes traits, not OS APIs directly.
- Primary priority is user device impact, not strict real-time sync.
- Vapor sync scope is a user-selected local directory replicated bidirectionally with a user-selected cloud directory.
- Vapor is not a full-device backup product and must never broaden scope beyond configured sync roots.
- A profile exists only when the user set its two roots. With no usable configuration (no `vapor.json`, a file that does not parse, no valid profile) the daemon stays up, syncs nothing, and creates no folder (owner, 2026-11-14).
- On a profile's first contact with its sync roots, Vapor creates the local root on-device, ensures the cloud root exists provider-side, and adopts both (`docs/architecture/data-flow.md` §Root identity). After that a missing root is waited for and never re-created on Vapor's own, and a root that is present without the adopted identity is put to the user as a decision; neither is ever mirrored as deletions.
- Core guarantees:
  - Never lose intent state.
  - Recover safely after crash/restart.
  - Defer under pressure and converge eventually.
- Bidirectional behavior is in MVP for Google Drive and must be safety-first.
- Feature parity is mandatory for the invariants above on every OS that currently ships a surface. Autolaunch, crash-loop protection, durable queue, throttle discipline, secret storage, and resource budgets must be delivered via the matching `core/platform` trait implementation; "skip it on the shipping OS" is never acceptable. An OS that is not yet a shipping surface (currently Windows; Linux has its native implementations but no app or release lane yet) may have `unimplemented!()` stubs behind the trait, provided the engine continues to compile on that OS so the door stays open.

## 1.1) Project maturity and compatibility policy

- Vapor is pre-GA and under heavy active development with no users yet.
- Backward compatibility is not guaranteed for local config/state/schema formats, app/daemon internal contracts, or developer-facing interfaces.
- Prefer clear, simple implementations over temporary legacy/migration shims unless the project owner explicitly requests compatibility preservation. Document a breaking change in `CHANGELOG.md` in the same change set.

## 1.2) Features and decisions

- A feature, a change to sync semantics, a redesign, or any change with
  more than one reasonable answer is designed with the project owner
  before it is built: facts first, then rounds of numbered questions
  with a recommendation each, until the owner agrees. Procedure: the
  `vapor-design` skill.
- Every answer is recorded as a numbered decision (D1, D2, ...) in the
  feature's spec, `docs/specs/YYYY-MM-DD-<slug>.md`. The spec outlives
  the build: it is how a later session learns why Vapor works this way.
- Recorded decisions hold until the owner changes them. A change that
  would contradict one goes back to the owner as a question. When the
  owner changes it, the spec gets a new decision that replaces the old
  one; the old one is marked, never rewritten.
- A fix with one obvious answer needs no spec. A runtime failure goes
  through `vapor-debug`, which ends in a fix plan the owner approves.
- An owner ruling written into this file or a skill carries who made it
  and when, as "(owner, 2026-11-14)", so a later session can tell a
  ruling from an agent's inference.

## 2) System boundaries

- Rust daemon (`core/daemon`)
  - Fs-watch ingest, debounce/coalescing, scheduler, throttle controller.
  - Durable queue/state, retry/backoff, reconcile, provider execution, live configuration reload.
- Providers (`core/providers`)
  - Cloud API integration via provider trait/capabilities.
  - No provider-specific assumptions in core engine.
- Shared contracts (`core/shared`)
  - Constants source of truth, configuration model, error taxonomies, runtime paths, logging.
- IPC channel (`core/ipc`)
  - Length-prefixed frames carrying Vapor's own tagged JSON envelopes
    (`{kind, payload}` requests, `{outcome, value}` responses; not
    JSON-RPC) between the daemon and every surface. Unix domain socket
    today; named pipe on Windows when that surface ships. Contract:
    `docs/architecture/ipc-contracts.md`.
- Platform layer (`core/platform`)
  - Traits + per-OS native implementations for fs-watch, service install,
    secret store, metrics sampling, idle detection, filesystem capabilities,
    and process supervision. Engine code consumes traits, not OS APIs.
- Daemon lifecycle (`core/lifecycle`)
  - `CrashLoopGuard`, `DaemonLifecycleManager`, `AutoLaunchSettingStore`.
    Every surface (macOS app, Windows app, Linux app, `vapor` CLI) consumes
    this layer; nothing reimplements it.
- `vapor` CLI (`core/cli`)
  - Headless-first binary that exposes the full runtime on every OS.
  - `vapor init` is the first run from a terminal. It and the
    onboarding of an app are the same onboarding: the same steps in
    the same order. A change to the steps of one changes the other in
    the same change set (owner, 2026-11-21).
- SwiftUI app (`apps/macos`)
  - UX, onboarding, settings, diagnostics, menubar state.
  - Delegates autolaunch, crash-loop, daemon control, and live status to
    `core/lifecycle` and the daemon by invoking the bundled `vapor` CLI
    (`Contents/Helpers/vapor`) as a subprocess with `--json` output; no
    lifecycle policy is implemented in Swift.
  - Restarts nothing. The supervisor job that every install registers
    is the one thing that restarts a crashed daemon, with the app open
    or closed; a surface only reads the state (`vapor service status`)
    (owner, 2026-11-21).
- Future apps (`apps/windows`, `apps/linux`) consume the same `core/*`
  stack; no business logic in UI code.

Do not move heavy compute into an app process or the fs-watch callback path.

## 2.1) macOS app component model and lifecycle semantics

- Treat the app window, menubar surface, and daemon as distinct runtime components.
- Login/startup flow should initialize Vapor as menubar-first (no automatic main-window presentation) while keeping daemon lifecycle bootstrap active.
- Closing the main window is a UI action only:
  - close the window
  - remove Dock presence for the UI app surface
  - keep menubar surface active
  - keep daemon runtime active
- Reopen flow (`Open Vapor` from menubar) must focus the existing main window when already open, or restore it when closed, without restarting the daemon.
- Full shutdown (`Quit Vapor` from menubar) must execute daemon stop/shutdown path and then terminate the app process.
- Never couple window-close behavior to daemon termination.

## 2.2) Naming conventions

- The macOS app/program name must be `Vapor`.
- The CLI binary name must be `vapor` (crate `core/cli`).
- The daemon binary name must be `vapord`.
- The daemon package/crate name should remain `vapor-daemon` for naming consistency.
- Brand/domain identifiers are fixed: brand `ARN`, domain `arn.sh`, bundle ID `sh.arn.vapor`.
- The macOS app bundle identifier must remain `sh.arn.vapor` unless the project owner explicitly changes it.
- Reverse-DNS identifiers used by Vapor (bundle IDs, helper IDs, launch labels, and related service identifiers) must be scoped under `sh.arn.vapor.*`.
- New binaries, CLIs, apps, and libraries must use consistent vapor naming (`vapor*`/`vapor-*`) and avoid unrelated names.

## 3) Performance and throttle invariants

- The fs-watch callback (FSEvents on macOS, ReadDirectoryChangesW on Windows, inotify on Linux) may only normalize/filter/record event metadata.
- No DB/hash/network work in callback.
- No provider I/O and no file hashing on the tick thread: transfers, probes, deletes, hashes, the changes poll, reconcile directory listings, and the cloud-root checks all run on worker threads and are harvested by a later tick. Two bounded exceptions remain, both open work in `docs/tasks/core.md`: the planner and the reconcile walk hash a file of up to 8 MiB in line, and the rare re-baseline of an expired changes cursor polls in line (owner, 2026-11-15).
- All expensive work must be throttle-state gated.
- Throttle states: `Idle`, `Light`, `Throttled`, `Suspended`.
- A throttle state makes sync slower. Only `Suspended` stops it, and a condition that can last never gives `Suspended`: a device on battery, a user at the device, and Low Power Mode slow sync down and never stop it. Critical thermal pressure is what suspends (owner, 2026-11-14).
- Under `Suspended`, uploads/hashing stop; only lightweight intent coalescing continues.
- Reconcile scans are interruptible and start in every state but `Suspended`, at the caps of that state. A whole-scope scan runs at start, once each 24 hours, and on the user's request (`vapor reconcile`, the app's Sync now).
- The resource ceilings are measured limits. The daemon reads its own CPU and memory and measures what the link carries; it never assumes a number, and more capacity means a higher rate (owner, 2026-11-14).
- Throttle inputs come from the host on macOS (CPU, power source, thermal state, Low Power Mode, memory, keyboard presence). Test harnesses pin neutral inputs with `VAPOR_THROTTLE_INPUTS=static`; never make that the production default.

## 4) Bidirectional sync safety requirements

- Implement loop prevention (`self_write_cache`, operation IDs, TTL discipline).
- Handle local/remote races deterministically.
- Default conflict policy: keep both (never silent overwrite).
- A deletion propagates on the evidence of the sync index only: the index has a row for the path, and the surviving copy is exactly what that row last saw. With no row, or a changed survivor, keeping data wins over honouring a deletion, and Vapor never deletes a cloud object that it did not compare. A modification time never decides that a deletion wins (owner, 2026-11-14).
- On a volume that folds case, a change in only the spelling of a name is a rename, carried to the other side as a move. When both sides changed the spelling, the cloud's spelling stays (owner, 2026-11-14).
- A file Vapor removes on this device goes to the trash (`core/daemon/src/trash.rs`, the `TrashBin` platform trait), never straight to an unlink, unless the user turned the trash off.
- Remote poll/apply pipeline must obey throttle and retry constraints.
- Equality checks in the reconcile walk are the rsync quick check on both sides (size plus the local mtime and the remote mtime the sync index recorded), never a whole-tree hash; a pair the index has no row for is verified once, not assumed converged; the upload planner hashes and converges identical content silently, and a change on one side only while the other side still equals the last synced hash is a transfer in that direction, never a conflict copy.
- Sync direction is selected by `syncMode` (`two-way` default; one-way
  `pull-only` / `push-only`), resolved per profile with the top-level value as
  the default. Vapor's "never lose data" / keep-both guarantee applies **only
  to `two-way`**. The one-way modes are an explicit, opt-in, per-profile
  exception: they are **strict mirror** (the non-authoritative side is driven
  to exactly match the declared source of truth, permanently overwriting
  divergent edits and removing extra content). They must never be enabled
  silently or inferred. The daemon holds a one-way profile until its
  data-loss warning is acknowledged for that profile, that mode, and that
  pair of roots; the warning is a decision, so every surface shows the same
  question and a hand-edited file cannot enable a mirror (owner,
  2026-11-14). The local trash keeps what a mirror removes on this device
  for the retention window; overwrites and cloud-side removals on a
  provider without a trash have no undo, so the warning is the safeguard.
  Full design: `docs/architecture/sync-modes.md`.

## 5) Data durability and migrations

- Queue/state storage must provide at-least-once intent semantics.
- Schema versions are explicit.
- Before GA the state database has no migrations. A database of another schema version, or a damaged one, is set aside next to itself, and a whole-scope reconcile rebuilds the index; with no index row nothing is deleted on either side (owner, 2026-11-14).
- Migrations start at GA. From then on forward migration and rollback behavior are defined and tested before a schema change ships.
- Corruption recovery path must be documented and observable (`docs/architecture/state-schema-migrations.md`).
- Pre-GA exception: contributors may make intentional breaking changes to config/state formats without migration when the change is documented and validated in the same change set.

## 6) Security and privacy

- Secrets/tokens only via `core/platform/secrets::SecretStore`: the login keychain on macOS (one generic-password item per secret under the `sh.arn.vapor` service, with an access list covering `vapor` and `vapord`), Credential Manager on Windows, Secret Service on desktop Linux, the external command named by `VAPOR_SECRETS_COMMAND` on headless Linux, never a plaintext file. Tests use the in-memory fake; the fake and the native store run the same contract test.
- A runtime directory other than the default one has its own scope in the store, so a dev checkout or a test sandbox never reads or removes the secrets of the real install. The daemon reads secrets without a prompt and holds for a sign-in when it cannot. A token never goes on a command line.
- Logs must redact secrets, tokens, auth headers, and sensitive identifiers.
- Telemetry is local-only unless explicitly designed otherwise.
- Any permissioned feature must degrade safely when denied.

## 6.1) Public repository

The repository is public on GitHub. Everything committed or pushed is
public and stays public: files, history, commit messages, branch names,
PR descriptions and comments.

- Never commit personal information: private email addresses, phone
  numbers, home addresses, account or resource IDs, tokens, secrets,
  environment variable values, paths copied from a real machine (a
  working directory, a real folder layout), the contents of private
  conversations or private repositories, and
  details of the owner's accounts, plans, or billing. What the
  repository already publishes (the maintainer's name and GitHub
  handle, the `arn.sh` domain, project names) is fine. So is an
  invented example path in a fixture or a document, with a common
  first name in it (owner, 2026-11-14).
- Raw output from any account (GitHub settings, Google Cloud, Apple
  Developer) and real logs, reports, or state from a `~/.vapor` go to
  `.context/` (gitignored), never into the tree. Record a setting by
  what it does, and a failure by a redacted excerpt.
- Test fixtures use values that are obviously fake (`ya29.test`), never
  a realistic credential or a real file name from someone's sync root.
- Before every commit and push, read the diff, the message, and the
  branch name for the above. If something private is already pushed,
  tell the owner at once; rewrite history only with the owner's
  approval.

## 7) Distribution and trust chain

Each platform owns its own trust chain. Cross-platform principles live here;
concrete per-platform policy lives under `docs/operations/<platform>/`.
The release procedure itself is the `vapor-release` skill and
`docs/operations/release-process.md`.

### 7.1) Shared principles

- Release artifacts are produced from a script-first pipeline, not an IDE
  archive flow. Every platform packaging script is CI-runnable.
- Each shipping platform gets an isolated GitHub Environment holding
  its release secrets (`release-macos`, `release-windows`, and
  `release-linux` all exist and carry the same protection; a
  platform's secrets go in when its surface ships), never
  repository-wide secrets. Secrets never cross-leak between platform
  release jobs.
  The `vapor` CLI has no environment of its own (§7.5).
- Signing follows the tag, on every platform. A stable tag ships only
  signed artifacts: the platform's `package` job refuses to run when its
  signing material is missing (the two Developer ID certificates and
  notarization on macOS, the EV certificate on Windows, the GPG key on
  Linux). A signature is inside the file that it signs; a release holds
  no detached signature file. A prerelease tag
  (`-alpha`, `-beta`, `-rc`) needs no signing material on any platform
  and ships unsigned when none is configured; when it is, the artifacts
  are signed all the same. The pre-GA tags are all prereleases, which is
  why no environment holds signing material yet.
- The Google Drive OAuth client is compiled into `vapor` and `vapord`
  from the platform environment's `VAPOR_GDRIVE_CLIENT_ID` and
  `VAPOR_GDRIVE_CLIENT_SECRET`, under the same tag rule: a stable tag
  refuses to package without the pair, a prerelease ships without Drive
  sign-in. The pair never enters the repository; local builds read it
  from the gitignored `.env`, and test builds leave it out
  (`docs/operations/provider-auth-operations.md`).
- `release.yml` packages through one `package (<platform>)` matrix
  entry per shipping platform, under that platform's environment, and
  one shared `publish` job. A platform joins by adding its entry and
  its steps in the same shape as the macOS ones; the recipe is
  `docs/operations/release-process.md` ("Adding a platform to the
  package matrix").
- Every platform release environment must be protected **before** its
  secrets are added: deployments restricted to one `v*` tag rule with no
  branch rule, a required reviewer, and `can_admins_bypass` set to
  `false` as soon as a second admin exists. These live in repository
  settings on purpose; the equivalent checks in workflow YAML are
  defence in depth, because that YAML is part of the ref being released.
- GitHub Actions are allowlisted in repository settings
  (`allowed_actions: selected`). Add a new third-party action to the
  allowlist **before** a workflow references it (nested actions inside
  a composite action included), then SHA-pin it. Current list and the
  commands: `docs/ci/overview.md`.
- App/daemon version compatibility rules must be maintained and tested
  per OS.
- Product release version source-of-truth is the repository root `VERSION`
  file; `scripts/version.sh` is the only entrypoint for bumps and sync;
  release tags must equal `v$(cat VERSION)`.
- Build provenance must keep semantic version and git commit SHA separate.
- AI contributors must never open a packaged app on their host (for
  example `open dist/Vapor.app`). A packaged app starts only inside a
  guest of Tier VM (§9.9), where an agent verifies how it behaves; the
  project owner judges how it looks (owner, 2026-11-21).

### 7.2) macOS distribution

Full policy: `docs/operations/macos/distribution-trust-chain.md`.

- Distribution model must include: code signing, hardened runtime,
  notarization, entitlement review.
- The Mac app is built for Apple silicon. There is no Intel Mac build
  (owner, 2026-11-14).
- The release asset is an installer, `Vapor-macos-arm64.pkg`. It puts
  `Vapor.app` in `/Applications` and links `/usr/local/bin/vapor` to
  the CLI inside the app, so the app is the one thing that is installed
  and updated (owner, 2026-11-21). The Developer ID Application
  certificate signs the app and the Developer ID Installer certificate
  signs the installer; both are notarized and stapled.
- `Vapor.app` must contain the three executables:
  - `Contents/MacOS/Vapor`
  - `Contents/MacOS/vapord`
  - `Contents/Helpers/vapor` (the CLI the app shim drives; it lives in
    `Helpers/` because the default macOS filesystem is case-insensitive
    and `vapor` would collide with `Vapor` inside `MacOS/`)
- The raw `.app` bundle directory remains a local packaging/validation
  artifact; the installer is the release asset.
- Runtime daemon launch must target only the bundled sibling binary
  (`Contents/MacOS/vapord`) and must not rely on global install paths.
- Xcode project/workspace support is optional convenience for debugging and
  must not become the release source of truth.
- LaunchAgent and login item behavior must be stable across upgrades.
- Apple bundle metadata (`CFBundleShortVersionString`, `CFBundleVersion`)
  uses Apple-valid version fields; commit SHA is stored in dedicated
  `VaporVersion` / `VaporGitCommit` app/daemon build-info fields for logs,
  UI, and `--version` output.

### 7.3) Windows distribution (future)

Lands with `apps/windows`, under the isolated `release-windows` GitHub
Environment: one installer for x64 and one for ARM64. Expected
controls: `docs/plans/core.md` §6.

### 7.4) Linux distribution (future)

Under the isolated `release-linux` GitHub Environment: a `.deb` and an
`.rpm` for x64 and for ARM64. A package installs on a host with no
desktop, where the CLI and the daemon work, and it may ship before the
Linux app window exists (owner, 2026-11-21). Expected controls:
`docs/plans/core.md` §6.

### 7.5) Release assets and the CLI

- An asset is named `Vapor-<os>-<arch>.<ext>`, so the name says which
  device it is for: `<os>` is `macos`, `windows`, or `linux`, and
  `<arch>` is `arm64` or `x64`. Every build is 64-bit (owner,
  2026-11-21).
- The CLI has no asset and no release job of its own. It ships inside
  each platform's install package, and the installer puts it on the
  `PATH`.
- Vapor has no automatic update and no update check for now. An update
  is the install of the newer file, which updates the app, the CLI, and
  the daemon together (owner, 2026-11-21).

## 8) Engineering standards

- Every change
  - Make the smallest complete change. No speculative abstraction, no
    dead code, no unrelated cleanup, and no placeholder other than the
    platform stubs that §9.7 allows. An abstraction needs a second use
    today or a boundary to enforce.
  - Never hide an error, weaken a type, loosen an assertion, or add a
    suppression to make a check pass.
  - Precedent is not a defence. Existing code that breaks an invariant
    in §3 to §6 does not license more of it: fix it when the fix is
    small and in the area being changed, otherwise report it and add a
    task under `docs/tasks/`.
  - A finding is never lost. Whoever finds a bug, something broken, or
    an improvement that could be made, and does not fix it in the same
    change, adds it as a task under `docs/tasks/` in that change set
    and names it in the report. The owner decides later what to do
    with each one (owner, 2026-11-21).
- Swift (macOS app only)
  - Prefer structured concurrency and explicit actor boundaries.
  - Keep UI/state surfaces deterministic and reason-first.
  - macOS UI must follow Apple Human Interface Guidelines and platform
    conventions.
  - Visual direction should be minimalist, sleek, and polished; prefer
    native controls, spacing, typography, and motion over heavy custom
    chrome.
  - Swift code is macOS-only by policy. Do not attempt to make Swift code
    cross-platform; reach for Rust in `core/*` when logic needs to be
    shared across surfaces.
- Rust (runtime + CLI + platform layer)
  - Use explicit error enums and classify transient vs permanent failures.
  - Canonicalize paths only through `vapor_shared::paths::canonicalize`
    (clippy's `disallowed-methods` enforces it, tests included). On
    Windows `std::fs::canonicalize` returns verbatim `\\?\` spellings;
    one helper keeps every component and every test on the same
    spelling, so path comparisons never fail on prefix alone.
  - Keep async/task lifetimes bounded and cancellation-aware.
  - Platform-sensitive code lives under `core/platform/<trait>/<os>.rs`
    behind a trait the engine consumes. Do not sprinkle `#[cfg(target_os)]`
    through engine code. `unsafe` is allowed only in `core/platform` FFI,
    one `SAFETY:` comment per block.
  - Native-optimal per OS is encouraged; portable-but-slow is not an
    acceptable final state.
- Future app shells (Windows, Linux)
  - UI frameworks are per-app discretion (see `docs/plans/core.md §8` for
    recommendations). Each app shell is a thin client over `core/*`.
- API/contracts
  - Version IPC payloads; pre-GA breaking changes are allowed with
    coordinated updates.
  - Provider trait changes require capability and behavior review
    (`vapor-provider` skill).
  - Platform-trait changes require a parity review so every supported OS
    either adopts the change or has a tracked task to do so.

## 8.1) Observability and diagnostics

- Contributors may add structured file logging when needed to diagnose reliability or lifecycle issues.
- Logging favours actionable context (state, reason, identifiers) and never contains secrets or tokens.
- Log levels mean something: routine transitions are INFO, WARNING is reserved for conditions a user may need to act on, ERROR for failures. A healthy run should not accumulate warnings.
- Temporary debug-heavy logging should be easy to dial down via log levels and should not violate low-impact goals.

## 8.2) Toolchain and platform version policy

- Target latest stable versions by default for every supported host:
  - macOS runner/image in CI + Xcode and Swift toolchain (macOS app).
  - Ubuntu (`ubuntu-latest`) and Windows (`windows-latest`) runners in CI
    for every `core/*` Rust crate.
  - Rust toolchain and required components on every runner.
- Avoid pinning old versions unless there is a documented blocker.
- If temporary pinning/downgrade is required, document the reason, owner,
  and removal criteria.

## 8.3) Known-good local baseline (reference)

The verified contributor baseline is in
`docs/development/toolchain-baseline.md`. It is informational, it is
updated when the baseline shifts materially, and it does not override
the "latest stable" policy above.

## 8.4) Script-first validation

Contributors run the repository wrapper scripts under `scripts/`, never
raw tool commands, in the order `format`, `lint`, `test`, then `e2e` for
runtime-affecting changes and `vm` for changes to the Mac app or to its
install. The wrappers set the project environment
(`VAPOR_DIR`, `VAPOR_ENV`, log routing). `lint` includes the dependency
check (`cargo deny`), which needs that tool installed and a network
connection. Which tier a change needs and
how to read a red run: the `vapor-validate` skill.

## 8.5) Runtime data directory policy

- Runtime artifacts must live under a single vapor directory root (`vapor_dir`): config, logs, and durable state.
- Configuration file path is `vapor_dir/vapor.json`.
- Logs are written under `vapor_dir/logs/`.
- Durable queue/state DB paths live under `vapor_dir/state/` (`vapor.sqlite` for the implicit profile, `profiles/<id>/vapor.sqlite` per configured profile, `lifecycle.json` for crash-loop state).
- Runtime directory selection is code-defined and env-overridable only; it is not a user-configurable `vapor.json` field.
- Runtime directory resolution order is:
  1. `VAPOR_DIR` environment variable (explicit override)
  2. local dev/test/CI default `./.vapor` when `VAPOR_ENV=dev` (the repository scripts set it; nothing else, a `CI` variable included, moves the directory)
  3. normal runtime default `~/.vapor`
- Repository scripts must default `VAPOR_DIR` to `./.vapor` so local and CI behavior are consistent.
- Repository scripts should default `VAPOR_ENV` to `dev` (and `prod` for package flow).

## 8.6) Shared constants policy

- Runtime/config/environment constants are centralized and treated as source of truth: `core/shared/src/constants.rs` for the portable runtime and every Rust consumer, mirrored for the macOS app in `apps/macos/Sources/VaporCore/VaporConstants.swift`.
- Product version source of truth is the root `VERSION` file; the Cargo workspace version is synced from it via `./scripts/version.sh`.
- No duplicated literals for `VAPOR_*` keys or shared defaults outside the constants modules.
- Every config key is classified as live-reload or restart-required in `constants::config`; the daemon and the CLI read that classification.
- Adding or changing a key, variable, default, path name, or launch label follows the `vapor-config` skill (constants, mirror, call sites, CLI typing, reload, docs, tests).

## 8.7) Localization and user-facing copy policy

- User-facing UI copy source-of-truth catalogs must live under `assets/locales/*.json`.
- Swift workflow scripts must sync locale catalogs into `apps/macos/Sources/VaporCore/Resources/locales/*.json` before build/test/package.
- Logs and internal diagnostics text may remain English-only.
- Language selection behavior must preserve safe fallback order:
  1. user `language` selection (default `en`),
  2. English (`en`) fallback if that catalog is unavailable.
- When adding or changing user-facing UI text, contributors must update `en.json` (and any other available catalogs) in the same change set.
- If a translation key is missing in a non-English catalog, fallback behavior must remain deterministic and resolve to English.

## 8.8) Brand assets policy

- Brand artwork (the app icons for macOS, Windows, and Linux, the menu
  bar mark, README banners, social previews, favicons, the landing-site
  metadata) lives under `assets/`, one directory per surface, with
  `assets/README.md` as the guide. App surfaces consume it through the
  resource sync (`scripts/resources.sh`) and the packaging scripts; no
  surface keeps a private copy, and the generated mirrors under `apps/*`
  stay gitignored.
- Every desktop icon derives from the masters under `assets/source/`
  through the rebuild tool there; a change to the sculpture goes into
  the master and through the tool, never into one platform's export by
  hand. A new raster joins the set with its generation prompt in
  `assets/source/GENERATION-NOTES.md`.

## 8.9) Code comment policy

- Code comments must never reference internal task-list or roadmap
  identifiers: no task ids (`C8-12`, `M2-1`, `L3-7`, …), no wave or phase
  numbers, and no pointers to `docs/tasks/*`. Task tracking lives in
  `docs/tasks/`; a code comment must stand on its own for a reader who has
  never seen the task lists. The same applies to strings that surface to
  users or logs (e.g. `unimplemented!()` messages).
- Prose in architecture, operations, development, and CI docs, and in
  the READMEs next to the code, carries no task id and no wave or
  phase number either. It may point at a task file
  (`docs/tasks/core.md`) for open work (owner, 2026-11-14).
- The same holds for spec decision numbers (`D7`): none in code
  comments, in strings, or in system docs. A code comment does not
  point at `docs/specs/*` either; a system doc may link the spec that
  records why. Both must make sense to a reader who has never seen the
  spec.
- Referencing stable documentation (`docs/architecture/*`, `docs/plans/*`,
  `AGENTS.md` sections) is fine; those documents describe the system, not
  the work schedule.
- Keep comments short and direct. A comment earns its place by stating a
  constraint, invariant, or non-obvious "why" that the code cannot express;
  it should not restate what the code does.
- Do not leave useless comments. Delete comments that narrate history
  ("replaces the old X", "retired with Y"), describe the change instead of
  the code, or restate a default/constant defined elsewhere. If a
  historical fact matters (e.g. "this is the only implementation"), keep
  the fact and drop the archaeology.
- A stale comment is a bug: when a change makes a nearby comment wrong or
  obsolete, update or delete it in the same change set.

## 8.10) Agent knowledge files and skills

Vapor keeps the vendor-neutral filename as the real file and gives each
agent tool its expected name as a symlink, so one source of truth serves
every agent. Layout and the add-a-skill checklist: `.agents/README.md`.

- Contributor rules live in `AGENTS.md`. `CLAUDE.md` is a symlink to it.
  Edit `AGENTS.md` and `.agents/` only; never the symlinks.
- Project skills live in `.agents/skills/<skill-name>/SKILL.md`.
  `.claude/skills/<skill-name>` is a symlink to the matching
  `.agents/skills/<skill-name>` directory, which is how Claude Code
  discovers them. **Adding a skill means adding its mirror symlink in the
  same change set**, otherwise the skill is invisible to Claude Code.
  `.gitignore` keeps the rest of `.claude/` (machine-local agent state)
  untracked while tracking `.claude/skills`.
- Vapor's own skills are named `vapor-<word>`: one word after the
  prefix, naming an entity (`config`, `provider`, `docs`, `e2e`) or an
  action (`validate`, `commit`, `release`, `debug`). A general-purpose
  skill brought in from outside the project (`unslop`) keeps its
  original name.
- `SKILL.md` front matter must stay within the fields every supported
  agent understands: `name` (required, identical to the directory name),
  `description` (required), and optionally `license`, `version`,
  `allowed-tools`, `user-invocable`. Do not add tool-specific keys.
  `license` matches the repository (`GPL-3.0-only`).
- The `description` is the only part an agent reads when deciding whether
  to invoke a skill; the body is loaded afterwards. Write it in the
  third person and state both what the skill does **and** when to use it.
- A skill change is verified as loadable before commit: the symlink
  resolves to a `SKILL.md`, the front matter parses, and the skill shows
  up in the agent's skill list and can be invoked.
- Writing rules for every document, comment, and message: the `unslop`
  skill. It always applies. Which style goes with which reader: §8.11.
- `.context/` is scratch space for agents: notes, raw command output,
  PR bodies, handoffs between sessions. It is gitignored and never
  shipped.

## 8.11) Writing styles

Two styles, chosen by the reader (owner, 2026-11-14). The `unslop`
pattern list applies to both: no em dashes, no AI vocabulary, no
puffery, no chatbot phrases.

**Simplified Technical English** for text the owner or an agent reads
to act on: reports and replies to the owner, design rounds, specs, PR
descriptions, and review findings. A misread costs work, so clarity
beats voice. Rules, adapted from ASD-STE100:

- One idea per sentence. An instruction has 20 words or fewer; a
  description has 25 words or fewer.
- Active voice. Simple present, simple past, or simple future tense.
- One word for one thing: the same term every time, and one meaning
  for each word.
- Common words. A single verb over a phrasal verb ("remove", not "take
  out").
- Keep the articles ("the", "a"). No telegraph style.
- Steps go in a numbered list, one action per step.
- A paragraph has one topic and six sentences or fewer.
- State evidence, uncertainty, and blockers directly: what was checked,
  what was not checked, and what stops the work.

Where these rules and `unslop` disagree (sentence length, rhythm),
these rules win.

**Unslop with voice** for prose a user reads: the root `README.md`.
Follow all of `unslop`, including its advice on rhythm and opinion.
The one emoji per Features bullet that §10 asks for stays.

Commit messages, code comments, `CHANGELOG.md`, the documents under
`docs/`, this file, and the skills follow `unslop` and stay short and
plain. Per-document rules: the `vapor-docs` skill.

## 9) Required test matrix

Testing is a non-negotiable part of every change. Vapor is coded
autonomously, so the coding agent's feedback loop is whatever
`./scripts/test.sh` tells it. The full taxonomy, per-module
expectations, and adding-a-test checklist live in
`docs/architecture/testing-strategy.md`. This section is the contract;
the procedure is the `vapor-validate` skill.

### 9.1) Testing philosophy

- **Cover real behavior, not trivial restatements of code.** A
  thousand trivial tests are worse than a hundred well-chosen ones. A
  failing test must catch a real bug.
- **Fast.** `./scripts/test.sh` (Tier 1) must finish in under 2 minutes
  on a contemporary dev machine and under 5 minutes on CI, where
  `VAPOR_TEST_MAX_SECONDS` enforces it. The script builds first and
  times the run only. If a change pushes the suite past the budget,
  split slow tests out to Tier 2.
- **Deterministic.** No `thread::sleep` for timing-dependent
  assertions; use test-injectable clocks. No retry decorators.
- **Independent.** Tests run in any order and in parallel. Each
  integration test uses its own `tempfile::TempDir`. No shared mutable
  state.
- **No network, no `~/.vapor`.** Never contact the real Internet;
  never touch the user's runtime dir. The macOS keychain contract test
  uses a throwaway service name and cleans up after itself.

### 9.2) What must be tested

Every non-trivial logic change in `core/*` must ship with one or more
of the following:

- **Unit tests** for pure logic (debounce, scheduler, throttle,
  workgate, retry, storm, state_db, reconcile, executor, path_filter,
  event_intents, fs_events, runtime_paths, logging, config reload).
- **Integration tests** composed through the `DaemonRuntime` and
  `MultiProfileRuntime` tick harnesses for multi-module behavior
  (local→remote / remote→local propagation, restart recovery, offline
  edits found by the startup reconcile, throttle transitions during real
  work, storm bursts, self-write-cache suppression, multi-profile
  isolation, config reload mid-work).
- **Property tests** for well-defined invariants where random inputs add
  value (path normalization, scheduler superseding, throttle
  monotonicity, retry backoff monotonicity, conflict-suffix determinism,
  durable-queue FIFO, ignore-rule precedence, IPC handshake skew
  matrix). `proptest` covers path normalization, scheduler superseding,
  and retry backoff (`core/daemon/tests/properties.rs`); explicit cases
  cover the rest until their properties land (`docs/tasks/core.md`).
- **Platform-trait contract tests.** Each trait's test body runs against
  the in-memory fake and the native impl on the shipping OS (the secret
  store and the fs watcher do this today; the other traits are open
  work in `docs/tasks/core.md`). Catches fake-vs-native drift.
- **Bidirectional race tests**: simultaneous local/remote edits,
  rename+modify, delete/restore, loop-prevention verification.
- **Crash / restart / recovery tests**: pre/post-restart durable
  intent count delta = 0 (excluding completed), retry slowdown
  restored, leases recovered.
- **Performance guard-rails** (Tier 1): cheap "someone accidentally
  made the callback 100× slower" checks. Distinct from the SLO perf
  suite (Tier 2).
- **`--json` shape locks** for every `vapor … --json` command: explicit
  assertion tests today; `insta` snapshots are open work
  (`docs/tasks/core.md`).

### 9.3) What must NOT be tested

As important as §9.2. Refuse tests for:

- Trivial getters / setters that return an inner field.
- `Default` impls whose values are constants mirrored 1:1 from the
  struct definition.
- `Debug` / `Display` impls unless the output is a wire format.
- `serde` derive round-trips of trivial structs.
- Generated code from `build.rs`.
- **UI rendering on any app surface**: SwiftUI views, menubar layout,
  Dock transitions, and the equivalent on future Windows/Linux apps.
  No unit test covers them. On macOS, Tier VM (§9.9) proves how the
  installed app behaves, and the project owner judges how it looks
  from the screenshots of a run (owner, 2026-11-21).
- **Interactive TTY behavior** on the `vapor` CLI: color codes, cursor
  positioning, terminal resize, ncurses interactions.
- Third-party crate internals.
- Code that just restates a policy from `constants.rs`.

If a test's failure mode is "I typo'd a default value", skip it.

### 9.4) Per-surface scope

- `core/*` (Rust runtime, CLI, platform layer): heavy testing. No UI,
  so nothing is carved out.
- `apps/macos` (Swift): logic tests only (configuration, lifecycle
  coordinator state transitions, view-model state mapping, localization
  fallback, logger redaction). **No UI tests in Tier 1.** What the
  installed app does on the screen is Tier VM's (§9.9).
- `core/cli` (`vapor` binary): logic + shape-lock + integration tests.
  `vapor service install` / `run` / `status` round-trip in CI. **No
  interactive TTY tests.**
- Future GUI apps (Windows, Linux): same rule: logic yes, UI no.

### 9.5) Test tiers

- **Tier 1**: `./scripts/test.sh` via `test.yml` (and `workflow_call`
  in a release). Runs on every PR; required check on `main`
  (`docs/ci/required-checks.md`). Budget: under 5 minutes per OS on CI.
- **Tier 2**: `scripts/perf.sh` via `perf.yml`, a release gate and on
  demand.
  One bounded soak cell against the release profile with the SLO
  checks asserted on its report; long-running property cases, fuzz
  corpora, and `loom`-backed concurrency tests as they land. Not a PR
  gate.
- **Tier S**: `./scripts/soak.sh` (the `tools/soak` driver), a
  release gate in `soak.yml` and on demand. Hours of seeded file churn on
  both sides of a real daemon, fault injection, and a model-checked
  oracle after every phase (nothing lost, nothing invented, both trees
  converged, one-way reverts honoured). The first violation freezes the
  run with the sandbox intact. Contract in
  `docs/development/soak-testing.md`; procedure in the `vapor-soak`
  skill. Never a PR gate; the workload and the model are never edited
  to make a run green.
- **Tier E2E**: `./scripts/e2e.sh` (the `tools/e2e` harness), on
  every PR in every `test.yml` OS job (macOS adds `--full`, which
  installs a real LaunchAgent and is for disposable runners only;
  scenarios whose needs the host cannot meet skip by name) and locally
  by the contributor (host-safe default). Contract in §9.8; procedure
  in the `vapor-e2e` skill.
- **Tier VM**: `./scripts/vm.sh` (the `tools/vm` harness), run by the
  contributor: required for a change to the Mac app or its install,
  and a leg of `./scripts/release.sh`. The installer of the build,
  installed in a macOS virtual machine, with the app driven as a user
  does. Never a PR gate: a GitHub-hosted runner cannot run a macOS
  guest. Contract in §9.9; procedure in the `vapor-vm` skill.

### 9.6) Flaky-test policy

- A test that fails intermittently on the same input is flaky.
- Flaky tests block merges until fixed or removed. Retry decorators
  are banned.
- A test flagged flaky twice in two weeks is either fixed or removed.
- Removing a flaky test requires an issue describing the invariant
  that is no longer covered.

### 9.7) Platform matrix specifics

- Every trait in `core/platform` must have (a) an in-memory fake used
  by cross-OS unit tests, and (b) a native implementation on every OS
  that currently ships a surface, exercised by the trait's contract
  test in the matching OS-specific CI job (`macos-latest` today).
- For OSes that do not currently ship a surface, a trait may be
  `unimplemented!()` or return neutral defaults on that OS. The stub
  must compile (so `cargo build --workspace` keeps succeeding on every
  OS in the CI matrix) and must be tracked in `docs/tasks/core.md`.
- `vapor service install` + `vapor run` + `vapor status` round-trip
  must pass on every OS that currently ships a surface before that OS
  is considered shipped.

### 9.8) End-to-end verification (Tier E2E)

Unit and integration tests are not the whole feedback loop. An agent
that ships a runtime-affecting change must also watch the real product
work once, end to end. Procedure: the `vapor-e2e` skill and
`docs/development/e2e-verification.md`. The invariants:

- `./scripts/e2e.sh` builds the harness (`tools/e2e`, crate
  `vapor-e2e`, a dev tool that never ships) and the shipping binaries
  (`vapor`, `vapord`), then drives the daemon black-box through the
  CLI only, never the macOS app, asserting via the `--json` commands,
  exit codes, daemon logs, the two trees on disk, and read-only
  durable-DB queries. It runs on the host it is built on; there is no
  container target.
- Every scenario runs in its own sandbox and ends with the tree
  oracle (the local root and the cloud root hold the same files) and
  log hygiene (no ERROR line, no warning it did not declare, no failed
  intent). A scenario opts out of the oracle only with a stated reason.
- Sandbox discipline is absolute: everything runs under the repo-local
  `.vapor/e2e/` directory (removed by `./scripts/clean.sh`). Tier E2E
  must never touch `~/.vapor`, install host services (LaunchAgents,
  login items), launch packaged apps, or use the network. The harness
  points `HOME` at the sandbox for every process it starts, so a
  default path cannot reach the real home.
- Required after Tier 1 passes for: features or fixes in `core/*` that
  change daemon/CLI-observable behavior, startup/shutdown/IPC/schema
  changes, and build changes to the shipping binaries. Not required
  for doc-only, UI-only, or test-only changes.
- When a change adds e2e-observable behavior, add a scenario for it
  under `tools/e2e/src/scenarios/` in the same change set, and run it
  once against the base commit: it must fail before the change and
  pass after. A green run of old scenarios proves non-regression, not
  the new feature.
- A scenario that asserts behavior the product does not have yet is a
  known gap: it stays in the suite marked as such, with the gap named
  in words and a task in `docs/tasks/core.md`. Weakening an assertion
  to make a run green is never acceptable. A known-gap scenario that
  passes makes the run red until its marker is removed.
- UI-affecting changes go through Tier VM (§9.9), which is where an
  agent sees the app.
- The filesystem provider is the default. Google Drive is an opt-in
  mode (`--provider gdrive`) that needs real credentials for a
  dedicated test account: never part of the default run, never in CI,
  never run implicitly by an agent, never against the project owner's
  account. It runs once per release, by hand, as a leg of
  `./scripts/release.sh`, which runs the suite for every provider
  the harness knows before a version bump.

### 9.9) Virtual machine verification (Tier VM)

Tier E2E never starts the Mac app, and the app cannot run on a
contributor's Mac without replacing the launchd jobs and the login item
of a real install. Tier VM tests the installed product on a Mac that is
not the contributor's: a macOS virtual machine. Procedure: the
`vapor-vm` skill and `docs/development/vm-verification.md`. The
invariants:

- `./scripts/vm.sh` builds the installer, the harness (`tools/vm`,
  crate `vapor-vm`) and its driver (`tools/vm/driver`), dev tools that
  never ship. It installs the installer, unchanged, in a macOS guest
  made with Tart from an image pinned by digest, and acts on the app
  through the accessibility API of macOS and the installed `vapor`
  CLI. The app carries no test hook (owner, 2026-11-21).
- Isolation is absolute (owner, 2026-11-21). The packaged app and the
  installer start only inside a guest. A guest sees two host
  directories (the staged build, read-only, and the run's directory),
  has no window, no sound, no shared clipboard, and no route to the
  Internet. A manual guest shows a window only when a person asked to
  use it by hand (`./scripts/vm.sh --guest --window`). The harness changes only VMs named `vapor-vm-*`, and a
  checkout only its own.
- A run deletes every guest it made; only `--keep` leaves one, on
  request. What stays on the host is the evidence under the repo-local
  `.vapor/vm/` (removed by `./scripts/clean.sh`), one empty lock file
  in the user's temporary directory, and in Tart's store the image and
  the prepared guests that every run clones (removed by
  `./scripts/vm.sh --clean` and `tart delete`).
- A run never downloads. `./scripts/vm.sh --setup` is the one command
  that uses the network (the guest image, 36 GB on disk). No script of
  the repository runs Homebrew: the contributor installs Tart by hand
  (owner, 2026-11-21).
- A scenario declares the guest it needs, by what it tests: a fresh
  guest for the install, a first start, a start at login, or a
  removal; the shared guest for the screens and states of the
  installed app (owner, 2026-11-21). The scenarios that share a guest
  do not depend on their order.
- Required before an agent reports as done a change under `apps/macos`
  or a change to how the app is packaged or installed, and as a leg of
  `./scripts/release.sh` (owner, 2026-11-21).
- When a change adds behaviour that a user sees in the app or in the
  install, add a scenario under `tools/vm/src/scenarios/` in the same
  change set: it must fail before the change and pass after. Weakening
  an assertion to make a run green is never acceptable.
- A scenario finds a control in a window by its accessibility
  identifier (`apps/macos/Sources/VaporCore/AccessibilityID.swift`),
  and compares a text with the English catalog. A new control gets its
  identifier in the same change set.
- An agent verifies behaviour and never the look. The report of a UI
  change gives the owner the screenshots of the run and a list of what
  to judge (owner, 2026-11-21).

## 10) Pull requests, commits, and documentation

- Commit shape, message convention, and labels: the `vapor-commit`
  skill.
- The pull request from branch to merge-ready (squash, rebase on
  `main`, push, description template, CI watch, adversarial review,
  report): the `vapor-pr` skill.
- Documentation duties for every change (CHANGELOG line, root README
  Features and Configuration, the owning `docs/` file, group READMEs,
  plans, tasks, and specs, this file when a rule changes): the
  `vapor-docs` skill.

The invariants behind them:

- Do not push commits to GitHub unless the project owner explicitly asks.
  A request to open a PR covers the pushes of that PR's branch (the
  first push, the fix commits, a rebase onto a moved `main`) and
  nothing else. An explicit release request counts as asking for the
  one push the release flow requires; it does not authorize any other
  push.
- Never commit, rebase, or push on `main` outside the release flow.
  The release flow puts one commit there, the release-prep commit,
  with its tag. A fix that the release gate finds goes through a pull
  request like any other change (owner, 2026-11-14). A branch that was
  rewritten on purpose is pushed with `--force-with-lease
  --force-if-includes`, never a bare `--force` (the exact command is in
  the `vapor-pr` skill).
- A branch is one commit when its PR opens, and the PR title is that
  commit's subject. The owner merges, as a squash, the only method the
  repository allows, so that subject is the commit on `main`. Agents
  never merge.
- A PR that changes what a user sees shows it. Its description
  carries the screenshots of the change, from a Tier VM run (§9.9),
  and a video when the change moves on the screen, so the reviewer
  sees what changed. Both are optional, and a picture shows a guest
  only, never a contributor's machine (owner, 2026-11-21).
- Every PR gets one adversarial review by the agent's own subagents
  (owner, 2026-11-14). It starts the moment the PR opens and runs
  while CI runs, so it adds no time to the wait. One pass; the agent
  verifies the fixes itself.
- No external AI reviewer runs on this repository, and none is to be
  added (owner, 2026-11-14). Such a reviewer sees the diff without the
  task or the decisions. If one comments on a PR anyway, it gets no
  answer and no fix. Human comments get an answer.
- Non-trivial feature/logic changes update the required documentation
  in the same change set, and add to `CHANGELOG.md` `Unreleased` before
  the commit: one line for each pull request, in words a user can
  notice (owner, 2026-11-14). The release body is that section, and
  GitHub refuses a long one.
- Root `README.md` is product-facing and concise; detailed content
  belongs under `docs/`. Its **Features** section stays complete and
  honest (`Available now` vs `In flight and coming next`), one emoji
  per bullet, user outcomes over internals, provider-agnostic wording,
  "device" not "laptop", no duplicated section content. Its
  **Configuration** section stays current for every key, default, and
  `VAPOR_*` variable. Shortened README content moves to `docs/`, never
  disappears.
- Every `docs/<group>/` and per-platform subdirectory has a `README.md`
  entrypoint kept current in the same change set as any file added,
  removed, or renamed there. `docs/tasks/README.md` is also the
  cross-surface roadmap orchestrator.
- Non-trivial macOS UI/UX changes document the intended experience and
  its alignment with Apple design conventions; Windows follows Fluent /
  WinUI, Linux follows GNOME HIG or KDE HIG per the chosen toolkit.
- If docs are intentionally not updated, the PR description says why.
- A change that builds a spec sets the spec's status in the same change
  set.

## 10.1) Standing permissions

Collected from the rules in this file, so an agent neither stops to ask
for what is already granted nor acts on what is not.

- Without asking: run the wrapper scripts the validation ladder calls
  for (§8.4), the e2e manual sandbox, and the Tier VM manual guest;
  read repository and PR state
  with `git` and read-only `gh` calls; write scratch files under
  `.context/` and the repo-local `.vapor/`; read `~/.vapor` to
  diagnose a failure the owner reported (`vapor-debug`); and, on a PR
  the owner asked for, push to its branch (the fix commits, a rebase
  onto a moved `main`), answer human comments, and run the adversarial
  review.
- Only when the owner asks for that specific action: push a branch,
  open a PR, cut or rehearse a release, run e2e against Google Drive,
  install or remove Tart (a removal with `HOMEBREW_NO_AUTOREMOVE=1`,
  so Homebrew removes nothing else), pull the guest image
  (`./scripts/vm.sh --setup`), change repository settings, release
  environments, or the action allowlist.
- Never: merge a PR, push to `main` outside the release flow, launch a
  packaged app on the host (inside a Tier VM guest is that tier's job),
  run `./scripts/e2e.sh --full` on a contributor machine,
  write to `~/.vapor` or run a test or a reproduction against it, or
  use the network in a test.

## 11) Definition of done

"Every shipping OS" below means the OSes currently shipping a surface.
macOS is the primary shipping OS today; Windows/Linux only count as
shipping OSes once their optional waves in `docs/tasks/README.md` have
landed.

A change is done when:

- Behavior works in happy and failure paths on every shipping OS.
- No regression of throttle/impact invariants on any shipping OS.
- Crash/restart recovery is preserved on every shipping OS.
- Observability is sufficient to explain current state/reason on every
  shipping OS.
- For changes that touch a `core/platform` trait: the native impl on
  every shipping OS passes its CI job; on non-shipping OSes the trait
  may be `unimplemented!()` but must compile and must be tracked in
  `docs/tasks/core.md`.
- Docs and contracts are updated (common docs at the top level,
  platform-specific docs under the matching `docs/<group>/<platform>/`).
- The validation ladder is green and quoted in the report
  (`vapor-validate`).

## 12) Incident playbooks (minimum)

Maintain runbooks under `docs/operations/` for:

- daemon crash loops
- auth/token refresh failures
- provider rate-limit storms
- a state database that was set aside (another schema version, damage)
- reconcile backlog non-convergence

Each runbook must include detection, mitigation, user-visible state, and recovery verification.

The runbooks that do not exist yet are open work in
`docs/tasks/core.md`.

## 13) Skills index

Real files under `.agents/skills/<name>/SKILL.md`, mirrored at
`.claude/skills/<name>`. Invoke the one that matches the job; each
carries its own trigger conditions in its description.

| Skill | Invoke when |
|---|---|
| `unslop` | Writing anything a human reads: docs, comments, commit messages, replies. Always. |
| `vapor-design` | The owner asks for a feature, a change to sync semantics, or any change with more than one reasonable answer; before building one; when a change would contradict a recorded decision. |
| `vapor-validate` | Before committing a change under `core/*`, `apps/*`, or `scripts/*`; when a script run is red. |
| `vapor-e2e` | A change alters daemon- or CLI-observable behaviour and Tier 1 is green; to watch a feature in the real product. |
| `vapor-vm` | A change touches the Mac app or its install; to see the installed app or reproduce a bug in it; to get screenshots of a UI change for the owner. |
| `vapor-soak` | Proving the product is safe for real data: hours of seeded churn with faults and a no-loss oracle, watched on a loop and triaged at the first violation. |
| `vapor-debug` | The daemon crashed, will not start, sync is stuck, or a status looks wrong; a bug or a failing test has no obvious cause. |
| `vapor-config` | Adding or changing a `vapor.json` key, `VAPOR_*` variable, default, path name, or launch label. |
| `vapor-provider` | Touching `core/providers` or adding a provider kind. |
| `vapor-docs` | Any non-trivial change; any file added, removed, or renamed under `docs/`. |
| `vapor-commit` | Creating commits. |
| `vapor-pr` | Opening, updating, or rebasing a pull request; watching its CI and conflicts; the adversarial review; answering a review comment; judging merge readiness. |
| `vapor-release` | The owner asks to cut or rehearse a release, or `release.yml` changes. |
