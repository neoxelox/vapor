# Install and first launch

What a user has to know between the download and the first sync. The
root `README.md` keeps the table of files and the short steps; the
detail is here.

## One file for each device

A release holds one install file for each platform and architecture,
and `Checksums.txt`. The file name says which device it is for:
`Vapor-<os>-<arch>.<ext>`.

| Device | File | Status |
|---|---|---|
| Mac with Apple silicon, macOS 26 or later | `Vapor-macos-arm64.pkg` | available as a prerelease |
| Windows, x64 and ARM64 | `Vapor-windows-x64.msi`, `Vapor-windows-arm64.msi` | planned |
| Linux, Debian and Ubuntu | `Vapor-linux-x64.deb`, `Vapor-linux-arm64.deb` | planned |
| Linux, Fedora and RHEL | `Vapor-linux-x64.rpm`, `Vapor-linux-arm64.rpm` | planned |

One install gives the app, the `vapor` command on the `PATH`, and the
`vapord` daemon that syncs in the background. There is nothing else to
install: Vapor ships no separate CLI download. There is no Intel Mac
build.

Releases up to `0.2.0-alpha.3` hold `Vapor.zip` in place of the
installer: unzip it, move Vapor to Applications, and find the CLI at
`Vapor.app/Contents/Helpers/vapor`.

## Install on macOS

1. Download `Vapor-macos-arm64.pkg` from the latest
   [GitHub release](https://github.com/neoxelox/vapor/releases).
2. Open it and follow the installer. It asks for an administrator
   password, because it writes to `/Applications` and to
   `/usr/local/bin`.
3. Run `vapor init` in a terminal (see "The first run" below).

From a terminal the install is one command:

```sh
sudo installer -pkg Vapor-macos-arm64.pkg -target /
```

What the installer leaves on the Mac:

- `Vapor.app` in `/Applications`, with the app, `vapord`, and the CLI
  inside it.
- A link at `/usr/local/bin/vapor` to the CLI inside the app. That
  folder is on the `PATH` of every new terminal. If another program
  named `vapor` is there already, the installer leaves it alone, and the
  CLI is at `/Applications/Vapor.app/Contents/Helpers/vapor`.

**Prereleases are not signed with a Developer ID yet**, so macOS stops
the installer the first time. Allow it under System Settings > Privacy
& Security > Open Anyway. The command above installs it with no such
stop. If that is not a trade you want to make, build from source
(`docs/development/runbook.md`).

## Update

Install the newer file over the old one. That updates the app, the
CLI, and the daemon together. Within a minute the supervisor notices
the new build, restarts itself, and replaces a daemon of the old build
with the new one. Quit and open the app again to get the new app
window. Vapor has no update check yet: a new release is on the
[releases page](https://github.com/neoxelox/vapor/releases).

## Remove

1. `vapor service uninstall` stops the daemon and removes the two
   background jobs.
2. Move `Vapor.app` to the Trash.
3. `sudo rm /usr/local/bin/vapor` removes the link.

The runtime directory `~/.vapor` (settings, logs, sync state, trash)
and your synced folders stay until you delete them.

## The first run

Vapor syncs nothing, and creates no folder, until you say what to
sync. `vapor init` is the one command for it. On a terminal it asks,
in this order:

1. Which cloud (`filesystem`, `gdrive`).
2. The sign-in, when the cloud has an account: Google Drive opens the
   browser.
3. The folder on this device (suggestion: `~/Vapor`).
4. The folder in the cloud (suggestion: `/Vapor`). Vapor looks in the
   account and says whether the folder exists or is new.
5. The sync mode (`two-way` by default). A one-way mode asks its
   data-loss question here.
6. Whether Vapor starts at login and keeps running in the background
   (yes by default).

Then it prints a summary with the state of the daemon. The app's own
setup screen, with the same steps, is on the way.

`vapor init` is safe to run again. Each step looks at what is true on
the device, so a later run does only what is left: a sign-in that
failed, a service that is not registered. On an install that is set up
it changes nothing and says "Vapor is initialized".

### With no terminal

A script, or a server session with no terminal, gives every answer as
a flag, and nothing is asked:

```sh
vapor init --provider filesystem --local ~/Vapor --cloud /mnt/backup/Vapor
```

- `--local` and `--cloud` are required: a folder that nobody chose is
  no choice.
- `--token -` reads a sign-in token from stdin, for a host with no
  browser: `<command that prints the token> | vapor init --provider
  gdrive --local ~/Vapor --cloud /Vapor --token -`. A token is never
  taken from the command line.
- `--auto-launch off` records that Vapor does not start at login. The
  daemon then runs by hand, with `vapor run`, which is what a container
  does.
- No flag answers the question of a one-way mode. The profile waits
  until `vapor decisions resolve` answers it.
- The exit code is 0 when Vapor is initialized, and 1 when a step
  failed or waits for a person. `--json` prints each step with its
  state.

## Good to know

- **macOS asks before any app reads Documents, Desktop, Downloads,
  iCloud Drive, or a removable drive.** If your sync folder is in one
  of them, say yes when it asks. `vapor doctor` tells you when a folder
  is out of reach.
- **A crashed daemon comes back on its own**, with the app open or
  closed. Every install registers a small supervisor next to the
  daemon for it (`docs/operations/macos/launchagent-policy.md`).
- **On a Linux server Vapor keeps running after you log out.** On a
  host with no desktop, the install of the background service turns
  lingering on for your account. `--linger off` leaves it alone.

## Google Drive

- Sign in during `vapor init`, later with `vapor auth login`, or with
  Sign In in the app.
- Google Docs, Sheets, Slides, and shortcuts stay in the cloud: they
  are not files Vapor can download
  (`docs/architecture/provider-onboarding.md`).
- A prerelease build asks you to sign in again after each update,
  because macOS identifies an unsigned build by its content
  (`docs/operations/provider-auth-operations.md`).
- A sign-in made by `vapor init` stays when you stop before the
  folders, so the next run continues there. `vapor auth logout`
  removes it.
