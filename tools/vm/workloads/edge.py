#!/usr/bin/env python3
"""edge: a battery of edge cases against the installed Vapor.

A case is a generator. The code before each `yield` acts on the two
folders; the runner then waits until the daemon is quiet, and the case
goes on (a check, or the next action). Cases of one group run together.

  edge.py list
  edge.py run <group> [--only id,id] [--timeout S]
"""
import argparse
import hashlib
import json
import os
import shutil
import socket
import subprocess
import sys
import time
import unicodedata

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import vt  # noqa: E402

L = vt.LOCAL
C = vt.CLOUD
RESULTS = []


class Ctx:
    def __init__(self, cid, title):
        self.id = cid
        self.title = title
        self.notes = []
        self.fails = []
        self.infos = []

    def expect(self, cond, msg):
        (self.notes if cond else self.fails).append(msg)
        return cond

    def info(self, msg):
        self.infos.append(msg)


def other(side):
    return C if side == L else L


TAG = os.environ.get("EDGE_TAG", "")


def name_of(side):
    return ("local" if side == L else "cloud") + TAG


def w(path, data, mode=None):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    if isinstance(data, str):
        data = data.encode()
    with open(path, "wb") as fh:
        fh.write(data)
    if mode is not None:
        os.chmod(path, mode)


def r(path):
    try:
        with open(path, "rb") as fh:
            return fh.read()
    except OSError:
        return None


def exists(path):
    return os.path.lexists(path)


def sh(cmd, timeout=120):
    p = subprocess.run(cmd, shell=True, capture_output=True, text=True, timeout=timeout)
    return p.returncode, (p.stdout + p.stderr).strip()


def listing(path):
    try:
        return sorted(os.listdir(path))
    except OSError:
        return None


def decisions():
    d = vt.vapor_json("decisions", "list") or {}
    return d.get("decisions", [])


def same(ctx, rel, src_side, what=None):
    """The file at `rel` on the other side holds the bytes of this side."""
    a = r(os.path.join(src_side, rel))
    b = r(os.path.join(other(src_side), rel))
    label = what or repr(rel)
    if a is None:
        ctx.fails.append(f"{label}: missing on the {name_of(src_side)} side, where it was written")
        return False
    if b is None:
        ctx.fails.append(f"{label}: did not reach the {name_of(other(src_side))} side")
        return False
    if a != b:
        ctx.fails.append(f"{label}: content differs ({len(a)} vs {len(b)} bytes)")
        return False
    ctx.notes.append(f"{label}: synced")
    return True


# ---------------------------------------------------------------------
# Group: names. Each case writes on one side and expects the other side.
# ---------------------------------------------------------------------

NAMES = [
    ("plain", "plain.txt"),
    ("space", "with space.txt"),
    ("lead-space", " leading space.txt"),
    ("trail-space", "trailing space .txt "),
    ("trail-dot", "trailing dot."),
    ("nfc", unicodedata.normalize("NFC", "café-nfc.txt")),
    ("nfd", unicodedata.normalize("NFD", "café-nfd.txt")),
    ("emoji", "🔥📁 fire.txt"),
    ("cjk", "日本語のファイル.txt"),
    ("rtl", "مرحبا.txt"),
    ("zwj", "family-👨‍👩‍👧‍👦.txt"),
    ("colon", "a:b.txt"),
    ("quote", 'quote"s.txt'),
    ("apos", "apos'trophe.txt"),
    ("star", "star*.txt"),
    ("ques", "ques?.txt"),
    ("pipe", "pipe|.txt"),
    ("angle", "<angle>.txt"),
    ("percent", "percent%20.txt"),
    ("hash", "hash#.txt"),
    ("amp", "amp&.txt"),
    ("semi", "semi;.txt"),
    ("dollar", "dollar$HOME.txt"),
    ("backtick", "back`tick`.txt"),
    ("newline", "new\nline.txt"),
    ("tab", "tab\tname.txt"),
    ("ctrl", "ctrl\x01char.txt"),
    ("dotfile", ".hidden"),
    ("dotenv", ".env"),
    ("dotdot", "..double"),
    ("dots3", "..."),
    ("dash", "-rf"),
    ("ddash", "--help"),
    ("tilde-lead", "~tilde"),
    ("win-con", "CON"),
    ("win-aux", "aux.txt"),
    ("noext", "Makefile"),
    ("multi-ext", "archive.tar.gz"),
    ("long255", "L" * 251 + ".txt"),
    ("looks-like-conflict", "report~conflict-otherdevice-1700000000000.txt"),
    ("square", "[brackets] {braces} (parens).txt"),
    ("comma-eq", "a,b=c+d@e!.txt"),
    ("unicode-space", "nbsp name.txt"),
    ("bom-like", "﻿bom.txt"),
]


