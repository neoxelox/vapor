---
title: "feat: GitHub release system for Vapor"
type: feat
status: active
date: 2026-03-08
---

# feat: GitHub release system for Vapor

## Overview

Create a script-first, policy-compliant GitHub release system for `Vapor` that adds a root `CHANGELOG.md`, defines version/tag governance, and introduces a GitHub Actions release workflow that builds, packages, signs/notarizes (when configured), and publishes artifacts to GitHub Releases.

This plan aligns with current repository constraints: packaging source-of-truth remains `apps/macos/scripts/package.sh`, release packaging remains CI-runnable, and distributable artifacts remain `Vapor.app` (with both `Vapor` and `vapord`) plus zip assets.

## Problem Statement

The repository currently has CI for lint and tests, but no standardized release automation, no `CHANGELOG.md`, and no established tag history. This creates risk in repeatability, release trust, and operator overhead.

Without a formal release system, version metadata can drift, release artifacts can be inconsistent, and signing/notarization guarantees can be applied inconsistently.

## Scope and Non-Goals

### In scope

- Add root `CHANGELOG.md` with a documented update process.
- Define pre-GA SemVer and tag policy for `v*` releases.
- Add `.github/workflows/release.yml` for tag-driven build/package/release.
- Publish release assets to GitHub Releases using maintained tooling.
- Enforce release gates for signing/notarization on official `v*` releases.
- Add release verification checklist and rollback playbook documentation.

### Non-goals

- Replacing `apps/macos/scripts/package.sh` with a new packaging pipeline.
- Changing product sync/runtime behavior.
- Implementing a full auto-version bump bot in this first iteration.
- Shipping additional providers or non-macOS release artifacts.

## Proposed Solution

Adopt a tag-driven release flow:

1. Maintainer prepares release commit (including `CHANGELOG.md` update).
2. Maintainer creates immutable annotated tag (`vX.Y.Z` or prerelease tag).
3. Tag push triggers release workflow on `macos-latest`.
4. Workflow runs script-first packaging (`./scripts/build.sh package`).
5. Workflow validates artifacts, signatures, and notarization outcomes.
6. Workflow creates/updates GitHub Release and uploads release assets.

For initial safety, use draft-first publishing for stable tags until 2-3 successful cycles validate reliability; then optionally switch to direct publish.

## Technical Approach

### Architecture

- **Source of truth:** `apps/macos/scripts/package.sh` remains canonical for app bundle assembly/signing/notarization.
- **CI orchestration:** new GitHub workflow handles trigger/gating/publish only.
- **Versioning contract:** tags are authoritative for release version (`CFBundleShortVersionString` already derives from latest tag).
- **Release notes contract:** `CHANGELOG.md` is canonical human narrative; generated GitHub notes are supplemental.

### Implementation Phases

#### Phase 1: Governance and docs foundation

- Create `CHANGELOG.md` at repo root using Keep a Changelog structure.
- Add versioning policy doc (SemVer pre-GA interpretation, stable vs prerelease tags, tag immutability, hotfix rules).
- Define release ownership and release checklist in `docs/operations/`.
- Add contribution guidance for changelog entry requirements per release PR.

Deliverables:

- `CHANGELOG.md`
- `docs/operations/release-process.md` (new)
- `docs/operations/release-incident-playbook.md` (new)

#### Phase 2: GitHub Actions release workflow

- Add `.github/workflows/release.yml`:
  - trigger: `on.push.tags: ["v*"]`
  - optional manual re-run: `workflow_dispatch`
  - explicit least-privilege permissions (`contents: write` for release job only)
  - concurrency by ref to avoid duplicate parallel releases
- Reuse existing toolchain/action patterns from lint/test workflows.
- Run `./scripts/build.sh package` as the packaging step.
- If release tag is official stable `vX.Y.Z`, require signing and notarization inputs and fail hard if unavailable/failed.
- Upload artifacts (`dist/Vapor.zip`, optional `dist/Vapor-notarized.zip`) and checksums.
- Create release via `gh release create` / `gh release upload` (avoid archived release actions).

Deliverables:

- `.github/workflows/release.yml`
- Optional `.github/release.yml` for generated notes categories

