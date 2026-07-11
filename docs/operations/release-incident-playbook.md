# Release Incident Playbook

## Purpose

Provide minimum detection, mitigation, and recovery verification steps for release pipeline failures.

## Incident severity

- `SEV-2`: stable release blocked (signing/notarization/publish failure).
- `SEV-3`: prerelease failure or draft asset inconsistency.

## Common failure scenarios

## 1) Missing signing/notarization prerequisites on stable tag

Detection:

- Workflow fails during prerequisite gate or package notarization step.
- Logs indicate missing `VAPOR_SIGN_IDENTITY`, `VAPOR_NOTARY_PROFILE`, or Apple certificate/notary secrets in the `release-macos` environment.

Mitigation:

- Verify secrets exist in the GitHub `release-macos` environment and match the expected certificate/API key inputs.
- Re-run workflow for same tag after secret correction.

Recovery verification:

- Successful run produces `Vapor.zip` and checksum file.
- Release remains draft until maintainer approval.

## 2) Notary service outage or timeout

Detection:

- `xcrun notarytool submit --wait` exits non-zero or times out.

Mitigation:

- Confirm Apple service status and credential validity.
- Retry workflow after backoff.
- Keep release in draft/unpublished state.

Recovery verification:

- Notarization step passes.
- `stapler` succeeds and the uploaded `Vapor.zip` was rebuilt from the stapled app bundle.

## 3) Partial asset upload or mismatched asset set

Detection:

- Release exists but required artifacts are missing.
- Asset names differ from expected set.

Mitigation:

- Re-run workflow for same tag; upload uses `--clobber` replacement semantics.
- If needed, manually remove stale assets and rerun.

Recovery verification:

- Release contains complete expected assets.
- Checksums match uploaded binaries.

## 4) Accidental tag push

Detection:

- Unexpected release workflow triggers from unapproved tag.

Mitigation:

- Immediately set release to draft/unpublished.
- Coordinate with release owner before deleting tag/release.
- Apply tag protection/ruleset corrections.

Recovery verification:

- No public stable release remains for accidental tag.
- Corrective tag policy is documented in follow-up.

## 5) Version/changelog mismatch

Detection:

- Workflow fails changelog version gate.

Mitigation:

- Update `CHANGELOG.md` entry to match tag.
- Push corrective commit and create new tag; do not rewrite existing release tag.

Recovery verification:

- Release notes section aligns with tagged version.

## Incident command checklist

- [ ] Assign incident owner.
- [ ] Record failing run URL and tag.
- [ ] Capture root cause category (secrets, tooling, notarization, asset handling, policy).
- [ ] Apply mitigation and rerun deterministically.
- [ ] Verify artifact integrity and release metadata.
- [ ] Document follow-up hardening actions.
