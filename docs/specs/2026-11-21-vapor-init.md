# `vapor init`: the first run from a terminal

Status: implemented 2026-11-21

## Goal

The project owner asked for one command that does everything a first
use of Vapor needs, in the owner's words "like an onboarding, similar
to the onboarding we will do in the apps, that takes care of anything
that must be done the first time vapor is used". `vapor profiles add`
and `vapor service install` stay as commands of their own.

What a user gets: after the install, one command in a terminal takes
the device from nothing to a sync that runs in the background. It
matters most on a server with no desktop, where no app window can ask
the questions.

## Facts

What was true when the design started (`main` at `9e9d533`, version
`0.2.0-alpha.3`):

- D7 of `2026-11-14-full-review.md` says: "The first `add` is the
  setup. There is no `vapor init`."
- `vapor profiles add` does one of the first-run steps. On a terminal
  it asks the provider, the folder on this device, the folder in the
  cloud, and the sync mode, and it asks the one-way question when the
  mode is one-way. Then it prints the sign-in command and stops
  (`core/cli/src/commands/profiles.rs`, `ask_for_missing` and `add`).
- The other steps are separate commands: `vapor auth login`, and
  `vapor service install`, which sets auto-launch on and starts the
  daemon (`core/cli/src/commands/service.rs`).
- On Linux the daemon is a systemd user unit. It stops at logout
  unless the user enables lingering
  (`core/platform/src/service/linux.rs`). The CLI has no code for
  lingering. The policy file of upstream systemd lets a user turn
  lingering on for the own account with no administrator
  (`src/login/org.freedesktop.login1.policy`, action
  `set-self-linger`, read 2026-11-21). A distribution can change
  that. macOS and Windows have no such setting: a LaunchAgent and a
  logon task run while the user is logged in.
- With no app running, nobody restarts a crashed daemon unless
  `vapor service install --supervise` registered the supervisor job
  (`docs/operations/macos/launchagent-policy.md`). The job has one
  label, so a device holds one at most. The app does not use it: a
  timer in the app runs `vapor service check` each 30 seconds
  (`apps/macos/Sources/VaporCore/DaemonHealthMonitor.swift`), and the
  app shows the outcome. The check does not restart a daemon that was
  stopped on purpose, and it counts an exit one time
  (`core/lifecycle/src/manager.rs`). Two checks at the same instant
  write one state file where the last writer wins
  (`core/lifecycle/src/durable.rs`). Not checked: the app and the
  supervisor job on the same device.
- `vapor auth login` needs a profile that exists
  (`core/cli/src/main.rs`, `auth_cmd::select_profile`), and so does
  `vapor auth logout`. The secret store holds a token under the id of
  the profile and the provider (`core/cli/src/commands/auth.rs`). The
  first profile gets the id `default`. With no terminal the sign-in
  reads a token on stdin. A token never goes on a command line
  (`AGENTS.md` §6).
- Vapor asks Google for the full Drive scope, so it can list the
  folders that exist in the account
  (`core/providers/src/gdrive/oauth.rs`). The provider interface
  lists a directory with `enumerate` and makes the cloud root with
  `ensure_cloud_root` (`core/providers/src/lib.rs`). Google Drive
  lets two folders of one parent have the same name.
- With no terminal, `vapor profiles add` refuses unless `--local` and
  `--cloud` are given (D39 of the full review). No flag answers the
  one-way question: the daemon holds the profile until
  `vapor decisions` has the answer (D6 of the full review,
  `AGENTS.md` §4).
- Task O-1 of `docs/tasks/macos.md` plans the app onboarding. Its
  starting order puts sign-in before the two folders. O-2 opens the
  onboarding only while the install is unconfigured, so a device set
  up with `vapor init` does not show it.
- On an unconfigured install, `vapor status`, `vapor doctor`, and the
  daemon log name `vapor profiles add`
  (`core/daemon/src/multi_runtime.rs`, `core/daemon/src/bootstrap.rs`,
  `core/cli/src/commands/doctor.rs`).
- The CLI exits with 0 or 1. `vapor doctor` exits 1 when a check
  fails (`core/cli/src/main.rs`).

