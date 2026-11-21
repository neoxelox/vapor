# One install package for each platform

Status: agreed

## Goal

The project owner asked what a release holds once Vapor ships on
macOS, Windows, and Linux, and then set the rules for it. In the
owner's words: "users will just install an app for their system and
everything will come inside and everything will be taken care of for
them", and "i dont want users to have to install 3 separate things or
to update 3 separate things".

What a user gets: one file to download, named after the platform and
the architecture of the device. The file installs the app, the `vapor`
CLI, and the `vapord` daemon, and it puts `vapor` on the `PATH`. A
newer file updates all three. On Linux the same file installs on a
server with no desktop.

## Facts

What was true when the design started (`main` at `9e9d533`, version
`0.2.0-alpha.3`):

- A release holds `Vapor.zip` and `Checksums.txt`. The zip holds
  `Vapor.app` with `Contents/MacOS/Vapor`, `Contents/MacOS/vapord`,
  and `Contents/Helpers/vapor` (`.github/workflows/release.yml`,
  `docs/operations/release-process.md`).
- `AGENTS.md` §7.5 and the tasks L6-1 to L6-6 of `docs/tasks/cli.md`
  planned a standalone CLI archive for five target triples. L6-6
  planned `Checksums.txt.asc`.
- `docs/plans/core.md` §6 planned an AppImage first on Linux, and
  "WiX or MSIX" on Windows.
- The `publish` job of `release.yml` writes `Checksums.txt` and holds
  no secret. The GPG key of Linux is planned for the `release-linux`
  environment. So the job that writes the file cannot sign it.
- A zip has no install step. An AppImage has none either, and it
  mounts at a new temporary path at each start, so a link into it
  does not last.
- On macOS `/usr/local/bin` is the first entry of `/etc/paths`, and
  `root` owns it. System Integrity Protection refuses a write to
  `/usr/bin`. Checked on the design machine.
- The CLI searches for `vapord` next to the path of the running
  program (`core/cli/src/commands/daemon_binary.rs`). A probe program
  built for this design, started through a symbolic link on macOS,
  printed the path of the link. So through a link the search starts
  in the wrong directory. The real `vapor` was not run through a
  link.
- `pkgbuild` with no signing identity writes a package with no
  signature, and Gatekeeper rejects it with "no usable signature".
  Measured with a probe package. The install of that package was not
  tested, and neither was an unsigned `.msi`, `.deb`, or `.rpm`.
- With no signing identity, `apps/macos/scripts/package.sh` signs
  the three executables ad hoc, which Apple silicon needs to run
  them.
- The daemon and the CLI run on Linux with no desktop, and Tier 1 and
  the e2e suite prove it on the Linux runner. The service is a
  systemd user unit that stops at logout unless the user enables
  lingering (`core/platform/src/service/linux.rs`). A host with no
  browser signs in with a token on stdin
  (`docs/operations/provider-auth-operations.md`).
- No task plans an update check or an automatic update.

## Decisions

Each decision below is the owner's (owner, 2026-11-21).

### Round 1, 2026-11-21: what a release holds

| # | Decision |
|---|---|
| D1 | Vapor ships no standalone CLI and no standalone runtime, on any platform. A platform ships install packages, and each one holds the app, `vapor`, and `vapord`. This replaces `AGENTS.md` §7.5 and the tasks L6-1 to L6-6. |
| D2 | A signature is inside the file that it signs. A release has no detached signature file and no `Checksums.txt.asc`. `Checksums.txt` stays. |
| D3 | An asset is named `Vapor-<os>-<arch>.<ext>`, so the name says which device it is for. `<os>` is `macos`, `windows`, or `linux`. `<arch>` is `arm64` or `x64`. Every build is 64-bit. `Vapor.zip` goes. |

### Round 2, 2026-11-21: what an install does

| # | Decision |
|---|---|
| D4 | The installer puts `vapor` on the `PATH` at install time, on every platform. It is not left to the app's first launch. |
| D5 | One install updates the app, the CLI, and the daemon together, on every platform. A user never installs or updates them as separate things. |
| D6 | The Linux package installs on a host with no desktop, and the CLI and the daemon work there. So Linux ships a `.deb` and an `.rpm` for each architecture, and no AppImage. |

