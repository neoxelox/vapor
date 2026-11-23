# What syncs, and what a project loses on the other side

Vapor syncs the content of files. A folder is a container for them, and
everything else a filesystem can hold is left out on purpose. This page
says what that means for a real project. The rules behind it, and the
reasons in engine terms, are in
`docs/architecture/data-flow.md` §Directory and symlink semantics.

## What arrives on the other side

- Every regular file, byte for byte, with its name and its place in the
  folder tree.
- On a cloud folder that is a folder on a disk, the permission bits of a
  file: a script that can be run stays one. Google Drive has no such
  bits.
- Deletions, renames, and moves of files and of folders.

## What does not arrive

| What | What you see on the other device | Why |
|---|---|---|
| Symbolic links | The link is missing. In a JavaScript project whose `node_modules` you chose to sync, `node_modules/.bin` is empty, so `npm run` fails there until you run an install on that device. | A link can point outside the synced folder, and following it would pull files in that you never chose. A cloud provider has nothing that stands for a link. |
| Empty folders | The folder is missing until it holds a file. A repository made with `git init` is "not a git repository" on the other device until its first commit, because `.git/objects` and `.git/refs` are empty until then. | A folder is made on the other side by the first file inside it. Vapor has no object for a folder of its own, which keeps conflicts and deletions about content only. |
| The holes of a sparse file | The file arrives with every byte written out. A sparse file of 64 MiB that takes 16 KiB on one device takes 64 MiB on the other. | A hole reads as zeros, and the zeros are what is transferred. |
| Hard links | Two names of one file arrive as two files. | The link is a property of one disk. |
| Pipes, sockets, and devices | Nothing. | They have no content to transfer. |
| Extended attributes, tags, and the modification time | The file arrives without them. | The content is what is compared and synced. |
| A change of permissions alone | Nothing changes on the other side until the content changes too. | A change is found by the content of a file. |
| Files that an ignore rule covers | Nothing, in both directions. | `docs/product/ignore-rules.md` lists the rules and how to change them. |

## What to do about it

- After the first sync of a project with dependencies, run its install
  on the other device (`npm install`, `bundle install`). The default
  ignore rules leave `node_modules` out, so that device needs the
  install in any case.
- Do not use a synced folder as the only copy of a git repository that
  has no commit yet.
- Keep a file that must stay sparse (a disk image) outside the synced
  folder, or add an ignore rule for it.