## Decisions

Each decision below is the owner's (owner, 2026-11-21).

### Round 1, 2026-11-21: what the command does

| # | Decision |
|---|---|
| D1 | `vapor init` is the command for the whole first run of an install. `vapor profiles add` stays the command that sets up one profile: from a terminal, from a script, and under the apps. `vapor service install` stays too. D1 replaces D7 of `2026-11-14-full-review.md`. |
| D2 | The steps, in this order for now: provider, folder on this device, folder in the cloud, sync mode with the one-way question, sign-in when the provider needs it, start at login, then a summary with the status. Replaced by D9. |
| D3 | The onboarding of the apps and `vapor init` are the same onboarding. A change to the steps changes both. |
| D4 | `vapor init` asks whether Vapor starts at login. The default answer is yes. |
| D5 | On a Linux host with no desktop, `vapor init` asks whether to turn lingering on. The default answer is yes. When the system refuses, it prints the command to run. Replaced by D17. |
| D6 | `vapor init` is idempotent. A later run does only the steps that are not done, and it adds no second profile. When every step is done, it says that Vapor is initialized and shows status information. The app shows its onboarding once, in the same way. |
| D7 | Each answer of `vapor init` can be given as a flag, so the command also runs with no terminal. |
| D8 | On an unconfigured install, `vapor status` names `vapor init`. |

### Round 2, 2026-11-21: order, flags, and the supervisor

| # | Decision |
|---|---|
| D9 | Sign-in comes right after the provider, before the two folders. The folder step can then show the folders of the account, so the user picks one that exists or makes a new one. The order is: provider, sign-in when the provider needs it, folder on this device, folder in the cloud, sync mode with the one-way question, start at login, then a summary with the status. D9 replaces D2. |
| D10 | D3 is kept by a written rule for contributors. No code enforces it. |
| D11 | The flags are `--provider <kind>`, `--local <path>`, `--cloud <path>`, `--mode <mode>`, `--token -`, `--auto-launch <on\|off>`, `--linger <on\|off>`, and `--json`. The first five mean what they mean on `vapor profiles add` and `vapor auth login`. |
| D12 | With no terminal and no flag, start at login and lingering take the answer yes. |
| D13 | `vapor init` exits with 0 when Vapor is initialized. It exits with 1 when a step failed or waits for a person, and it names the step. |
| D14 | `vapor init` registers the supervisor job on every host. The daemon is one for the app and the CLI, and so is the job: a registration that finds the job adds no second one. |

### Round 3, 2026-11-21: the folder step, lingering, and the app

| # | Decision |
|---|---|
| D15 | The first build takes a typed cloud folder, with a suggestion. It checks the folder in the account and says whether the folder exists or is new. The menu of cloud folders that D9 names is the next task, for the CLI and the apps together. |
| D16 | A sign-in stays when the user stops before a profile exists. The next `vapor init` continues at the folders. `vapor auth logout` removes a sign-in that has no profile yet. |
| D17 | On a Linux host with no desktop, Vapor turns lingering on with no question when it registers the background service, and the summary says so. `--linger off` stops it. `vapor service install` does the same. A Linux desktop, macOS, and Windows need no such setting. D17 replaces D5. |
| D18 | The supervisor job is the one thing that restarts a crashed daemon, for the app too. The app registers the job when it is missing. Its check each 30 seconds only reads the state. |

### Round 4, 2026-11-21: two flags

| # | Decision |
|---|---|
| D19 | `vapor service install --supervise` goes. With D18 every install has the supervisor job. |
| D20 | `--linger` exists only in the Linux build. The other platforms do not show it and do not take it. |

## Design

A plan for the build. The code decides the details.

- `core/cli` gets the `init` command. It runs the steps of D9 with
  the code that `vapor profiles add`, `vapor auth login`, and
  `vapor service install` run already, and with the same line-based
  questions. It adds no second way to write a profile.
- Each step reads its state from what is true on the device: a
  profile exists, a sign-in is stored, the service is registered or
  auto-launch is off. No marker file says "initialized". That is how
  D6 holds after a profile is removed or a sign-in expires.
