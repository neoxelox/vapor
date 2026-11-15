# Ignore rules

Which files Vapor leaves out of sync, where the rules come from, how
they are read, and how to see what they hold back. The root
`README.md` keeps the four keys in its Configuration table and links
here.

## What "ignored" means

- A name that matches the rules never syncs, in either direction. It
  is not uploaded, it is not downloaded, and it can never produce a
  conflict copy. An ignored `.DS_Store` in the cloud stays in the
  cloud.
- The same rules apply everywhere: to the filesystem watcher, before
  an event is even recorded; to the changes that arrive from the
  cloud; and to the comparison of both folders.
- An ignored folder is not entered. Nothing under it is read.

## Where the rules come from

Four sources, from the lowest precedence to the highest. A later
source can override an earlier one.

| # | Source | Setting | Default |
|---|---|---|---|
| 1 | Rules applied first | `preIgnoreRules` | a built-in list (below) |
| 2 | `.gitignore` files under the local root | `useGitignore` | `true` |
| 3 | `.vaporignore` files under the local root | `useVaporignore` | `true` |
| 4 | Rules applied last | `postIgnoreRules` | `[]` |

- A `.gitignore` or a `.vaporignore` applies to the folder it is in
  and to everything below it, as in git. Every such file under the
  local root is read.
- `.vaporignore` is for rules that are about sync and have no place in
  a repository's `.gitignore`. It sits after `.gitignore`, so it can
  bring back a file that a `.gitignore` hides.
- A `!rule` in `postIgnoreRules` wins over everything else.
- An ignore file inside a folder that the rules so far already hide is
  not read: a vendored package cannot bring its own files back.

## How a rule is read

Rules use the `.gitignore` syntax and mean what they mean in git:

```gitignore
*.log            # every file that ends in .log
build/           # a folder named build, and not a file of that name
/notes.txt       # notes.txt next to this ignore file only
!keep.log        # show keep.log again
```

- A rule that ends in `/` matches folders only.
- A file under a hidden folder cannot be shown again. `logs/` followed
  by `!logs/keep.txt` still hides `keep.txt`. Hide the contents
  instead: `logs/*` followed by `!logs/keep.txt`.

## The built-in list

`preIgnoreRules` starts as a list of paths that a machine can make
again and that would otherwise dominate the sync:

- dependency folders: `node_modules/`, `vendor/`, `Pods/`, and the
  like;
- build output: `dist/`, `build/`, `out/`, `target/`, `_build/`;
- tool caches: `__pycache__/`, `.gradle/`, `.terraform/`, `.cache/`;
- OS and editor junk: `.DS_Store`, `Thumbs.db`, swap, temporary and
  partial files, logs;
- what an OS keeps at the root of a volume: `.fseventsd/`,
  `.Spotlight-V100/`, `System Volume Information/`.

Two things are left in on purpose: repositories (`.git/`) and dotenv
files sync like any other content.

A value of your own replaces the whole list. To add to it, put the
extra rules in `postIgnoreRules` or in a `.vaporignore`. To bring back
one path that the list hides, add a `!rule` for it in the same places.
The full list is `DEFAULT_PRE_IGNORE_RULES` in
`core/shared/src/constants.rs`.

## What no rule can change

- A folder named `.vapor` never syncs, at any depth, with everything
  under it. It is Vapor's own runtime state or the trash of a volume.
  `.vaporignore` is a different name and syncs like any file.
- Vapor's own working files never sync: `.vapor-tmp-*`,
  `*.vapor-meta.json`, and `.vapor-root`.

## When a rule changes

- A change applies within seconds, in an ignore file or in
  `vapor.json`, with no restart. A scan of both folders follows, so a
  file that a rule frees starts to sync.
- A file that already synced and that a new rule hides stays where it
  is on both sides. Vapor stops syncing it; it deletes nothing.
- A folder that arrives with an ignore file of its own (a project that
  is moved in with its `.gitignore`) syncs by those rules from the
  start: Vapor loads them before it looks at the files.
- Files that are written in the same seconds as a new ignore file next
  to them can be picked up before its rules load. This is open work
  (`docs/tasks/core.md`).

## See what the rules hold back

- `vapor status` counts the local paths that the rules keep out of
  sync, for each profile.
- `vapor ignored` lists each one with the rule that hides it, as the
  rule is written. A hidden folder is one line. `--profile <id>` picks
  a profile, and `--json` prints the list for a script. The list stops
  at 5 000 entries and says so.

## The settings

```sh
vapor config set useGitignore false
vapor config set postIgnoreRules '["*.bak", "!important.bak"]'
```

All four keys apply while Vapor runs. Defaults, types, and how a
change reaches the daemon: `configuration.md`.
