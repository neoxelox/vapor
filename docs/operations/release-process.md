# Release Process

## Scope and goals

This runbook defines the source-of-truth release flow for Vapor macOS artifacts published on GitHub Releases.

Release invariants:

- Packaging remains script-first via `./scripts/build.sh package` and `apps/macos/scripts/package.sh`.
- Root `VERSION` is the single source-of-truth for Vapor app + daemon semantic versioning.
- `dist/Vapor.app` is the local packaged bundle used for validation and zipping.
- GitHub Releases publish file assets, so the distributable release payload is `Vapor.zip` containing `Vapor.app`.
- Packaged app metadata uses Apple-valid bundle version keys, while raw semver/prerelease value and git commit SHA are stored in dedicated Vapor build-info fields.
- Stable releases (`vX.Y.Z`) require signing and notarization and fail hard if prerequisites are unavailable.
- Tag history is immutable for released versions.

## Ownership

- Primary owner: project owner (release engineering).
- Backup owner: designated maintainer for the release cycle.

## Versioning and tag policy (pre-GA)

- `VERSION` stores the exact release version, including prerelease suffixes when applicable.
- Tag prefix is mandatory: `v`.
- Stable tag grammar: `vX.Y.Z`, where `X.Y.Z` must equal `VERSION`.
- Supported prerelease grammar:
  - `vX.Y.Z-rc.N`
  - `vX.Y.Z-beta.N`
  - `vX.Y.Z-alpha.N`
  - `N` must stay within `1...255` so the app bundle build version remains Apple-valid.
- Prerelease tags must exactly match `v$(cat VERSION)`.
- Pre-GA SemVer interpretation:
  - `X` may increment for breaking internal/runtime/config changes.
  - `Y` increments for additive behavior changes.
  - `Z` increments for fixes and small reliability updates.
- Tags are immutable once pushed and used for release publication.
- If a bad tag is pushed, do not retag in place; publish a corrective follow-up tag.

## Required release inputs

- `CHANGELOG.md` updated with the target version section.
- `VERSION` updated to the exact stable or prerelease version being released.
- `Cargo.toml` synced from `VERSION` via `./scripts/version.sh`.
- `Cargo.lock` refreshed after the version change so workspace package versions stay aligned.
- Run release preparation from `main`.
- Before invoking `./scripts/version.sh`, the worktree must be clean except for `CHANGELOG.md`.
- The platform's GitHub Environment (`release-macos`; `release-windows` /
  `release-linux` when those surfaces ship) exists, and its protections were
  applied **before** its secrets were added — see "GitHub release environment
  setup" below.
- Signing follows the tag, on every platform: a stable tag needs the
  platform's full signing material and refuses to package without it; a
  prerelease tag (`-alpha`, `-beta`, `-rc`) needs none and ships unsigned
  when none is configured (signed when it is). The lists below are what a
  stable tag requires per platform.
- For stable releases on macOS:
  - `VAPOR_SIGN_IDENTITY` configured in the `release-macos` environment secrets.
  - `VAPOR_NOTARY_PROFILE` configured in the `release-macos` environment secrets.
  - `APPLE_DEVELOPER_ID_P12_BASE64` configured in the `release-macos` environment secrets.
  - `APPLE_DEVELOPER_ID_P12_PASSWORD` configured in the `release-macos` environment secrets.
  - `APPLE_KEYCHAIN_PASSWORD` configured in the `release-macos` environment secrets.
  - `APPLE_NOTARY_API_KEY_P8_BASE64` configured in the `release-macos` environment secrets.
  - `APPLE_NOTARY_KEY_ID` configured in the `release-macos` environment secrets.
  - `APPLE_NOTARY_ISSUER_ID` configured in the `release-macos` environment secrets when using an App Store Connect Team key; omit it for Individual keys.
- For stable releases on Windows (when `apps/windows` ships): the EV
  code-signing material named by the Windows trust chain doc, in the
  `release-windows` environment secrets.
- For stable releases on Linux (when `apps/linux` ships): the GPG signing
  key material named by the Linux trust chain doc, in the `release-linux`
  environment secrets.
- Optional:
  - `VAPOR_ENTITLEMENTS` path override when needed.

## GitHub release environment setup

Every shipping platform gets its own environment holding only its release
secrets — `release-macos` today, `release-windows` and `release-linux` when
those surfaces ship. The steps below apply to **each** of them. Rule:
`AGENTS.md` §7.1.

