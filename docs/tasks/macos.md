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
      conflict. Naming the file inside the notification depends on
      `core.md` C8-71.
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
      providers/accounts). Waits for `core.md` E-3, which decides what
      overlapping local roots mean.
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
      login items).
- [ ] M5-4 Preserve rollback artifacts per `docs/operations/release-
      process.md`.
- [ ] M5-5 What a removal leaves behind. An installer has no
      uninstaller: after the user moves `Vapor.app` to the Trash, the
      link at `/usr/local/bin/vapor` points at nothing, and the two
      LaunchAgents point at binaries that are gone.
      `docs/product/install.md` gives the three steps by hand today.
      Decide between a `vapor` command that removes the install and a
      menu item in the app.

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
      answer writes, what a cancel leaves behind), no UI tests, and the
      manual verification checklist for the owner: fresh install shows
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