- Sign-in before a profile (D9, D16): the token goes into the secret
  store under the id that the first profile gets. `vapor init` is what
  signs in there, since it knows the provider, and `vapor auth logout`
  removes such a sign-in. When the profile is written for another
  provider, the sign-in of the first one is removed.
- Lingering (D17) is Linux code, so it lives in
  `core/platform/src/service/linux.rs` behind the service trait. The
  other platforms need no change for it. "No desktop" is the test
  that the idle detection uses already.
- The supervisor (D14, D18): the service install path registers the
  supervisor job for every caller. The Mac app keeps its timer for
  the menu bar state, and the timer no longer restarts the daemon: it
  reads `vapor service status`, which carries the `health` that a
  check would find. The supervisor also ends itself when an update
  replaces its binary, so the new build takes over with no step by the
  user.
- A one-way mode that waits for its answer makes the profile step
  wait, on the run that writes the profile and on every later run,
  until the acknowledgement is in the state database of the
  profile.
- A refusal to turn lingering on leaves the service step done, with
  the command for an administrator in its detail. `vapor doctor` does
  not check lingering yet (`docs/tasks/cli.md` L5-4).
- A host with no service manager, such as a container, runs
  `vapor init --auto-launch off` and then `vapor run`. That answer
  registers no job, the supervisor included: D14 holds where the
  background service is registered. It also looks at nothing, so a
  sandbox or a test never reaches the service manager of the host.
- The service step is done only with both jobs registered. A daemon
  job with no supervisor next to it is installed again.
- A host whose secret store keeps nothing past the process (no
  keychain and no secrets command) fails the sign-in step with the
  reason, and no sign-in is made.
- `vapor init --json` asks nothing and prints one object: each step
  with its state, and the status. Keys are camelCase and values are
  kebab-case, as on every command.
- The rule of D10 goes into `AGENTS.md` §2, `docs/plans/cli.md`, and
  task O-1 of `docs/tasks/macos.md`. O-1 takes the order of D9.

Three recorded rules limit the flags of D11:

- With no terminal, `--local` and `--cloud` are required (D39 of the
  full review).
- A token goes on stdin only (`AGENTS.md` §6).
- No flag answers the one-way question. A script answers it with
  `vapor decisions resolve` (`AGENTS.md` §4).

How the invariants hold:

- `AGENTS.md` §1: `vapor init` creates no folder before the profile
  is written. A stored sign-in is a secret, not a folder.
- `AGENTS.md` §2: the restart of a crashed daemon leaves the app and
  stays in `core/lifecycle`.
- `AGENTS.md` §4: a one-way mode starts only after its question has
  an answer, on a terminal or in `vapor decisions`.
- `AGENTS.md` §6: the token goes through `SecretStore`, in the scope
  of the runtime directory.

## Verification

What ran on 2026-11-21: the format and lint scripts, Tier 1, and the
e2e suite on macOS; the lint of the platform, lifecycle, and CLI crates
for Linux in a container. The new scenario S70 failed on the base
commit ("unrecognized subcommand 'init'") and passes on the build. Not
run: the `--full` service scenario, which is for disposable runners,
and the real `loginctl`, which needs a systemd user session.

- Unit tests with the scripted prompter: which steps a given state
  leaves to do, the flags, the rules for no terminal, the exit codes
  of D13, and a second run that changes nothing.
- A shape lock for `vapor init --json`.
- A `core/lifecycle` test: two checks of one exit count one crash.
- An e2e scenario: a fresh sandbox, `vapor init` with flags and
  `--auto-launch off`, then a sync of one file, then a second
  `vapor init` that reports "initialized" with exit 0. It fails on
  the base commit, where the command does not exist.
- The service step and the supervisor job run in the `--full` phase
  of the e2e suite, which installs a real service on a disposable
  runner. Lingering needs a systemd user session on the runner, the
  same limit as task L2-6 of `docs/tasks/cli.md`.
- The change in the Mac app gets a manual checklist for the owner.

## Open points

None.
