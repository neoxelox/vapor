---
name: vapor-release
description: Prepares and cuts a Vapor release with scripts/version.sh and the tag-driven GitHub workflow, including the preconditions (clean main, CHANGELOG), the one permitted push, the protected-environment and action-allowlist checks, and post-run verification. Use only when the project owner asks to cut, prepare, or rehearse a release, or when editing release.yml or the release docs.
license: GPL-3.0-only
---

# Cut a release

Runbook: `docs/operations/release-process.md`. Trust-chain policy:
`docs/operations/macos/distribution-trust-chain.md`. Incident handling:
`docs/operations/release-incident-playbook.md`. This skill is the short
form; the runbook wins on any disagreement.

## Preconditions

1. On `main`, up to date with `origin/main`, and clean except
   `CHANGELOG.md` (`scripts/version.sh` refuses otherwise).
2. `CHANGELOG.md` holds a `## [<version>] - YYYY-MM-DD` section with
   the lines of the release. A person moves them there from
   `Unreleased`; the script only checks that the section exists and
   stops with "CHANGELOG.md missing section" when it does not.
3. `./scripts/release.sh` green on the commit being released: the
   format check, lint, Tier 1, the e2e suite once per provider, and
   the Tier VM suite. Tier VM is the last leg and needs Tart and the
   guest image on this Mac: check `./scripts/vm.sh --list` and
   `tart list` before the gate starts (`vapor-vm`). The
   Google Drive leg runs only here, by hand, with the dedicated test
   account and `VAPOR_GDRIVE_CLIENT_ID` set (exported or in `.env`); it
   never runs in CI and never against a personal account.
   A red gate stops the release: the fix goes through a pull request
   (`vapor-pr`), and the release starts again from the merged `main`.
4. The target version sorts above the current one. `version.sh` refuses
   anything else, `prerelease` on a stable version (use `set
   <version>-rc.1`), and `bump` on a prerelease (use `release` or
   `set`).
5. The release environment is protected before any secret is added:
   one `v*` tag rule and no branch rule, a required reviewer, and
   `can_admins_bypass` set to `false` once a second admin exists. New
   environments are created unprotected and nothing warns you. Check
   with the `gh api` commands in the runbook.
6. A stable tag needs `VAPOR_GDRIVE_CLIENT_ID` and
   `VAPOR_GDRIVE_CLIENT_SECRET` in the platform's release environment:
   the package job compiles them into the binaries and refuses a stable
   tag without them. A prerelease without them ships without Google
   Drive sign-in.
6. Every third-party action a workflow references is on the repository
   allowlist and SHA-pinned; add the pattern before the workflow
   references it, including actions nested in a composite action.

## Version source of truth

- `VERSION` at the repo root; `Cargo.toml` `[workspace.package]` and
  every member's lockfile entry are synced from it by the script.
- The tag must equal `v$(cat VERSION)`.
- `scripts/version.sh` is the only entrypoint:
  `current`, `check-sync`, `sync`, `set <version>`,
  `bump major|minor|patch`, `prerelease alpha|beta|rc [n]`, `release`.
  The `set`/`bump`/`prerelease`/`release` forms create the release-prep
  commit and the tag.

## The one push

An explicit release request from the owner authorises exactly one push:
`git push origin main --follow-tags` for the release-prep commit and its
tag. Nothing else. The tag triggers `release.yml`: preflight (tag
format, `VERSION` match, ancestry on `main`, the changelog section),
then `lint`, `test`, `perf`, and `soak` as reusable workflows, then one
`package (<platform>)` job per shipping platform, each of which pauses
for its environment's required reviewer before the signing certificate
is imported, then `publish`, which assembles the draft release from
every platform's assets. The soak is the long leg: nine cells of 45
minutes side by side on macOS and Linux runners, so the gates take
about an hour. A paused run is not a broken run. Signing follows the
tag on every platform: a stable tag refuses to package without the
platform's signing secrets, a prerelease tag ships unsigned when none
is configured. On macOS the material is two identities, set together:
`VAPOR_SIGN_IDENTITY` for the app and `VAPOR_INSTALLER_SIGN_IDENTITY`
for the installer. Adding a platform to the package matrix follows the
recipe in `docs/operations/release-process.md`.

## Post-run verification

- Release assets: `Vapor-macos-arm64.pkg`, `Checksums.txt`.
- The installer holds `Applications/Vapor.app` with
  `Contents/MacOS/Vapor`, `Contents/MacOS/vapord`, and
  `Contents/Helpers/vapor`, and the `postinstall` script that links
  `/usr/local/bin/vapor`
  (`apps/macos/scripts/verify-package.sh <file>` checks it; the package
  job also installs it on its runner).
- Release notes match the `CHANGELOG.md` section for the tag.
- Never open the packaged app on the host. Tier VM starts it in a
  guest, and the owner judges the look.

## Reruns and failures

A rerun for the same tag is allowed while the release is a draft: it
rewrites the notes and replaces the assets. A rerun on a published
release fails before it changes anything, because users hold its files
and its checksums; a published release is corrected with a new tag.
Anything else goes through the incident playbook.