#### Phase 3: Verification and operational hardening

- Add CI validation checks:
  - tag version matches changelog header
  - artifact contains both binaries in `Vapor.app/Contents/MacOS/`
  - deterministic rerun behavior for same tag
- Document rollback and recovery:
  - failed notarization
  - partial asset upload
  - accidental tag push
- Run first dry-run with prerelease tag and one stable release rehearsal.

Deliverables:

- release verification checklist
- rerun/rollback procedures

## Alternative Approaches Considered

### A) Release-on-main without tags

Rejected: conflicts with existing bundle version derivation from git tags and weakens reproducibility.

### B) Use archived GitHub release actions (`actions/create-release`, `actions/upload-release-asset`)

Rejected: archived/unmaintained, higher long-term breakage risk.

### C) Full auto-versioning bot in first iteration

Rejected for now: adds policy complexity before baseline release reliability is proven.

## System-Wide Impact

### Interaction Graph

`push tag vX.Y.Z` -> GitHub Actions `release.yml` -> `./scripts/build.sh package` -> `apps/macos/scripts/package.sh` -> builds `Vapor` + `vapord` -> assembles/signs/notarizes `dist/Vapor.app` -> zips artifacts -> `gh release create/upload` publishes GitHub Release assets.

### Error and Failure Propagation

- Packaging failures in `scripts/build.sh` or `package.sh` must fail the job before release creation.
- Notarization failures must block stable release publish.
- Asset upload failure must mark workflow failed and preserve diagnostics.
- Retry path should be deterministic and documented (replace assets or fail with clear operator action).

### State Lifecycle Risks

- Partial releases (release created without complete artifacts) are the primary lifecycle risk.
- Mitigation: draft-first flow, upload assets before publish, and explicit rerun policy.

### API Surface Parity

- Root docs (`README.md` and operations docs) must stay aligned with release behavior.
- Scripts and workflow must use the same env vars (`VAPOR_SIGN_IDENTITY`, `VAPOR_NOTARY_PROFILE`, `VAPOR_BUILD_NUMBER`).

### Integration Test Scenarios

- Stable release happy path on signed/notarized flow.
- Missing secrets on stable tag fails hard.
- Notary submit failure blocks publish.
- Duplicate rerun on same tag remains idempotent.
- First release bootstrap from no-tag history behaves as documented.

## Acceptance Criteria

### Functional requirements

- [ ] Root `CHANGELOG.md` exists and includes an initial unreleased section plus first release format.
- [ ] Release process is documented with exact maintainer steps and tag conventions.
- [ ] Tag push matching `v*` triggers the release workflow.
- [ ] Workflow builds and packages via `./scripts/build.sh package` only.
- [ ] GitHub Release is created/updated with release notes and attached artifacts.
- [ ] Stable release assets include checksum file(s).

### Security and trust requirements

- [ ] Workflow permissions are least-privilege; release publishing uses only required scopes.
- [ ] Stable `vX.Y.Z` releases fail if signing/notarization prerequisites are missing or fail.
- [ ] No deprecated notarization tooling (`altool`) is introduced.
- [ ] Release flow preserves trust-chain requirements in `docs/operations/distribution-trust-chain.md`.

### Reliability requirements

- [ ] Workflow concurrency prevents duplicate parallel release runs for the same ref.
- [ ] Rerun behavior for existing tags/assets is deterministic and documented.
- [ ] Release incident playbook covers failure detection, mitigation, and recovery verification.

### Documentation requirements

- [ ] `README.md` release/build documentation reflects new release flow.
- [ ] New/updated operations docs describe ownership, cadence, and rollback procedure.

## Success Metrics

- First 3 tagged releases complete without manual artifact surgery.
- 100% of stable releases include signed/notarized distributable artifact and checksum.
- Median release operator time from tag push to published release decreases by at least 50% vs manual baseline.
- Zero stable releases published with missing `vapord` inside app bundle.

## Dependencies and Prerequisites

- GitHub repository settings:
  - release tag ruleset for `v*`
  - required reviewers on production release environment (if used)
- Apple credentials and certificates available to CI via secure secrets.
- Maintainer agreement on pre-GA versioning policy and changelog ownership.