def make_name_case(side, key, name):
    def case(ctx):
        rel = f"edge/names-{name_of(side)}/{name}"
        payload = f"{key} from {name_of(side)}\n"
        try:
            w(os.path.join(side, rel), payload)
        except OSError as e:
            ctx.info(f"the filesystem refused the name: {e}")
            return
        yield
        same(ctx, rel, side, what=f"name {name!r}")
    return case


# Names Vapor documents as never syncing or as needing a question.
def case_backslash(side):
    def case(ctx):
        rel = f"edge/names-{name_of(side)}/back\\slash.txt"
        w(os.path.join(side, rel), "backslash\n")
        yield
        got = exists(os.path.join(other(side), rel))
        ds = [d for d in decisions() if d.get("kind") == "unsyncable-name"]
        if side == L:
            ctx.expect(not got, "a name with a backslash stays on this device")
            ctx.expect(bool(ds), f"an `unsyncable-name` question is open ({len(ds)})")
        else:
            ctx.info(f"written in the cloud: reached local={got}; unsyncable-name questions={len(ds)}")
    return case


def case_internal_names(side):
    def case(ctx):
        base = f"edge/internal-{name_of(side)}"
        names = ["notes.vapor-meta.json", ".vapor-tmp-user", "sub/.vapor-root", ".vapor/inside.txt", ".vaporignore-not.txt"]
        for n in names:
            w(os.path.join(side, base, n), f"user file named {n}\n")
        w(os.path.join(side, base, "normal.txt"), "normal\n")
        yield
        for n in names:
            got = exists(os.path.join(other(side), base, n))
            ctx.info(f"{n!r}: reached the other side={got}")
        same(ctx, f"{base}/normal.txt", side)
        same(ctx, f"{base}/.vaporignore-not.txt", side)
    return case


def case_deep_path(side):
    def case(ctx):
        # 60 levels of 12 characters: a path of about 800 bytes below the root.
        rel = "edge/deep-" + name_of(side) + "/" + "/".join(f"level-{i:05d}" for i in range(60)) + "/leaf.txt"
        full = os.path.join(side, rel)
        try:
            w(full, "deep\n")
        except OSError as e:
            ctx.info(f"could not create a {len(full)}-byte path: {e}")
            return
        ctx.info(f"path length {len(full)} bytes")
        yield
        same(ctx, rel, side, what="a file 60 folders deep")
    return case


# ---------------------------------------------------------------------
# Group: types. Sizes, modes, links, special files, attributes.
# ---------------------------------------------------------------------

