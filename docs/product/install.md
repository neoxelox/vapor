# Install and first launch

What a user has to know between the download and the first sync. The
root `README.md` keeps the one-line install steps; the detail is here.

## Requirements

- **macOS**: macOS 26 or later on Apple silicon. There is no Intel Mac
  build.
- **Windows and Linux**: planned. The CLI would ship there before the
  apps do.

## Install on macOS

1. Download `Vapor.zip` from the latest
   [GitHub release](https://github.com/neoxelox/vapor/releases).
2. Unzip it and move `Vapor` into Applications.
3. Optional: link the CLI onto your `PATH`. It ships inside the app, at
   `Vapor.app/Contents/Helpers/vapor`.

## Before the first launch

- **Prereleases are not signed with a Developer ID yet**, so macOS
  refuses to open the app the first time. Allow it under System
  Settings > Privacy & Security > Open Anyway. If that is not a trade
  you want to make, build from source
  (`docs/development/runbook.md`).
- **Vapor syncs nothing, and creates no folder, until you say what to
  sync.** The app's own setup screen is on the way. Today the terminal
  does it: `vapor profiles add` asks which cloud, which folder on this
  device, and which folder in the cloud.
- **macOS asks before any app reads Documents, Desktop, Downloads,
  iCloud Drive, or a removable drive.** If your sync folder is in one
  of them, say yes when it asks. `vapor doctor` tells you when a folder
  is out of reach.

## Google Drive

- Sign in with `vapor auth login`, or with Sign In in the app.
- Google Docs, Sheets, Slides, and shortcuts stay in the cloud: they
  are not files Vapor can download
  (`docs/architecture/provider-onboarding.md`).
- A prerelease build asks you to sign in again after each update,
  because macOS identifies an unsigned build by its content
  (`docs/operations/provider-auth-operations.md`).
