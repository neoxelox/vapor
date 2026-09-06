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
2. `CHANGELOG.md` `Unreleased` holds every line for the release; the
   script rolls them into the versioned section.
3. Tier 1 and e2e green on the commit being released.
4. The release environment is protected before any secret is added:
   one `v*` tag rule and no branch rule, a required reviewer, and
   `can_admins_bypass` set to `false` once a second admin exists. New
   environments are created unprotected and nothing warns you. Check
   with the `gh api` commands in the runbook.
5. Every third-party action a workflow references is on the repository
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
format, `VERSION` match, ancestry on `main`), then `lint`, `test`, and
`perf` as reusable workflows, then the `release` job, which pauses for
the environment's required reviewer before the signing certificate is
imported. A paused run is not a broken run.

## Post-run verification

- Release assets: `Vapor.zip`, `Checksums.txt`.
- Package contents: `Vapor.app/Contents/MacOS/Vapor`,
  `Vapor.app/Contents/MacOS/vapord`, `Vapor.app/Contents/Helpers/vapor`.
- Release notes match the `CHANGELOG.md` section for the tag.
- Never open the packaged app; the owner verifies launch by hand.

## Reruns and failures

Reruns for the same tag are allowed and idempotent (a published release
is never demoted back to draft). Anything else goes through the incident
playbook.