### Round 3, 2026-11-21: macOS, order, and updates

| # | Decision |
|---|---|
| D7 | macOS ships a `.pkg` in place of the zip. It installs `Vapor.app` and links `/usr/local/bin/vapor` to the CLI inside the app. |
| D8 | Signing still follows the tag, for an installer as for the app. A prerelease tag with no signing material ships unsigned installers, which the owner installs past the warning of the system. A stable tag refuses to package without the material. |
| D9 | The Linux package may ship before the Linux app window exists. It then holds `vapor` and `vapord`. The window joins the same package later, under the same file name. |
| D10 | Vapor has no automatic update and no update check for now. An update is the install of the newer file. |

## Design

The assets of one release, when every platform ships:

| Platform | Asset |
|---|---|
| macOS, Apple silicon | `Vapor-macos-arm64.pkg` |
| Windows | `Vapor-windows-arm64.msi`, `Vapor-windows-x64.msi` |
| Linux, Debian family | `Vapor-linux-arm64.deb`, `Vapor-linux-x64.deb` |
| Linux, Red Hat family | `Vapor-linux-arm64.rpm`, `Vapor-linux-x64.rpm` |
| All | `Checksums.txt` |

What the decisions ask of the build:

- macOS: `package.sh` builds the `.pkg` from the signed app. A stable
  tag signs it with a Developer ID Installer certificate, a second
  certificate in `release-macos`, then notarizes and staples it.
- The CLI resolves a link to its real path before it searches for
  `vapord`. The macOS link of D7 needs this.
- After an update the app, the CLI, and the daemon run the same
  version. The build gave this to the supervisor job and not to the
  installer: the supervisor ends itself when its binary is replaced,
  the service manager starts the new one, and that one replaces a
  daemon of the old build. An installer then needs no script that
  reaches into a user's session, and the same holds for every user of
  the device and for every package format.
- Linux: `vapor` goes to `/usr/bin`. The desktop libraries are
  optional dependencies of the package, so it installs with no
  desktop. The GPG signature is inside the package.
- Windows: `Vapor.exe` and `vapor.exe` cannot share a directory,
  because the filesystem ignores case. The CLI gets its own
  directory, and that directory goes on the `PATH`.
- `release.yml`, the release runbook, the trust chain documents, the
  install page, the README, the CLI plan, the task lists, and
  `AGENTS.md` §1 and §7 change to match.

## Verification

The package job of a platform checks at least the asset names of D3,
the three executables inside the package, and `vapor` on the `PATH`
after an install on a clean runner.

Built on 2026-11-21, for macOS (D1 to D5, D7, D8, D10):

- `apps/macos/scripts/package.sh` writes `Vapor-macos-arm64.pkg`, and
  `verify-package.sh` checks the three executables, the link script,
  the refusal of an Intel Mac, and that the bundle is not relocatable.
  Run on the design machine, unsigned.
- `scripts/tests/installer.sh` runs the link script against a
  directory that stands in for the disk: a fresh install, an update, a
  dead link, and another program with the same name.
- The `build` workflow installs the unsigned installer on its macOS
  runner on every pull request and checks the app, the link, and that
  the CLI finds the daemon through the link.
- Not verified: the install by hand on a Mac, which is the owner's,
  and the signed and notarized path, which waits for the two
  certificates.

D6 and D9, and the Windows installer, are open work:
`docs/tasks/core.md` C7-10 and C6-11.

## Open points

1. Windows: `.msi` or `.msix`. The names above use `.msi`. The choice
   follows the UI toolkit of the Windows app.
2. How the installer of each platform restarts the daemon after an
   update. The build's choice, on macOS and Linux: the supervisor job
   does it (see Design). Windows has no supervisor yet.
3. Where the Linux package puts `vapord`: next to `vapor` in
   `/usr/bin`, or in a private directory.
4. A Linux distribution with neither `.deb` nor `.rpm` has no
   package. It builds from source for now.
5. A `.pkg` has no uninstaller. When the user moves `Vapor.app` to
   the Trash, the link of D7 stays and points at nothing.
6. An apt and a dnf repository, and an update notice in the app, are
   later features (D10).
