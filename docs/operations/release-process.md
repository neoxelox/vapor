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
- GitHub Environment `release` exists and is configured for release jobs.
- For stable releases:
  - `VAPOR_SIGN_IDENTITY` configured in the `release` environment secrets.
  - `VAPOR_NOTARY_PROFILE` configured in the `release` environment secrets.
  - `APPLE_DEVELOPER_ID_P12_BASE64` configured in the `release` environment secrets.
  - `APPLE_DEVELOPER_ID_P12_PASSWORD` configured in the `release` environment secrets.
  - `APPLE_KEYCHAIN_PASSWORD` configured in the `release` environment secrets.
  - `APPLE_NOTARY_API_KEY_P8_BASE64` configured in the `release` environment secrets.
  - `APPLE_NOTARY_KEY_ID` configured in the `release` environment secrets.
  - `APPLE_NOTARY_ISSUER_ID` configured in the `release` environment secrets when using an App Store Connect Team key; omit it for Individual keys.
- Optional:
  - `VAPOR_ENTITLEMENTS` path override when needed.

## GitHub release environment setup

- Create a GitHub Actions environment named `release` before the first tagged release.
- Grant the environment required reviewers if you want a human approval gate before signing/notarization starts.
- Move Apple signing and notarization secrets into that environment instead of leaving them as repository-wide secrets.
- Keep workflow permissions least-privilege:
  - `contents: read` for preflight, lint, test, and perf
  - `contents: write` only for the release publish job
- Keep `actions/checkout` on `persist-credentials: false`; authenticated git fetches in release preflight must pass `GITHUB_TOKEN` explicitly.

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
     - Stable example: `./scripts/version.sh set 0.2.0`
     - Prerelease example: `./scripts/version.sh set 0.2.0-rc.1`
     - Increment current prerelease example: `./scripts/version.sh prerelease rc`
    - Update `CHANGELOG.md`:
      - Move items from `Unreleased` into a new `## [<VERSION>] - YYYY-MM-DD` section.
      - Keep sections concise and user-impact focused.
    - Ensure docs and CI references are current.

2. Validate before tagging
   - Run required checks in order:
     - `./scripts/format.sh`
     - `./scripts/lint.sh`
     - `./scripts/test.sh`
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
     - Calls reusable `lint.yml`, `test.yml`, and `perf.yml` in parallel.
     - Verifies the tag ref is valid, exactly matches `VERSION`, and the tag commit is reachable from `origin/main`.
     - The `release` job declares `needs: [preflight, lint, test, perf]`, so packaging does not begin unless ref validation and all three gates pass.
     - Imports the Developer ID certificate into a temporary keychain and creates the `notarytool` profile on-runner when signing/notarization is configured.
     - Verifies the imported keychain actually contains `VAPOR_SIGN_IDENTITY` before packaging begins.
     - Passes the temporary keychain path into packaging so `notarytool submit` resolves the stored profile explicitly.
     - Runs `./scripts/build.sh package` on `macos-latest`.
      - Validates artifact structure and changelog/version alignment.
      - Generates SHA-256 checksums.
      - Creates/updates GitHub Release and uploads assets with deterministic replacement (`--clobber`).

6. Publish policy
   - Stable releases are created as drafts for operator verification before publishing.
   - Prerelease tags are marked prerelease in GitHub Release metadata.

7. Post-run verification
     - Confirm release assets include:
       - `Vapor.zip`
       - `Checksums.txt`
    - Confirm package contents include:
      - `Vapor.app/Contents/MacOS/Vapor`
      - `Vapor.app/Contents/MacOS/vapord`
    - Confirm release notes match `CHANGELOG.md` section for the tag.

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
- [ ] Validation scripts passed (`format`, `lint`, `test`).
- [ ] `./scripts/version.sh ...` created commit `release: v$(cat VERSION)` and tag `v$(cat VERSION)`.
- [ ] Release push command used: `git push origin "$(git branch --show-current)" --follow-tags`.
- [ ] Release gates passed (`lint`, `test`, `perf` reusable workflows / local script equivalents).
- [ ] Release workflow succeeded.
- [ ] Stable release zip was built from the signed, stapled app bundle.
- [ ] Checksums present and verified.
- [ ] Draft reviewed and published by maintainer.