A newly created environment has **zero** protection rules and GitHub does
not warn you about it, so protecting it is an explicit setup step. Do it
**before** adding that platform's secrets, never after: there must be no
window in which signing material sits in an unguarded environment. Nothing
is inherited from an already-configured platform.

- Move each platform's signing and notarization secrets into its own environment instead of leaving them as repository-wide secrets.
- Keep workflow permissions least-privilege:
  - `contents: read` for preflight, lint, test, perf, soak, and package
  - `contents: write` only for the `publish` job
- Release preflight relies on the default authenticated checkout credentials for `git fetch origin main`.

### Required protections

1. **Restrict which refs may deploy.** Use a custom deployment policy with a
   single **tag** rule, `v*`, and **no branch rule** — so only a release tag
   can reach the signing secrets, and no branch can.

   This is enforced by repository settings on purpose. The workflow already
   restricts itself (tag-only trigger, preflight asserting the tag matches
   `VERSION` and descends from `origin/main`), but that YAML is part of the
   ref being released and is editable by anyone with write access. The
   environment policy is not.

2. **Require a reviewer**, so producing a signed artifact is a deliberate
   act rather than an automatic consequence of pushing a tag.

3. **Leave `can_admins_bypass` at `true`** only while a single maintainer
   holds admin — the gate is on the same person who would bypass it, and it
   preserves an escape hatch during a release incident. Set it to `false`
   as soon as a second admin exists.

A wait timer is not used; a reviewer gate is strictly better.

### Applying them

Replace `release-macos` with the platform environment being set up, and
`<reviewer-user-id>` with the numeric id from `gh api user -q .id`:

```sh
gh api -X PUT repos/neoxelox/vapor/environments/release-macos --input - <<'JSON'
{
  "wait_timer": 0,
  "prevent_self_review": false,
  "reviewers": [{"type": "User", "id": <reviewer-user-id>}],
  "deployment_branch_policy": {
    "protected_branches": false,
    "custom_branch_policies": true
  }
}
JSON

gh api -X POST repos/neoxelox/vapor/environments/release-macos/deployment-branch-policies \
  -f name='v*' -f type=tag
```

Keep `prevent_self_review` at `false` while one maintainer holds admin,
otherwise no one can ever approve a release and the environment deadlocks.

Verify with:

```sh
gh api repos/neoxelox/vapor/environments/release-macos
gh api repos/neoxelox/vapor/environments/release-macos/deployment-branch-policies
```

### Current status

- `release-macos` — fully protected: `v*` tag-only deployment policy,
  `neoxelox` as required reviewer, `prevent_self_review: false`, no wait
  timer, `can_admins_bypass: true`. Configured before any Apple secret was
  added; none is configured yet.
- `release-windows` / `release-linux` — created 2026-09-16 with the
  identical protection (`v*` tag rule, `neoxelox` as required reviewer,
  `prevent_self_review: false`, no wait timer, `can_admins_bypass: true`),
  so the gate is in place before either platform's secrets exist. No
  secret is configured in either; the platform's signing material goes
  in when its surface ships, together with its `package` matrix entry.

Note for whoever sets up the next platform environment: the required-reviewer
and wait-timer rules are free only on public repositories. On a private
repository they need a paid plan and the API rejects them with
`422 … billing plan supports the required reviewers protection rule`. The tag
policy has no such restriction, so on a private repository apply the tag rule
immediately and treat the reviewer gate as blocked rather than optional.

With the reviewer gate active, a release no longer runs straight through:
pushing the tag runs preflight, lint, test, perf, and soak, then **pauses** for
approval in the Actions UI before the platform's `package` job starts and
signing material is imported. A release that looks stuck at that point is waiting on
a human, not broken. This has not been exercised yet — the environment was
created after the last release ran, so the next tagged release is the first
one it gates.

## Apple secret preparation

- Export a `Developer ID Application` certificate plus private key from Keychain Access as a `.p12` file.
- Convert the `.p12` file to base64 for `APPLE_DEVELOPER_ID_P12_BASE64`:
  - `base64 -i vapor-developer-id.p12 | pbcopy`
- Store the `.p12` export password as `APPLE_DEVELOPER_ID_P12_PASSWORD`.
- Create a strong temporary-keychain password for CI as `APPLE_KEYCHAIN_PASSWORD`.
- Set `VAPOR_SIGN_IDENTITY` to the exact `Developer ID Application: ...` identity string shown by Keychain Access or `security find-identity -v -p codesigning`.
- Download the App Store Connect API key `.p8` file and convert it to base64 for `APPLE_NOTARY_API_KEY_P8_BASE64`:
  - `base64 -i AuthKey_<KEYID>.p8 | pbcopy`
