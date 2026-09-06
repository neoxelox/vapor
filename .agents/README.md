# .agents

The vendor-neutral home of everything an AI coding agent reads before
working on Vapor. Real files live here; each agent tool gets the name it
expects as a symlink, so there is one source of truth.

- `AGENTS.md` at the repository root holds the durable rules: intent,
  boundaries, invariants, standards, testing contract, definition of
  done, and an index of the skills. `CLAUDE.md` is a symlink to it.
- `skills/<name>/SKILL.md` holds one procedure each. `.claude/skills/<name>`
  is a relative symlink to the matching directory here, which is how
  Claude Code discovers them.

## Skills

| Skill | Use it when |
|---|---|
| `vapor-unslop` | Writing anything a human will read: docs, commit messages, replies. Always. |
| `vapor-validate` | Before committing a change under `core/*`, `apps/*`, or `scripts/*`; when a script run is red. |
| `vapor-e2e` | A change alters daemon- or CLI-observable behaviour and Tier 1 is green; to watch a feature in the real product. |
| `vapor-debug` | The daemon crashed, will not start, sync is stuck, or a status looks wrong. |
| `vapor-config` | Adding or changing a `vapor.json` key, a `VAPOR_*` variable, a default, a path name, or a launch label. |
| `vapor-provider` | Touching `core/providers` or adding a provider kind. |
| `vapor-docs` | Any non-trivial change; any file added, removed, or renamed under `docs/`. |
| `vapor-commit` | Creating commits or a pull request. |
| `vapor-release` | The owner asks to cut or rehearse a release, or `release.yml` changes. |

## Adding a skill

1. Create `skills/vapor-<word>/SKILL.md`, where the word names an
   entity or an action (`vapor-config`, `vapor-validate`). Front matter
   is limited to `name` (identical to the directory), `description`, and
   optionally `license`, `version`, `allowed-tools`, `user-invocable`.
   Write the description in the third person and say both what the
   skill does and when to use it; it is the only part an agent reads
   when deciding whether to invoke the skill.
2. Add the mirror: `ln -s ../../.agents/skills/<name> .claude/skills/<name>`.
   Without it the skill is invisible to Claude Code.
3. Add a row to the table above and to the skills index in `AGENTS.md`.
4. Check it loads: the symlink resolves to a `SKILL.md`, the front
   matter parses, and after a session restart the skill is listed and
   can be invoked.
