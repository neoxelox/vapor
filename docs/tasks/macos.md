# Vapor macOS task list

Plan reference: `docs/plans/macos.md`
Core tasks (runtime, engine, platform layer): `docs/tasks/core.md`
CLI tasks: `docs/tasks/cli.md`

Status legend:

- `[ ]` pending
- `[~]` in progress
- `[x]` complete

Scope of this list: macOS app surface work only; SwiftUI shell, menubar UX,
macOS-native lifecycle UI, and macOS distribution trust chain. Runtime,
engine, and platform work is in `docs/tasks/core.md`.

This file holds open work only. A task that is done leaves the file:
`CHANGELOG.md` and the git history record what landed. Task ids are
stable and are never reused.

## Diagnostics UX (IPC consumer)

What already ships: the health tick reads `vapor status --json` and
maps run state, throttle state, queue depth, and failed count onto the
surface state with the daemon's reason; `Pause`, `Resume`, and `Sync
now` drive the CLI; the menu bar mark turns the brand colour while a
decision waits; the sign-in notice has a Sign In button.

- [ ] M3-2 The full menubar state model: the per-state copy and
      controls beyond what the health tick shows today. The run state
      `unconfigured` shows as paused with the daemon's reason until the
      first-run flow exists (O-2).
- [ ] M3-3 Implement diagnostics panel (throttle reason, queue depth,
      conflicts, failures, effective ceilings, utilization, the
      measured capacity of the link in each direction, idle-boost
      reason).
- [ ] M3-4 Implement per-intent "why stuck" diagnostics UI per
      `docs/architecture/ipc-contracts.md` diagnostics section (filterable
      list + per-row drill-down; surface `droppedEvents` and
      `suppressedEchoes`).
- [ ] M3-5 Implement the live timeline tab (bounded buffer; default 1000
      events; non-persistent across relaunch; say so when the answer
      was `truncated`).
- [ ] M3-6 Tests for timeline ordering, truncation at max length,
      IPC field-omission.
- [ ] M3-8 Conflict notification: native user notification on
      `conflict` timeline events, opening the M3-9 pane. Timeline events
      are the trigger, never the ledger: the pane always lists from
      `vapor decisions list`, so the timeline's cap can never hide a
      conflict. The event's message names the file, and says how many
      more when one tick made several.
- [ ] M3-9 Decisions pane: every question waiting on the user, driving
      `vapor decisions list --json` / `show` / `resolve --json`
      (`docs/architecture/data-flow.md` §Decisions): one row per open
      decision with its plain-language question and its short option
      list as buttons, a Reveal in Finder affordance for a `conflict`
      row (its `evidence` names the original and the copy), and a native
      notification on `decision` timeline events. Kinds today:
      `conflict`, `mass-delete`, `root-missing`, `root-replaced`,
      `type-mismatch`, `unsyncable-name`, `one-way-mode`. No matching
      or resolution logic in Swift; the CLI and the daemon are the
      single engine (`docs/architecture/conflict-resolution.md`).
- [ ] M3-10 Trash pane: what Vapor removed on this device, driving
      `vapor trash list --json` / `restore --json` / `empty --yes
      --json`, with the reason and the original path per row and a
      Restore action; the `trash` settings (`enabled`,
      `retentionDays`, `useSystem`) in Settings.
- [ ] M3-11 Root state: a profile held on a missing or replaced sync
      root (`run.reason` naming the decision) shows as its own state
      with the decision's options, not as a generic error.
- [ ] M3-13 Sign-in notification: a native user notification when a
      profile starts needing a sign-in (the `sign-in` timeline event),
      opening the app at the sign-in notice. Shares the notification
      plumbing with M3-8 and M3-9. A prerelease build asks for a
      sign-in again after each update, because the keychain item names
      the binaries that may read it
      (`docs/operations/provider-auth-operations.md`).
- [ ] M3-14 Ignored paths: show the count that `vapor status --json`
      gives for each profile (`ignoredPaths`) and the list from `vapor
      ignored --json`, with the rule that hides each path.
- [ ] M3-15 (idea) One long-lived status connection in place of a CLI
      process for each health tick.
- [ ] M3-16 The state before the first read of the daemon. The app
      reads the daemon 30 seconds after its start, and until then the
      menu and the Dashboard show `Idle` with the provider
      `Filesystem`, whatever the daemon does. Read the daemon once at
      the start, and show that the state is not known until an answer
      is there. Found by Tier VM (`./scripts/vm.sh`), whose state
      scenarios wait for the first read.
- [ ] M3-17 The screen answers an action late. The app reads the
      daemon each 30 seconds only, so after Sync now, Restart sync, or
      a change of the start-at-login toggle the menu and the Dashboard
      show the old state for up to 30 seconds. Read the daemon right
      after each action that the app starts. Related: M3-15.
- [ ] M3-18 The menu item `Toggle auto-launch`. It does not say
      whether the setting is on, and it names the setting in another
      way than the Dashboard and Settings ("Start Vapor at login").
      Show it as one item with a checkmark and the same wording.
- [ ] M3-19 "Start Vapor at login" set to off stops sync at once. On
      macOS the removal of a launchd job ends its process, so the
      toggle stops the daemon while the app stays open, and
      `vapor service uninstall --keep-running` cannot keep it up (its
      own test says so). The button "Disable auto-launch and stop now"
      in Settings then does nothing more than the toggle. Decide what
      "off" means for the daemon that runs now: keep it running until
      the next logout, or say on the toggle that sync stops, and
      remove the second control.

Exit gate:

- User can understand "what is happening" and "why" from the app without
  shell access.
- Unresolved conflicts are discoverable (badge + notification), listable,
  and resolvable entirely from the app.
- Open decisions and the trash are discoverable, listable, and
  answerable or restorable entirely from the app.

## Profiles UX

- [ ] M4-1 App UI flows to create, rename, select, enable/disable, and
      delete profiles; bind each profile to a provider + authenticated
      account. The create flow is the onboarding sequence re-entered
      from Settings (O-3); the others drive `vapor profiles` (`cli.md`
      L1-7 for enable, disable, and rename).
- [ ] M4-2 Settings UI for profile-scoped override knobs (sync roots,
      ignore rules, resource ceilings, idle-boost, and the per-profile
      `syncMode` selector: `two-way` / `pull-only` / `push-only`). Surface
      each profile's effective mode and its mirror-driven revert/delete
      counts.
- [ ] M4-3 Safe profile disconnect/delete flows that leave other profiles
      untouched (`vapor profiles remove --json`: it keeps the trash and
      names its folder).
- [ ] M4-4 UI flows for multi-provider fan-out (same local root → multiple
      providers/accounts). The runtime allows two profiles on one local
      folder and refuses local folders that lie one inside the other
      (`docs/architecture/data-flow.md`, "Multi-profile watch
      coordination").
- [ ] M4-5 Strict-mirror safety UX for the one-way modes. The daemon
      already holds a one-way profile behind a `one-way-mode` decision
      until its warning is acknowledged, so the app's part is to show
      that question where the mode is chosen and in the decisions pane,
      with the destructive-action confirmation that Apple HIG asks for.
      The copy states what cannot be undone, in the words of
      `docs/architecture/sync-modes.md` §Safety: the local trash keeps
      what a mirror removes on this device; an overwrite, and a removal
      in a cloud without a trash, have no undo.

Exit gate:

- Profiles can be fully managed from the app without CLI intervention.
- One-way sync modes cannot be enabled without the destructive-action
  warning; the app never silently deletes/reverts to converge a mirror.

## Distribution hardening

- [ ] M5-1 Verify signed + notarized path end-to-end against a clean macOS
      host every release cycle, for the app and for the installer.
      `package.sh` reads the notarization status of each and checks the
      stapled app and the stapled installer, but that path has not run:
      no signing material exists yet. It needs two certificates in one
      `.p12`, Developer ID Application and Developer ID Installer
      (`docs/operations/release-process.md`). The unsigned installer is
      built and installed on a runner on every pull request.
- [ ] M5-2 Verify entitlements drift check runs on every release build.
- [ ] M5-3 Verify LaunchAgent/login-item behavior stability across upgrades
      (upgrade from N-1 → N does not leave orphaned plists or duplicate
      login items). Tier VM (`docs/development/vm-verification.md`)
      gives the clean Mac for this: a scenario that installs the
      installer of release N-1 in a fresh guest, then the one of N.
      It needs the older installer as an input of the run.
- [ ] M5-4 Preserve rollback artifacts per `docs/operations/release-
      process.md`.
- [ ] M5-5 What a removal leaves behind. An installer has no
      uninstaller: after the user moves `Vapor.app` to the Trash, the
      link at `/usr/local/bin/vapor` points at nothing, and the two
      LaunchAgents point at binaries that are gone.
      `docs/product/install.md` gives the removal by hand today, as
      six commands. Decide between a `vapor` command that removes the
      install and a menu item in the app. Check also what macOS itself
      keeps after the app is gone: the login item of the app and the
      privacy answers. A fresh guest of Tier VM
      (`docs/development/vm-verification.md`) is the Mac to check it
      on, and the scenario that proves the removal belongs there.
- [ ] M5-6 A restart of the Mac within a few seconds of the first
      start loses the login item. macOS writes a new login item to
      disk a few seconds after the app registers it. After a restart
      inside that window the daemon and the supervisor run, the app
      does not start, and no menu bar item shows until the user opens
      the app, which registers the item again. Decide whether that is
      acceptable, or whether a job that already runs at login should
      start the app. Found by Tier VM: its start-at-login scenario
      waits until macOS saved the item.
- [ ] M5-7 Three notifications at the first start, and how macOS lists
      Vapor's background items. The first start gives "Login Item
      Added" for the app, and one "App Background Activity" each for
      `vapor` and `vapord`, named by their executables. In Login Items
      & Extensions the two jobs are items of an unknown developer, not
      of Vapor. Check what a signed build shows. Then decide whether
      the app should register the two jobs through `SMAppService`, so
      that they belong to Vapor and the user sees one item; the CLI
      must still install the service with no app.

Exit gate:

- Every macOS release cycle validates the macOS trust chain automatically.

## First run and onboarding

The runtime side is in place: an install with no usable configuration
syncs nothing and creates no folder, `vapor status` says `unconfigured`
and names `vapor init`, which is the same first run from a terminal,
and `vapor profiles add` sets the first profile up with no restart. Owned here even though the runtime underneath is
cross-platform, because the onboarding UI is macOS-native; Windows and
Linux get parallel tasks in their own lists once those apps start.

Intent, from the project owner (2026-09-10): the first run of the app
asks before it creates. The user picks the provider for the first
profile, signs in when the provider needs it, confirms the two roots,
and only then does Vapor create folders, adopt roots, and start
syncing. The filesystem provider remains the default for the CLI and
for tests; the app never assumes it.

- [ ] O-1 Clarification pass, then the design: information
      architecture, step sequence, copy, and every UX state (unsigned
      account, folder picker cancelled, overlapping roots, provider
      needing a client id, sign-in refused, a folder that macOS has not
      let Vapor into). The sequence to start from: welcome; provider
      for the first profile (Filesystem, Google Drive; the list comes
      from the CLI so a new provider appears without app work); sign-in
      for a provider that needs an account, driving `vapor auth login
      --browser` and showing its result; local folder (a native folder
      picker, suggestion `~/Vapor`); cloud folder (the provider-aware
      suggestion, editable); sync mode, with the one-way choice behind
      the confirmation from M4-5; start at login, default on; a summary
      and a Start button. This is the order of `vapor init`, and it
      stays that way: the onboarding of the app and `vapor init` are
      the same onboarding, and a change to the steps of one changes
      the other in the same change set (`AGENTS.md` §2). The cloud
      folder step gets a menu of the account's folders, from the same
      listing as the CLI's (`cli.md` L1-8). More settings and options join the sequence
      as the owner defines them at the pass. Follows Apple HIG for
      onboarding: a single window, native controls, one decision per
      step, a Back path, no custom chrome.
- [ ] O-5 The app's first start writes every setting it knows into
      `vapor.json` with its default, the 74 `preIgnoreRules` among
      them (`VaporConfiguration.swift`, `encode(to:)`). A later build
      with a better default does not reach that install, because a
      value in the file wins. Write a key only when the user changes
      it. Found in the Mac guest, 2026-11-22.
- [ ] O-2 The gate. When the runtime reports an unconfigured install,
      the app opens the onboarding window on its own instead of
      the menubar-only surface; this is the one window that may appear
      without the user asking, and only while unconfigured. The daemon
      may already be bootstrapped, since it creates nothing until a
      profile exists, so the status shown during the flow is live.
      Finishing writes the first profile through `vapor profiles add
      --json`, which composes it in the running daemon; the app
      then returns to menubar-first behaviour. Quitting mid-flow leaves
      the install unconfigured and the next launch asks again.
- [ ] O-3 Re-entry. "Add profile…" in Settings reuses the same steps
      for a second profile (this is the create half of M4-1); the
      provider list, the pickers, and the validation are shared, not
      copied. A profile after the first one needs a daemon restart, and
      the flow says so.
- [ ] O-4 Copy in `assets/locales/en.json`, logic tests for the step
      model and the gate (which state opens the window, what each
      answer writes, what a cancel leaves behind), no UI tests in Tier
      1, Tier VM scenarios for the flow, and the list of what the owner
      judges: fresh install shows
      the flow, nothing exists under the home until Start, Google Drive
      sign-in round-trips, the filesystem choice creates exactly the
      two folders chosen.

Exit gate:

- A fresh install creates no folder, adopts no root, and syncs nothing
  until the user finishes onboarding.
- The first profile can be created for either provider without the
  CLI, and the same flow adds a second one from Settings.

## Tests

Policy: `AGENTS.md §9`. Full taxonomy:
`docs/architecture/testing-strategy.md`. macOS-specific scope lives in
`docs/plans/macos.md §7.1`.

- [ ] MT-1 Audit the existing Swift test suite (`apps/macos/Tests/`)
      for trivial-test smell: `Default` impls mirroring constants,
      `Debug`/`Display` string equality tests, round-trips of trivial
      structs. Remove or replace with behavior-level assertions. Keep
      the coverage of configuration, lifecycle, localization, paths,
      logger redaction, bundle layout, and ViewModel state transitions
      intact.
- [ ] MT-2 Tier VM scenarios that the first version does not have
      (`docs/development/vm-verification.md`): the crash-loop state of
      the menu with its acknowledge action, the sign-in notice, the
      configuration issue box, the ignore rules editor and the
      language picker of Settings, the actions Sync now and Restart
      sync, and the login item approval hint. The upgrade and the
      removal scenarios belong to M5-3 and M5-5.
- [ ] MT-3 Identifiers for the menu bar menu. macOS builds its items
      from the SwiftUI views and drops the accessibility identifier,
      so Tier VM finds an item by its catalog text and an assistive
      tool has no stable name for it. An AppKit menu, or the window
      style of the menu bar extra, keeps the identifiers. Both change
      how the menu is built, so the owner decides.
- [ ] MT-4 `./scripts/vm.sh --clean` under a run of another checkout.
      It keeps the prepared guests only while a guest of another
      checkout runs. Between two scenarios of that run no guest runs,
      and the clean deletes the prepared guest that the run clones
      next. Take the store lock and look for guests of other checkouts
      that exist, not only for ones that run.
- [ ] MT-5 The daemon in a Tier VM guest works at the pace of
      `Throttled`. The pointer events of the driver count as a user at
      the device, so the scenario of the syncing state sees
      "Throttled" where an idle Mac shows "Syncing". Find a way to
      show the idle pace too, for example a wait until the daemon
      counts the user as away before the scenario looks.
- [ ] MT-7 A start that waits for a place holds the lock of Tart's
      store for the whole wait, up to 10 minutes (`start_guest` in
      `tools/vm/src/runner.rs`). While a run waits for the place of a
      manual guest, every other run and every `--guest` on the machine
      waits behind it, also for the place that is free. And a manual
      guest that starts between two scenarios of a run takes the place
      that the run's next guest needs. Take the lock only to look and
      to clone, and let a run keep its place between its scenarios.
- [ ] MT-6 Tier VM scenarios against a real cloud provider. A manual
      guest can have the Internet (`./scripts/vm.sh --guest
      --internet`), and a run of the suite cannot. A Google Drive leg
      needs the dedicated test account and a folder of its own for
      each run. The sign-in no longer needs a person each time: the
      token of the account is in the gitignored `.env` as
      `VAPOR_E2E_GDRIVE_TOKEN` (`core.md` TR-8), and `vapor auth login
      --token -` takes it inside a guest. On a local build the daemon
      then cannot read it from the keychain (`core.md` PL-7); until
      that is fixed a scenario has to widen the item's partition list
      itself, as `tools/vm/workloads/README.md` says. Decide with the
      owner whether it is a leg of `./scripts/release.sh` next to the
      Drive leg of Tier E2E (`core.md` TR-8).