- Store the App Store Connect key identifier as `APPLE_NOTARY_KEY_ID`.
- Store the issuer UUID as `APPLE_NOTARY_ISSUER_ID` only when the API key belongs to a Team. Individual keys must leave it unset.
- Choose a memorable `VAPOR_NOTARY_PROFILE` name; the workflow stores this profile inside a temporary keychain and passes that keychain path into the packaging step.

## End-to-end flow

1. Prepare release changes
   - Commit all non-release changes on `main`.
   - Set the target version with `./scripts/version.sh`:
     - Show current version: `./scripts/version.sh current`
     - Set an exact stable version: `./scripts/version.sh set 0.2.0`
     - Bump the stable base version: `./scripts/version.sh bump patch|minor|major`
     - Set an exact prerelease: `./scripts/version.sh set 0.2.0-rc.1`
     - Bump the current prerelease: `./scripts/version.sh prerelease rc|beta|alpha`
     - Convert the current prerelease to its stable release: `./scripts/version.sh release`
    - Update `CHANGELOG.md`:
      - Move items from `Unreleased` into a new `## [<VERSION>] - YYYY-MM-DD` section.
      - Keep sections concise and user-impact focused.
    - Ensure docs and CI references are current.

2. Validate before tagging
   - Run the release gate: `./scripts/release.sh`. It runs
     `format`, `lint`, `test`, and the e2e suite once per provider the
     harness knows (`./scripts/e2e.sh --providers`). The Google Drive
     leg runs only here, never in CI: it needs the dedicated test
     account signed in (`vapor auth login gdrive`) and
     `VAPOR_GDRIVE_CLIENT_ID` in the environment, and a missing leg
     fails the gate.
   - Commit any non-`CHANGELOG.md` fixes produced by validation.
   - Confirm only `CHANGELOG.md` remains dirty before release preparation.
   - Optionally run local package rehearsal: `./scripts/build.sh package`.

3. Run release preparation
   - Run the appropriate `./scripts/version.sh ...` command.
   - The script validates the clean-worktree rule and matching `CHANGELOG.md` entry.
   - The script updates `VERSION`, syncs `Cargo.toml`, refreshes `Cargo.lock`, creates commit `release: v$(cat VERSION)`, creates tag `v$(cat VERSION)`, and prints the push command.

4. Push release commit and tag
   - `git push origin "$(git branch --show-current)" --follow-tags`

5. Workflow execution (`.github/workflows/release.yml`)
     - Triggered on `push.tags: ["v*"]`.
     - Calls reusable `lint.yml`, `test.yml`, `perf.yml`, and `soak.yml` in parallel; the soak matrix takes about an hour.
     - Preflight verifies the tag ref is valid, exactly matches `VERSION`, the tag commit is reachable from `origin/main`, and `CHANGELOG.md` holds the tag's section.
     - `package (<platform>)` runs once per shipping platform (`macos` on `macos-latest` under the `release-macos` environment today; Windows and Linux entries join the matrix when those surfaces ship, each under its own environment) and declares `needs: [preflight, lint, test, perf, soak]`, so packaging does not begin unless ref validation and all four gates pass.
     - The macOS entry imports the Developer ID certificate into a temporary keychain and creates the `notarytool` profile on-runner when signing/notarization is configured, verifies the imported keychain actually contains `VAPOR_SIGN_IDENTITY` before packaging begins, and passes the temporary keychain path into packaging so `notarytool submit` resolves the stored profile explicitly.
     - Every entry runs `./scripts/build.sh package`, validates its own artifact structure, and uploads its assets as the `release-<platform>` workflow artifact.
     - `publish` (on `ubuntu-latest`, the only job with `contents: write`) downloads every platform's assets into `dist/`, writes one `Checksums.txt` with a SHA-256 line per asset, extracts the release notes from the changelog, and creates/updates the GitHub Release, uploading assets with deterministic replacement (`--clobber`).

6. Publish policy
   - Stable releases are created as drafts for operator verification before publishing.
   - Prerelease tags are marked prerelease in GitHub Release metadata.

7. Post-run verification
     - Confirm release assets include:
       - `Vapor.zip`
       - `Checksums.txt` (one `sha256sum` line per asset, verifiable with `sha256sum -c` next to the downloaded files)
    - Confirm package contents include:
      - `Vapor.app/Contents/MacOS/Vapor`
      - `Vapor.app/Contents/MacOS/vapord`
      - `Vapor.app/Contents/Helpers/vapor`
    - Confirm release notes match `CHANGELOG.md` section for the tag.