def case_sizes(side):
    def case(ctx):
        base = f"edge/sizes-{name_of(side)}"
        mib = 1 << 20
        sizes = {"empty": 0, "one-byte": 1, "4k": 4096, "8mib-minus1": 8 * mib - 1, "8mib": 8 * mib, "8mib-plus1": 8 * mib + 1, "33mib": 33 * mib + 7}
        for n, size in sizes.items():
            blob = hashlib.sha256(n.encode()).digest()
            data = (blob * (size // len(blob) + 1))[:size]
            w(os.path.join(side, base, n + ".bin"), data)
        yield
        for n in sizes:
            same(ctx, f"{base}/{n}.bin", side, what=f"{n} file")
    return case


def case_modes(side):
    def case(ctx):
        base = f"edge/modes-{name_of(side)}"
        w(os.path.join(side, base, "script.sh"), "#!/bin/sh\necho hi\n", 0o755)
        w(os.path.join(side, base, "readonly.txt"), "read only\n", 0o444)
        w(os.path.join(side, base, "private.txt"), "private\n", 0o600)
        w(os.path.join(side, base, "unreadable.txt"), "nobody can read this\n", 0o000)
        w(os.path.join(side, base, "setgid.txt"), "odd bits\n", 0o2644)
        yield
        o = other(side)
        for n, mode in (("script.sh", 0o755), ("readonly.txt", 0o444), ("private.txt", 0o600)):
            if same(ctx, f"{base}/{n}", side):
                got = os.stat(os.path.join(o, base, n)).st_mode & 0o777
                ctx.expect(got == mode, f"{n}: mode {oct(got)} on the other side, {oct(mode)} here")
        got = exists(os.path.join(o, base, "unreadable.txt"))
        ctx.info(f"a file with mode 000: reached the other side={got}")
        st = vt.status_line()
        ctx.info(f"status with an unreadable file: queued={st['queued']} failed={st['failed']} db_pending={st['db_pending']}")
        os.chmod(os.path.join(side, base, "unreadable.txt"), 0o644)
        yield
        same(ctx, f"{base}/unreadable.txt", side, what="the file after it became readable")
    return case


def case_links(side):
    def case(ctx):
        base = os.path.join(side, f"edge/links-{name_of(side)}")
        w(os.path.join(base, "target.txt"), "target\n")
        os.makedirs(os.path.join(base, "dir"), exist_ok=True)
        w(os.path.join(base, "dir/inside.txt"), "inside\n")
        os.symlink("target.txt", os.path.join(base, "link-to-file"))
        os.symlink("dir", os.path.join(base, "link-to-dir"))
        os.symlink("nowhere", os.path.join(base, "dangling"))
        os.symlink("/etc/hosts", os.path.join(base, "absolute-outside"))
        os.symlink("loop-b", os.path.join(base, "loop-a"))
        os.symlink("loop-a", os.path.join(base, "loop-b"))
        os.symlink("..", os.path.join(base, "dir/parent"))
        os.link(os.path.join(base, "target.txt"), os.path.join(base, "hardlink.txt"))
        yield
        rel = f"edge/links-{name_of(side)}"
        o = os.path.join(other(side), rel)
        same(ctx, f"{rel}/target.txt", side)
        same(ctx, f"{rel}/dir/inside.txt", side)
        same(ctx, f"{rel}/hardlink.txt", side, what="a hard link")
        for n in ("link-to-file", "link-to-dir", "dangling", "absolute-outside", "loop-a", "loop-b", "dir/parent"):
            ctx.expect(not exists(os.path.join(o, n)), f"symlink {n}: not on the other side (by contract)")
    return case


def case_specials(side):
    def case(ctx):
        base = os.path.join(side, f"edge/specials-{name_of(side)}")
        os.makedirs(base, exist_ok=True)
        os.mkfifo(os.path.join(base, "pipe.fifo"))
        s = socket.socket(socket.AF_UNIX)
        cwd = os.getcwd()
        os.chdir(base)
        try:
            s.bind("sock")
        finally:
            os.chdir(cwd)
        w(os.path.join(base, "next-to-them.txt"), "regular\n")
        yield
        s.close()
        rel = f"edge/specials-{name_of(side)}"
        same(ctx, f"{rel}/next-to-them.txt", side, what="a regular file next to a FIFO and a socket")
        o = os.path.join(other(side), rel)
        ctx.expect(not exists(os.path.join(o, "pipe.fifo")), "the FIFO is not on the other side")
        ctx.expect(not exists(os.path.join(o, "sock")), "the socket is not on the other side")
    return case


def case_xattrs(side):
    def case(ctx):
        rel = f"edge/xattr-{name_of(side)}"
        f = os.path.join(side, rel, "tagged.txt")
        w(f, "tagged\n")
        sh(f"xattr -w com.apple.metadata:_kMDItemUserTags 'Red' '{f}'")
        sh(f"xattr -w user.custom 'my value' '{f}'")
        q = os.path.join(side, rel, "quarantined.txt")
        w(q, "from the web\n")
        sh(f"xattr -w com.apple.quarantine '0081;00000000;Safari;' '{q}'")
        locked = os.path.join(side, rel, "locked.txt")
        w(locked, "locked in Finder\n")
        sh(f"chflags uchg '{locked}'")
        yield
        o = other(side)
        same(ctx, f"{rel}/tagged.txt", side)
        same(ctx, f"{rel}/quarantined.txt", side)
        same(ctx, f"{rel}/locked.txt", side, what="a file with the Finder lock")
        _, out = sh(f"xattr '{os.path.join(o, rel, 'tagged.txt')}'")
        ctx.info(f"extended attributes on the other side: {out.split()}")
        _, out = sh(f"xattr '{os.path.join(o, rel, 'quarantined.txt')}'")
        ctx.info(f"quarantine attribute on the other side: {'com.apple.quarantine' in out}")
        # A change on the other side now has to replace a locked file here.
        w(os.path.join(o, rel, "locked.txt"), "changed on the other side\n")
        yield
        got = r(locked)
        st = vt.status_line()
        ctx.info(f"a change for a locked file: local still old={got == b'locked in Finder' + bytes([10])}; queued={st['queued']} failed={st['failed']} pending={st['db_pending']}")
        sh(f"chflags nouchg '{locked}'")
        yield
        same(ctx, f"{rel}/locked.txt", o, what="the locked file after the lock was removed")
    return case


def case_sparse(side):
    def case(ctx):
        rel = f"edge/sparse-{name_of(side)}/sparse.img"
        f = os.path.join(side, rel)
        os.makedirs(os.path.dirname(f), exist_ok=True)
        with open(f, "wb") as fh:
            fh.truncate(64 << 20)
            fh.seek(32 << 20)
            fh.write(b"island")
        yield
        if same(ctx, rel, side, what="a 64 MiB sparse file"):
            a = os.stat(f).st_blocks * 512
            b = os.stat(os.path.join(other(side), rel)).st_blocks * 512
            ctx.info(f"bytes on disk: {a} here, {b} on the other side")
    return case


def case_bundle(side):
    def case(ctx):
        rel = f"edge/bundle-{name_of(side)}/Thing.app"
        w(os.path.join(side, rel, "Contents/Info.plist"), "<plist/>\n")
        w(os.path.join(side, rel, "Contents/MacOS/Thing"), "#!/bin/sh\n", 0o755)
        w(os.path.join(side, rel, "Contents/Resources/en.lproj/Localizable.strings"), "x\n")
        os.makedirs(os.path.join(side, rel, "Contents/Frameworks"), exist_ok=True)
        os.makedirs(os.path.join(side, f"edge/bundle-{name_of(side)}/empty-folder"), exist_ok=True)
        yield
        for n in ("Contents/Info.plist", "Contents/MacOS/Thing", "Contents/Resources/en.lproj/Localizable.strings"):
            same(ctx, f"{rel}/{n}", side)
        ctx.info(f"empty folder inside a bundle on the other side: {exists(os.path.join(other(side), rel, 'Contents/Frameworks'))}")
        ctx.info(f"empty folder on the other side: {exists(os.path.join(other(side), f'edge/bundle-{name_of(side)}/empty-folder'))}")
    return case


# ---------------------------------------------------------------------
# Group: ops. One side changes; the other follows.
# ---------------------------------------------------------------------

def case_modify(side):
    def case(ctx):
        rel = f"edge/ops-{name_of(side)}/modify.txt"
        f = os.path.join(side, rel)
        w(f, "v1\n")
        yield
        same(ctx, rel, side, what="v1")
        w(f, "v2 is longer than v1\n")
        yield
        same(ctx, rel, side, what="v2, written in place")
        with open(f, "ab") as fh:
            fh.write(b"appended\n")
        yield
        same(ctx, rel, side, what="v3, appended")
        open(f, "wb").close()
        yield
        same(ctx, rel, side, what="v4, truncated to nothing")
        tmp = f + ".1234.5678"
        w(tmp, "v5 by a temporary file and a rename\n")
        os.replace(tmp, f)
        yield
        same(ctx, rel, side, what="v5, replaced by a rename")
        ctx.expect(not exists(os.path.join(other(side), rel + ".1234.5678")), "the temporary name is not on the other side")
    return case


def case_delete(side):
    def case(ctx):
        base = f"edge/ops-{name_of(side)}/del"
        for i in range(8):
            w(os.path.join(side, base, f"keep-{i}.txt"), f"keep {i}\n")
        w(os.path.join(side, base, "gone.txt"), "to delete\n")
        for i in range(5):
            w(os.path.join(side, base, "gone-dir/sub", f"f{i}.txt"), f"{i}\n")
        yield
        same(ctx, f"{base}/gone.txt", side)
        os.remove(os.path.join(side, base, "gone.txt"))
        shutil.rmtree(os.path.join(side, base, "gone-dir"))
        yield
        o = other(side)
        ctx.expect(not exists(os.path.join(o, base, "gone.txt")), "a deleted file is gone on the other side")
        ctx.expect(not exists(os.path.join(o, base, "gone-dir")), "a deleted folder is gone on the other side")
        ctx.expect(exists(os.path.join(o, base, "keep-0.txt")), "the files next to them stay")
    return case


def case_rename(side):
    def case(ctx):
        base = f"edge/ops-{name_of(side)}/ren"
        s = os.path.join(side, base)
        w(os.path.join(s, "old-name.txt"), "rename me\n")
        for i in range(30):
            w(os.path.join(s, "old-dir", f"f{i:02d}.txt"), f"file {i}\n" * 50)
        w(os.path.join(s, "to-move.txt"), "move me\n")
        w(os.path.join(s, "swap-a.txt"), "content A\n")
        w(os.path.join(s, "swap-b.txt"), "content B\n")
        w(os.path.join(s, "Case.txt"), "case\n")
        w(os.path.join(s, "CaseDir/x.txt"), "x\n")
        yield
        os.rename(os.path.join(s, "old-name.txt"), os.path.join(s, "new-name.txt"))
        os.rename(os.path.join(s, "old-dir"), os.path.join(s, "new-dir"))
        os.makedirs(os.path.join(s, "dest/deeper"), exist_ok=True)
        os.rename(os.path.join(s, "to-move.txt"), os.path.join(s, "dest/deeper/moved.txt"))
        os.rename(os.path.join(s, "swap-a.txt"), os.path.join(s, "swap-tmp"))
        os.rename(os.path.join(s, "swap-b.txt"), os.path.join(s, "swap-a.txt"))
        os.rename(os.path.join(s, "swap-tmp"), os.path.join(s, "swap-b.txt"))
        os.rename(os.path.join(s, "Case.txt"), os.path.join(s, "case.txt"))
        os.rename(os.path.join(s, "CaseDir"), os.path.join(s, "casedir"))
        yield
        o = os.path.join(other(side), base)
        ctx.expect(not exists(os.path.join(o, "old-name.txt")) and r(os.path.join(o, "new-name.txt")) == b"rename me\n", "a renamed file")
        ctx.expect(listing(os.path.join(o, "new-dir")) == [f"f{i:02d}.txt" for i in range(30)] and not exists(os.path.join(o, "old-dir")), "a renamed folder of 30 files")
        ctx.expect(r(os.path.join(o, "dest/deeper/moved.txt")) == b"move me\n" and not exists(os.path.join(o, "to-move.txt")), "a file moved into a new folder")
        ctx.expect(r(os.path.join(o, "swap-a.txt")) == b"content B\n" and r(os.path.join(o, "swap-b.txt")) == b"content A\n", "two files that swapped names")
        names = listing(o) or []
        ctx.expect("case.txt" in names and "Case.txt" not in names, f"a rename that changes only the case of a file name (other side has {[n for n in names if n.lower() == 'case.txt']})")
        ctx.expect("casedir" in names and "CaseDir" not in names, f"a rename that changes only the case of a folder name (other side has {[n for n in names if n.lower() == 'casedir']})")
        ctx.expect(not exists(os.path.join(o, "swap-tmp")), "no leftover of the swap")
    return case


def case_fast_sequences(side):
    def case(ctx):
        base = f"edge/ops-{name_of(side)}/fast"
        s = os.path.join(side, base)
        os.makedirs(s, exist_ok=True)
        # A file that lives for a moment.
        w(os.path.join(s, "ephemeral.txt"), "gone at once\n")
        os.remove(os.path.join(s, "ephemeral.txt"))
        # 200 rewrites in a row; the last one has to win.
        for i in range(200):
            w(os.path.join(s, "rewrites.txt"), f"version {i}\n" * (i % 7 + 1))
        # Rename and change in one go.
        w(os.path.join(s, "ren-then-mod.txt"), "before\n")
        yield
        os.rename(os.path.join(s, "ren-then-mod.txt"), os.path.join(s, "renamed-and-changed.txt"))
        w(os.path.join(s, "renamed-and-changed.txt"), "after the rename\n")
        # Delete, then the same name with other content.
        w(os.path.join(s, "reborn.txt"), "first life\n")
        yield
        os.remove(os.path.join(s, "reborn.txt"))
        w(os.path.join(s, "reborn.txt"), "second life, other content\n")
        # A chain of renames.
        w(os.path.join(s, "chain-0.txt"), "chain\n")
        for i in range(10):
            os.rename(os.path.join(s, f"chain-{i}.txt"), os.path.join(s, f"chain-{i + 1}.txt"))
        yield
        o = os.path.join(other(side), base)
        ctx.expect(not exists(os.path.join(o, "ephemeral.txt")), "a file that was created and deleted at once is not on the other side")
        ctx.expect(r(os.path.join(o, "rewrites.txt")) == r(os.path.join(s, "rewrites.txt")), "200 rewrites: the other side has the last one")
        ctx.expect(r(os.path.join(o, "renamed-and-changed.txt")) == b"after the rename\n" and not exists(os.path.join(o, "ren-then-mod.txt")), "a rename followed at once by a change")
        ctx.expect(r(os.path.join(o, "reborn.txt")) == b"second life, other content\n", "a delete followed at once by a new file of that name")
        names = [n for n in (listing(o) or []) if n.startswith("chain-")]
        ctx.expect(names == ["chain-10.txt"], f"a chain of 10 renames leaves one file (other side has {names})")
    return case


def case_type_change(side):
    def case(ctx):
        base = f"edge/ops-{name_of(side)}/type"
        s = os.path.join(side, base)
        w(os.path.join(s, "file-to-dir"), "I am a file\n")
        w(os.path.join(s, "dir-to-file/inner.txt"), "inner\n")
        yield
        os.remove(os.path.join(s, "file-to-dir"))
        w(os.path.join(s, "file-to-dir/now-inside.txt"), "the name is a folder now\n")
        shutil.rmtree(os.path.join(s, "dir-to-file"))
        w(os.path.join(s, "dir-to-file"), "the name is a file now\n")
        yield
        o = os.path.join(other(side), base)
        ctx.expect(r(os.path.join(o, "file-to-dir/now-inside.txt")) == b"the name is a folder now\n", "a file that became a folder")
        ctx.expect(r(os.path.join(o, "dir-to-file")) == b"the name is a file now\n", "a folder that became a file")
        ds = [d for d in decisions() if d.get("kind") == "type-mismatch"]
        ctx.info(f"type-mismatch questions open: {len(ds)}")
    return case


def case_out_and_back(side):
    def case(ctx):
        base = f"edge/ops-{name_of(side)}/outback"
        s = os.path.join(side, base)
        w(os.path.join(s, "same.txt"), "leaves and comes back the same\n")
        w(os.path.join(s, "changed.txt"), "leaves and comes back changed\n")
        for i in range(20):
            w(os.path.join(s, "folder", f"f{i}.txt"), f"{i}\n")
        yield
        stash = os.path.expanduser(f"~/stash-{name_of(side)}")
        shutil.rmtree(stash, ignore_errors=True)
        os.makedirs(stash)
        for n in ("same.txt", "changed.txt", "folder"):
            os.rename(os.path.join(s, n), os.path.join(stash, n))
        yield
        o = os.path.join(other(side), base)
        ctx.expect(not exists(os.path.join(o, "same.txt")) and not exists(os.path.join(o, "folder")), "what moved out of the sync folder is gone on the other side")
        w(os.path.join(stash, "changed.txt"), "changed while it was outside\n")
        for n in ("same.txt", "changed.txt", "folder"):
            os.rename(os.path.join(stash, n), os.path.join(s, n))
        yield
        ctx.expect(r(os.path.join(o, "same.txt")) == b"leaves and comes back the same\n", "a file that came back the same")
        ctx.expect(r(os.path.join(o, "changed.txt")) == b"changed while it was outside\n", "a file that came back changed")
        ctx.expect(listing(os.path.join(o, "folder")) == sorted(f"f{i}.txt" for i in range(20)), "a folder that came back")
        shutil.rmtree(stash, ignore_errors=True)
    return case


def case_touch_chmod(side):
    def case(ctx):
        base = f"edge/ops-{name_of(side)}/meta"
        s = os.path.join(side, base)
        w(os.path.join(s, "touched.txt"), "same bytes\n")
        w(os.path.join(s, "chmodded.sh"), "echo\n", 0o644)
        w(os.path.join(s, "stealth.txt"), "AAAA\n")
        yield
        hw = vt.queue_counts()["hw"]
        o = os.path.join(other(side), base)
        before = os.stat(os.path.join(o, "touched.txt")).st_mtime
        os.utime(os.path.join(s, "touched.txt"), (time.time() + 3600, time.time() + 3600))
        os.chmod(os.path.join(s, "chmodded.sh"), 0o755)
        # Other bytes, same size, same modification time.
        st = os.stat(os.path.join(s, "stealth.txt"))
        with open(os.path.join(s, "stealth.txt"), "r+b") as fh:
            fh.write(b"BBBB\n")
        os.utime(os.path.join(s, "stealth.txt"), ns=(st.st_atime_ns, st.st_mtime_ns))
        yield
        ctx.info(f"a touch with the same bytes: other side's mtime changed={os.stat(os.path.join(o, 'touched.txt')).st_mtime != before}; intents enqueued={vt.queue_counts()['hw'] - hw}")
        ctx.info(f"a chmod alone: other side's mode={oct(os.stat(os.path.join(o, 'chmodded.sh')).st_mode & 0o777)}")
        ctx.expect(r(os.path.join(o, "stealth.txt")) == b"BBBB\n", "new bytes with the old size and the old modification time, while the daemon runs")
        ctx.expect(not [n for n in (listing(o) or []) if "~conflict-" in n], "no conflict copy from a touch or a chmod")
    return case


def case_slow_writer(side):
    def case(ctx):
        rel = f"edge/ops-{name_of(side)}/slow/growing.bin"
        f = os.path.join(side, rel)
        os.makedirs(os.path.dirname(f), exist_ok=True)
        t0 = int(time.time() * 1000)
        with open(f, "wb") as fh:
            for i in range(40):
                fh.write(bytes([i]) * (512 * 1024))
                fh.flush()
                time.sleep(0.5)
        yield
        same(ctx, rel, side, what="a file that grew by 0.5 MiB each half second for 20 s")
        tl = vt.vapor_json("timeline") or {}
        n = 0
        for e in tl.get("events", tl.get("entries", [])):
            if "growing.bin" in json.dumps(e) and int(e.get("timestampMs", e.get("ts", 0)) or 0) >= t0:
                n += 1
        ctx.info(f"timeline entries that name the file since it began to grow: {n}")
    return case


def case_many_small(side):
    def case(ctx):
        base = f"edge/ops-{name_of(side)}/many"
        s = os.path.join(side, base)
        t0 = time.time()
        for d in range(20):
            for i in range(100):
                w(os.path.join(s, f"d{d:02d}", f"f{i:03d}.txt"), f"{d}/{i}\n" * 20)
        ctx.info(f"wrote 2000 files in {time.time() - t0:.1f}s")
        ctx.t0 = time.time()
        yield
        o = os.path.join(other(side), base)
        n = sum(len(fs) for _, _, fs in os.walk(o))
        ctx.expect(n == 2000, f"2000 small files in 20 folders: the other side has {n}")
        w(os.path.join(s, "flat", "seed.txt"), "x")
        for i in range(3000):
            w(os.path.join(s, "flat", f"n{i:05d}.dat"), f"{i}")
        yield
        n = len(listing(os.path.join(o, "flat")) or [])
        ctx.expect(n == 3001, f"3001 files in one folder: the other side has {n}")
    return case


# ---------------------------------------------------------------------
# Group registry
# ---------------------------------------------------------------------

def both(factory, cid, title):
    return [
        (f"{cid}-L", f"{title} (written on this device)", factory(L)),
        (f"{cid}-C", f"{title} (written in the cloud folder)", factory(C)),
    ]


def group_names():
    cases = []
    for side in (L, C):
        tag = "L" if side == L else "C"
        for key, name in NAMES:
            cases.append((f"N-{key}-{tag}", f"name {name!r} from {name_of(side)}", make_name_case(side, key, name)))
    cases += both(case_backslash, "N-backslash", "a name with a backslash")
    cases += both(case_internal_names, "N-internal", "names that Vapor keeps for itself")
    cases += both(case_deep_path, "N-deep", "a path 60 folders deep")
    return cases


def group_types():
    cases = []
    cases += both(case_sizes, "T-sizes", "file sizes around the 8 MiB limit")
    cases += both(case_modes, "T-modes", "file permissions")
    cases += both(case_links, "T-links", "symbolic and hard links")
    cases += both(case_specials, "T-specials", "a FIFO and a socket")
    cases += both(case_xattrs, "T-xattr", "extended attributes and the Finder lock")
    cases += both(case_sparse, "T-sparse", "a sparse file")
    cases += both(case_bundle, "T-bundle", "an app bundle and empty folders")
    return cases


def group_ops():
    cases = []
    cases += both(case_modify, "O-modify", "ways to change a file")
    cases += both(case_delete, "O-delete", "delete a file and a folder")
    cases += both(case_rename, "O-rename", "renames and moves")
    cases += both(case_fast_sequences, "O-fast", "fast sequences on one name")
    cases += both(case_type_change, "O-type", "a file becomes a folder and back")
    cases += both(case_out_and_back, "O-outback", "out of the sync folder and back")
    cases += both(case_touch_chmod, "O-meta", "touch, chmod, and a stealth change")
    return cases


def group_bulk():
    cases = []
    cases += both(case_slow_writer, "B-slow", "a file that grows slowly")
    cases += both(case_many_small, "B-many", "thousands of small files")
    return cases


GROUPS = {"names": group_names, "types": group_types, "ops": group_ops, "bulk": group_bulk}


def run_group(name, only=None, timeout=600, quiet=10):
    cases = GROUPS[name]()
    if only:
        cases = [c for c in cases if any(c[0].startswith(o) for o in only)]
    skip = [o for o in os.environ.get("EDGE_SKIP", "").split(",") if o]
    cases = [c for c in cases if not any(c[0].startswith(o) for o in skip)]
    live = []
    for cid, title, factory in cases:
        ctx = Ctx(cid, title)
        try:
            gen = factory(ctx)
        except Exception as e:  # noqa: BLE001
            ctx.fails.append(f"setup raised {e!r}")
            gen = None
        live.append((ctx, gen))
    start_ms = int(time.time() * 1000)
    step = 0
    active = [(ctx, gen) for ctx, gen in live if gen is not None]
    while active:
        nxt = []
        for ctx, gen in active:
            try:
                next(gen)
                nxt.append((ctx, gen))
            except StopIteration:
                pass
            except Exception as e:  # noqa: BLE001
                ctx.fails.append(f"raised {type(e).__name__}: {e}")
        active = nxt
        if not active:
            break
        step += 1
        ok, secs, st = vt.wait_quiet(timeout, quiet, verbose=True, label=f" {name} step {step}")
        if not ok:
            for ctx, _ in active:
                ctx.fails.append(f"the daemon was not quiet {int(secs)}s after step {step}: {json.dumps(st)}")
    out = []
    for ctx, _ in live:
        status = "FAIL" if ctx.fails else "PASS"
        out.append({"id": ctx.id, "title": ctx.title, "status": status, "fails": ctx.fails, "infos": ctx.infos, "passed": len(ctx.notes)})
    report = {
        "group": name,
        "cases": out,
        "passed": sum(1 for c in out if c["status"] == "PASS"),
        "failed": sum(1 for c in out if c["status"] == "FAIL"),
        "status": vt.status_line(),
        "log": vt.logscan(start_ms),
        "decisions": decisions(),
        "compare": vt.compare(sub="edge", max_list=30),
    }
    os.makedirs(vt.OUT, exist_ok=True)
    path = os.path.join(vt.OUT, f"edge-{name}-{int(time.time())}.json")
    with open(path, "w") as fh:
        json.dump(report, fh, indent=1, ensure_ascii=False)
    for c in out:
        if c["status"] == "FAIL":
            print(f"FAIL {c['id']}: {c['title']}")
            for f in c["fails"]:
                print(f"     - {f}")
        for i in c["infos"]:
            print(f"info {c['id']}: {i}")
    print(f"== {name}: {report['passed']} passed, {report['failed']} failed; report {path}")
    print("status:", json.dumps(report["status"]))
    print("decisions:", json.dumps([{k: d.get(k) for k in ("id", "kind", "path", "question")} for d in report["decisions"]], ensure_ascii=False))
    print("log:", json.dumps(report["log"], ensure_ascii=False)[:3000])
    cmp_ = report["compare"]
    print("compare:", json.dumps({k: v for k, v in cmp_.items() if k != "samples"}))
    if not cmp_["converged"] or cmp_["conflict_copies"]:
        print("compare samples:", json.dumps(cmp_["samples"], ensure_ascii=False)[:3000])


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("list")
    rp = sub.add_parser("run")
    rp.add_argument("group")
    rp.add_argument("--only", default="")
    rp.add_argument("--timeout", type=float, default=600)
    rp.add_argument("--quiet", type=float, default=10)
    a = ap.parse_args()
    if a.cmd == "list":
        for g, fn in GROUPS.items():
            for cid, title, _ in fn():
                print(g, cid, title)
        return
    run_group(a.group, [o for o in a.only.split(",") if o], a.timeout, a.quiet)


if __name__ == "__main__":
    main()