## Risk Analysis and Mitigation

- **Unauthorized/accidental tags** -> protect `v*` tags with rulesets and restricted creators.
- **Notarization outages/timeouts** -> fail stable publish; document retry/backoff path.
- **Action/runtime drift** -> pin/maintain third-party actions; avoid archived actions.
- **Version/changelog drift** -> CI gate compares tag version with latest changelog entry.
- **Partial upload state** -> use draft-first, explicit asset replacement policy, and post-release verification checklist.

## Resource Requirements

- One maintainer for release policy and docs.
- One engineer for workflow implementation and CI secret/environment setup.
- One rehearsal release cycle (prerelease + stable) before full rollout.

## Future Considerations

- Automatic version bump helper based on conventional commit parsing.
- Signed SBOM/provenance attestations attached to releases.
- Promotion workflow from prerelease to stable with approval gates.

## Documentation Plan

- Update `README.md` build/package/release sections.
- Add release process and incident playbook under `docs/operations/`.
- If workflow behavior changes, update `docs/ci/required-checks.md` as needed.

## Open Questions (to resolve before implementation lock)

1. Should stable `v*` runs always fail hard if signing/notarization is unavailable, with no fallback artifact publication?
2. Should stable tags auto-publish, or remain draft until explicit maintainer confirmation?
3. What exact prerelease grammar is accepted (`vX.Y.Z-rc.N`, `vX.Y.Z-beta.N`)?
4. For reruns, should assets be replaced (`--clobber`) or require manual cleanup first?

## Sources and References

### Internal references

- Packaging source-of-truth: `apps/macos/scripts/package.sh:51`, `apps/macos/scripts/package.sh:74`, `apps/macos/scripts/package.sh:118`, `apps/macos/scripts/package.sh:189`
- Script-first build wrapper: `scripts/build.sh:5`, `scripts/build.sh:8`, `scripts/build.sh:20`
- Existing CI patterns: `.github/workflows/lint.yml:1`, `.github/workflows/test.yml:1`
- Distribution trust-chain policy: `docs/operations/distribution-trust-chain.md:14`
- Contributor release constraints: `AGENTS.md:99`
- Packaging and env documentation: `README.md:80`, `README.md:85`, `README.md:87`, `README.md:103`

### Institutional learnings

- No matching `docs/solutions/` entries currently exist for release automation/tagging/notarization.

### External references (2025-2026)

- GitHub workflow syntax, permissions, tags, concurrency: https://docs.github.com/en/actions/writing-workflows/workflow-syntax-for-github-actions
- GitHub concurrency controls: https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/control-workflow-concurrency
- GitHub events (`push` tags, `release`): https://docs.github.com/en/actions/reference/events-that-trigger-workflows
- GitHub secrets and security hardening: https://docs.github.com/en/actions/security-for-github-actions/security-guides/using-secrets-in-github-actions
- GitHub release management: https://docs.github.com/en/repositories/releasing-projects-on-github/managing-releases-in-a-repository
- GitHub generated release notes: https://docs.github.com/en/repositories/releasing-projects-on-github/automatically-generated-release-notes
- GitHub CLI release commands: https://cli.github.com/manual/gh_release_create
- Apple notarization guidance: https://developer.apple.com/documentation/security/notarizing_macos_software_before_distribution
- Apple notice (`altool` sunset): https://developer.apple.com/news/upcoming-requirements/?id=11012023a
- Keep a Changelog: https://keepachangelog.com/en/1.1.0/
- Conventional Commits: https://www.conventionalcommits.org/en/v1.0.0/
- Semantic Versioning: https://semver.org/

## Suggested Implementation Task List (for `/ce:work`)

- [ ] `CHANGELOG.md`: create and seed sections for Unreleased and first tagged release.
- [ ] `docs/operations/release-process.md`: document release prep, tag creation, publish, and verification.
- [ ] `docs/operations/release-incident-playbook.md`: add detection/mitigation/recovery steps.
- [ ] `.github/workflows/release.yml`: implement tag-triggered package + release pipeline.
- [ ] `.github/release.yml`: optional release-note categories.
- [ ] `README.md`: update release/build instructions and environment expectations.
