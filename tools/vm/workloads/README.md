# Workloads for a manual guest

Scripts that use an installed Vapor hard, from inside a Tier VM guest,
and judge it from the outside: the two folders on disk, `vapor
status`, the state database (read-only), and the daemon log. They are
the tools of the heavy-user session of 2026-11-22. They are not a
tier and no script of the repository runs them; making them a run
that anyone can repeat is open work (`docs/tasks/core.md`).

They never ship, and they run only inside a guest. They kill the
daemon, move the sync roots away, answer every `mass-delete` question
with `apply`, and write to `~/Vapor`, `~/CloudDrive`, and `~/.vapor`
of the user they run as: on a machine with a real install that is
damage. `vt.py`, which the others import, and `wl_npm.sh` refuse to
start where the read-only share of a guest is missing.

## How to run them

```sh
./scripts/vm.sh --guest --second            # a guest, in the place of the fresh guests
mkdir -p .vapor/vm/manual-<id>/tools
cp tools/vm/workloads/* .vapor/vm/manual-<id>/tools/   # the guest sees this folder
tools/vm/workloads/in-guest.sh 'vapor init --provider filesystem --local ~/Vapor --cloud ~/CloudDrive/Vapor'
tools/vm/workloads/in-guest.sh 'python3 "$R/tools/vt.py" mon start'
tools/vm/workloads/in-guest.sh 'cd ~; python3 "$R/tools/edge.py" run names'
./scripts/vm.sh --guest-stop
```

`.vapor/vm/manual-<id>/` is the directory the guest mounts read-write
as `/Volumes/My Shared Files/results` (`$R` in a command of
`in-guest.sh`). Reports and the sampler's CSV land in its `out/`.

The guest image has Python 3, git, node, npm, and sqlite3.

## What each one does

| File | What it does |
|---|---|
| `vt.py` | The helpers the others import. `st`: one line of state. `wait`: until the queue is empty and nothing was queued for 10 s; a queue that holds only intents that failed three times counts as settled and is listed as `stuck`. `compare`: the tree oracle, local root against cloud root by size and SHA-256. `mon start` and `mon stop`: a sampler of the daemon's CPU, memory, and queue. `logscan`: warnings and errors of the daemon log. |
| `edge.py` | Edge cases in both directions, in groups: `names` (88 names), `types` (sizes around 8 MiB, modes, links, special files, attributes, sparse files, bundles), `ops` (modify, delete, rename, fast sequences, a file that becomes a folder, out of the root and back), `bulk`. |
| `conflicts.py` | Two-sided cases, one after the other: an event with no new bytes on one side while an edit waits on the other, real conflicts (paused, stopped, live), delete against edit, renames on both sides, a file against a folder, alternating writes, an edit during a download, answering a question. |
| `lifecycle.py` | Pause and stop with changes on both sides, `kill -9` during uploads, a cloud folder that goes away, another folder at the local path, ignore rules that change. `LC-crash-loop` was not run in the session. |
| `case_rename.py` | A folder renamed by capitals only, then an edit, a child rename, and a delete inside it, with and without a wait between. |
| `extras.py` | 200 MiB and 1 GiB files timed in both directions, `git init` with no commit, and the same tree on both sides before the first sync. |
| `wl_coding.py` | A person or an agent that writes code: a real git repository, saves the way editors save (in place, a temporary file and a rename, the JetBrains and Vim sequences), bursts over many files, commits, branch switches. It keeps a model of what it wrote and checks each file before it touches it again, so a sync that puts an older version back is caught in the act. `check` compares the model, the tree, and git on both sides. |
| `wl_mover.py` | A person who reorganizes a library: moves, folder renames, bulk renames, duplicates, deletes, out of the root and back, renames by capitals. `check` compares names without case and counts payloads. |
| `wl_npm.sh` | Real `npm ci`, `npm install`, `npm uninstall`, and `rm -rf node_modules` from an offline cache, with the time Vapor needs after each. |
| `latency.py` | Times one change at a time in both directions: a new file, an edit, a media file, a 5 MB file, a deletion, a rename. Between the two roots of the filesystem provider, or between two local folders that sync through one cloud. |
| `probe.py` | Saves one file every few seconds in each folder it is given and times its arrival on the other side. Run it next to a workload to see what a single save costs meanwhile. |
| `answer_loop.py`, `resolve_all.py` | A user who answers `mass-delete` questions with `apply` a few seconds after they show; and one pass that answers every open question. |
| `drive_tests.py`, `drive_conflict.py` | Two profiles on one Google Drive folder, as two devices: first files, the time of one save from device to device, names, operations both ways, a git repository, a burst and its removal, and a conflict in five variants. They need a guest with the Internet and the dedicated test account. |
| `presence.swift` | Posts a small pointer move every few seconds, so macOS counts a user at the device and the daemon runs `Throttled`. |
| `in-guest.sh`, `monstat.py` | Host side: run a command in the manual guest, and summarize the sampler's CSV. |

## What they need that the repository does not hold

- **The npm cache.** `wl_npm.sh setup` unpacks
  `$R/fixtures/npm-cache.tar` and copies `$R/fixtures/webapp/`
  (`package.json`, `package-lock.json`). Make them on the host, with a
  network, in a scratch directory: `npm install --cache <dir>
  --ignore-scripts <packages>`, then `tar -cf npm-cache.tar -C
  <parent> npm-cache`. The session used react, vite, vitest,
  typescript, eslint, and a few more: 10 493 files, 126 MB installed.
- **`presence`.** Build it on the host and copy it next to the tools:
  `swiftc -O -o presence presence.swift`.
- **Google Drive.** A guest with `--internet` and a build that carries
  the OAuth client (`.env`). The test account signs in with `<command
  that prints the token> | vapor auth login --profile <id> --token -`.
  `drive_tests.py` and `drive_conflict.py` expect two profiles, one
  on `~/VaporDrive` and one on `~/VaporDrive2`, on one Drive folder.
  On a local build the daemon cannot read that sign-in from the
  keychain (open work in `docs/tasks/core.md`). In a guest, sign in
  through the real path of the CLI
  (`/Applications/Vapor.app/Contents/Helpers/vapor`), then let the
  daemon's binary into the item, and restart the service:
  `security set-generic-password-partition-list -S
  "cdhash:<vapord>,cdhash:<vapor>,apple-tool:,apple:" -s sh.arn.vapor
  -a auth.<profile>.gdrive.token -k admin`, with the two hashes from
  `codesign -dvvv`. `admin` is the password of the guest image.

## Limits

- A case states what it expects in words; read a `FAIL` before you
  believe it. Two "failures" of the session were the tools: a rerun in
  the same folder made a second conflict copy, and a blind write into
  the cloud folder 0.25 s after an upload is last writer wins, not a
  lost edit.
- `vt.py wait` reads one profile: the implicit one, or the one named
  by `VT_PROFILE`.
- A change from the cloud side takes seconds to arrive, and a minute
  with `presence` running. A check that runs right after `wait` can
  come before it.
- `wl_coding.py` takes the tree as its model after each branch switch.
  A version that sync put back just before a switch is committed with
  the rest and is not reported.
- The daemon does not show a folder that waits for its deferred scan
  as queued work, so `wait` can return before a burst has started to
  move. Compare the trees, do not trust the quiet.