## Adding a platform to the package matrix

`release.yml` packages through one `package (<platform>)` matrix entry
per shipping platform and one shared `publish` job. Windows and Linux
join the same way macOS is in today; nothing in `preflight`, the four
gates, or `publish` changes. The environment for each already exists
and is protected (see "Current status" above), so the order is: secrets
into the environment, then the matrix entry, then the trust chain doc.

1. **Matrix entry.** Add to `jobs.package.strategy.matrix.include`:
   `platform` (`windows` or `linux`; it names the artifact
   `release-<platform>` and gates the platform's steps), `os` (the
   GitHub-hosted runner), `environment` (`release-windows` or
   `release-linux`), and `assets` (the paths under `dist/` the packaging
   writes, one per line). `fail-fast` stays off so one platform's
   failure does not cancel another's.
2. **Signing check, first.** Mirror the macOS "Validate signing and
   notarization configuration" step, gated on `matrix.platform`: read
   every secret the platform needs, set `enabled` to whether any is
   present, fail when some but not all are present, and fail when
   `RELEASE_STABLE` is `true` and none is. That step is what makes a
   stable tag refuse to ship unsigned and lets a prerelease tag through
   without material. Do not gate on the tag anywhere else.
3. **Install the material**, gated on the platform and on `enabled`:
   the EV certificate into the runner's certificate store on Windows
   (Azure Key Vault or a USB HSM per `AGENTS.md` §7.3), the GPG key into
   a throwaway keyring on Linux. Export whatever the packaging script
   needs through `GITHUB_ENV`, as the macOS step does with the keychain
   path.
4. **Build** with `./scripts/build.sh package`, the same step for every
   platform. Teach `scripts/<stack>/build.sh` to sign when the material
   is present (`signtool` on Windows, a detached GPG signature next to
   the AppImage on Linux) and to write the assets under `dist/`.
5. **Validate outputs**, gated on the platform: the equivalent of the
   macOS `test -x` lines for the installer or AppImage and, when
   `enabled`, that the signature verifies.
6. **Upload** is the shared step: `matrix.assets` goes up as
   `release-<platform>`. `publish` picks it up, adds it to
   `Checksums.txt`, and attaches it to the release without any change.
7. **Clean up** the material, gated on the platform and `enabled`,
   with `if: always()`, as the macOS step does with its keychain.
8. **Docs in the same change set.** The platform's trust chain doc under
   `docs/operations/<platform>/`, the "For stable releases" list above,
   the release checklist, `docs/ci/overview.md`, and the incident
   playbook's platform section.

The first stable tag on a new platform is the first time its signing
path runs in anger; rehearse it with a prerelease tag after the secrets
are in, since a prerelease signs when the material is present.

## Deterministic rerun policy

- Reruns are allowed for the same tag.
- Rerun the existing tag workflow from GitHub Actions; do not create a manual release run from a branch ref.
- Asset uploads use replacement semantics (`gh release upload --clobber`).
- Existing release object is reused and edited in place when present.
- If rerun still fails, follow `docs/operations/release-incident-playbook.md`.

## Rollback and corrective release policy

- Do not move or rewrite release tags.
- If a release is broken:
  - Keep the failing draft for audit context.
  - Create a corrective commit and publish a new tag (for example `v0.2.1`).
- If an accidental prerelease tag was pushed:
  - Delete the GitHub Release draft if unused.
  - Remove tag from remote only if no consumers rely on it and incident owner approves.

## Release checklist

- [ ] Changelog entry exists for target version.
- [ ] Worktree is clean except for `CHANGELOG.md` before running `./scripts/version.sh`.
- [ ] `./scripts/release.sh` passed (`format`, `lint`, `test`, e2e for every provider).
- [ ] `./scripts/version.sh ...` created commit `release: v$(cat VERSION)` and tag `v$(cat VERSION)`.
- [ ] Release push command used: `git push origin "$(git branch --show-current)" --follow-tags`.
- [ ] Release gates passed (`lint`, `test`, `perf`, `soak` reusable workflows / local script equivalents).
- [ ] Deployment to the platform release environment approved in the Actions
      UI (the run pauses there after the gates pass, before signing).
- [ ] Release workflow succeeded.
- [ ] Stable release zip was built from the signed, stapled app bundle.
- [ ] Checksums present and verified.
- [ ] Draft reviewed and published by maintainer.
