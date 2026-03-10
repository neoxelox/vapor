# Release Process

## Scope and goals

This runbook defines the source-of-truth release flow for Vapor macOS artifacts published on GitHub Releases.

Release invariants:

- Packaging remains script-first via `./scripts/build.sh package` and `apps/macos/scripts/package.sh`.
- `dist/Vapor.app` is the local packaged bundle used for validation and zipping.
- GitHub Releases publish file assets, so the distributable release payload is `Vapor.zip` containing `Vapor.app`.
- Stable releases (`vX.Y.Z`) require signing and notarization and fail hard if prerequisites are unavailable.
- Tag history is immutable for released versions.

## Ownership

- Primary owner: project owner (release engineering).
- Backup owner: designated maintainer for the release cycle.

## Versioning and tag policy (pre-GA)

- Tag prefix is mandatory: `v`.
- Stable tag grammar: `vX.Y.Z`.
- Supported prerelease grammar:
  - `vX.Y.Z-rc.N`
  - `vX.Y.Z-beta.N`
  - `vX.Y.Z-alpha.N`
- Pre-GA SemVer interpretation:
  - `X` may increment for breaking internal/runtime/config changes.
  - `Y` increments for additive behavior changes.
  - `Z` increments for fixes and small reliability updates.
- Tags are immutable once pushed and used for release publication.
- If a bad tag is pushed, do not retag in place; publish a corrective follow-up tag.

## Required release inputs

- `CHANGELOG.md` updated with the target version section.
- Clean release commit on `main`.
- For stable releases:
  - `VAPOR_SIGN_IDENTITY` configured in GitHub Actions secrets.
  - `VAPOR_NOTARY_PROFILE` configured in GitHub Actions secrets.
  - `APPLE_DEVELOPER_ID_P12_BASE64` configured in GitHub Actions secrets.
  - `APPLE_DEVELOPER_ID_P12_PASSWORD` configured in GitHub Actions secrets.
  - `APPLE_KEYCHAIN_PASSWORD` configured in GitHub Actions secrets.
  - `APPLE_NOTARY_API_KEY_P8_BASE64` configured in GitHub Actions secrets.
  - `APPLE_NOTARY_KEY_ID` configured in GitHub Actions secrets.
  - `APPLE_NOTARY_ISSUER_ID` configured in GitHub Actions secrets.
- Optional:
  - `VAPOR_ENTITLEMENTS` path override when needed.
  - `VAPOR_BUILD_NUMBER` override for explicit build number control.

## End-to-end flow

1. Prepare release changes
   - Update `CHANGELOG.md`:
     - Move items from `Unreleased` into a new `## [X.Y.Z] - YYYY-MM-DD` section.
     - Keep sections concise and user-impact focused.
   - Ensure docs and CI references are current.

2. Validate before tagging
    - Run required checks in order:
      - `./scripts/format.sh`
      - `./scripts/lint.sh`
      - `./scripts/test.sh`
   - Optionally run local package rehearsal: `./scripts/build.sh package`.

3. Create annotated tag
   - Stable example:
     - `git tag -a v0.2.0 -m "release: v0.2.0"`
   - Prerelease example:
     - `git tag -a v0.2.0-rc.1 -m "release: v0.2.0-rc.1"`
   - Push tag:
     - `git push origin v0.2.0`

4. Workflow execution (`.github/workflows/release.yml`)
     - Triggered on `push.tags: ["v*"]`.
     - Calls reusable `lint.yml`, `test.yml`, and `perf.yml` in parallel.
     - Verifies the tag ref is valid and the tag commit is reachable from `origin/main`.
     - The `release` job declares `needs: [preflight, lint, test, perf]`, so packaging does not begin unless ref validation and all three gates pass.
     - Imports the Developer ID certificate into a temporary keychain and creates the `notarytool` profile on-runner when signing/notarization is configured.
     - Runs `./scripts/build.sh package` on `macos-latest`.
     - Validates artifact structure and changelog/version alignment.
      - Generates SHA-256 checksums.
      - Creates/updates GitHub Release and uploads assets with deterministic replacement (`--clobber`).

5. Publish policy
   - Stable releases are created as drafts for operator verification before publishing.
   - Prerelease tags are marked prerelease in GitHub Release metadata.

6. Post-run verification
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
- [ ] Validation scripts passed (`format`, `lint`, `test`).
- [ ] Release gates passed (`lint`, `test`, `perf` reusable workflows / local script equivalents).
- [ ] Annotated tag created with approved version grammar.
- [ ] Release workflow succeeded.
- [ ] Stable release zip was built from the signed, stapled app bundle.
- [ ] Checksums present and verified.
- [ ] Draft reviewed and published by maintainer.
