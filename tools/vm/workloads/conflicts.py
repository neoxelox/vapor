#!/usr/bin/env python3
"""conflicts: two-sided cases, one after the other.

Each case builds a small state, lets it sync, then changes both sides
(with the daemon paused, stopped, or running) and checks that no bytes
were lost and that a conflict copy appears only when both sides really
changed.

  conflicts.py list
  conflicts.py run [--only id,id]
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import vt  # noqa: E402
from edge import Ctx, L, C, w, r, exists, listing, decisions, sh  # noqa: E402

BASE = "edge/cx" + os.environ.get("EDGE_TAG", "")


def settle(timeout=180, quiet=8):
    return vt.wait_quiet(timeout, quiet, verbose=False)


def pause():
    vt.vapor("pause")


def resume():
    vt.vapor("resume")


def all_bytes(root, rel_dir):
    """name -> bytes for every file under a folder."""
    out = {}
    d = os.path.join(root, rel_dir)
    for base, _, fnames in os.walk(d):
        for f in fnames:
            if vt.is_internal(f):
                continue
            p = os.path.join(base, f)
            out[os.path.relpath(p, d)] = r(p)
    return out


def copies(root, rel_dir):
    return sorted(n for n in all_bytes(root, rel_dir) if "~conflict-" in n)


def both_equal(ctx, rel_dir):
    a, b = all_bytes(L, rel_dir), all_bytes(C, rel_dir)
    ok = a == b
    if not ok:
        ctx.fails.append(f"the two sides differ under {rel_dir}: local={sorted(a)} cloud={sorted(b)}; differing={[n for n in set(a) & set(b) if a[n] != b[n]]}")
    return ok


def survives(ctx, rel_dir, payload, what):
    """`payload` is in some file on both sides."""
    a = [n for n, d in all_bytes(L, rel_dir).items() if d == payload]
    b = [n for n, d in all_bytes(C, rel_dir).items() if d == payload]
    ctx.expect(bool(a) and bool(b), f"{what} survives on both sides (local: {a}, cloud: {b})")
    return a, b


def phantom(kind, origin, editor):
    """A file from `origin`. The `editor` side edits it while the daemon is
    paused; the other side gets an event that changes no bytes."""
    def case(ctx):
        d = f"{BASE}/phantom-{kind}-{'L' if origin == L else 'C'}origin-{'L' if editor == L else 'C'}edit"
        rel = f"{d}/doc.txt"
        w(os.path.join(origin, rel), "version one, the synced bytes\n")
        settle()
        other = C if editor == L else L
        if r(os.path.join(other, rel)) is None or r(os.path.join(editor, rel)) is None:
            ctx.fails.append("setup: the file did not sync")
            return
        time.sleep(1.2)
        pause()
        edit = b"version two, the only real edit\n"
        w(os.path.join(editor, rel), edit)
        target = os.path.join(other, rel)
        if kind == "chmod":
            os.chmod(target, 0o600)
        elif kind == "touch":
            now = time.time() + 5
            os.utime(target, (now, now))
        elif kind == "xattr":
            sh(f"xattr -w user.note hello '{target}'")
        elif kind == "samebytes":
            w(target, "version one, the synced bytes\n")
        elif kind == "chflags":
            sh(f"chflags uchg '{target}' && chflags nouchg '{target}'")
        time.sleep(0.5)
        resume()
        settle()
        cl, cc = copies(L, d), copies(C, d)
        ctx.expect(not cl and not cc, f"no conflict copy (local: {cl}, cloud: {cc})")
        ctx.expect(r(os.path.join(L, rel)) == edit, f"this device holds the edit under the name (holds {r(os.path.join(L, rel))!r})")
        ctx.expect(r(os.path.join(C, rel)) == edit, f"the cloud holds the edit under the name (holds {r(os.path.join(C, rel))!r})")
    return case


def true_conflict(mode):
    def case(ctx):
        d = f"{BASE}/both-edit-{mode}"
        rel = f"{d}/doc.txt"
        w(os.path.join(L, rel), "base\n")
        settle()
        la, cb = b"edit made on this device\n", b"edit made in the cloud, longer\n"
        if mode == "paused":
            pause()
        elif mode == "stopped":
            vt.vapor("service", "stop")
            time.sleep(1)
        w(os.path.join(L, rel), la)
        w(os.path.join(C, rel), cb)
        if mode == "paused":
            resume()
        elif mode == "stopped":
            vt.vapor("service", "start")
            time.sleep(3)
        settle()
        survives(ctx, d, la, "the edit of this device")
        survives(ctx, d, cb, "the edit of the cloud")
        both_equal(ctx, d)
        n = len(copies(L, d))
        ctx.expect(n == 1, f"one conflict copy ({n})")
        ds = [x for x in decisions() if x.get("kind") == "conflict" and d in (x.get("path") or "")]
        ctx.expect(len(ds) == 1, f"one conflict question for the file ({len(ds)})")
    return case


def same_content(ctx):
    d = f"{BASE}/same-content"
    rel = f"{d}/doc.txt"
    w(os.path.join(L, rel), "base\n")
    settle()
    pause()
    w(os.path.join(L, rel), "both sides wrote exactly this\n")
    w(os.path.join(C, rel), "both sides wrote exactly this\n")
    w(os.path.join(L, d, "new.txt"), "a new file with one content on both sides\n")
    w(os.path.join(C, d, "new.txt"), "a new file with one content on both sides\n")
    resume()
    settle()
    ctx.expect(not copies(L, d) and not copies(C, d), f"no conflict copy when both sides wrote the same bytes ({copies(L, d)})")
    both_equal(ctx, d)


def delete_vs_edit(deleter):
    def case(ctx):
        tag = "L" if deleter == L else "C"
        d = f"{BASE}/delete-{tag}-vs-edit"
        rel = f"{d}/doc.txt"
        w(os.path.join(L, rel), "base\n")
        w(os.path.join(L, d, "bystander.txt"), "stays\n")
        settle()
        editor = C if deleter == L else L
        pause()
        os.remove(os.path.join(deleter, rel))
        edit = b"edited on the other side while it was deleted here\n"
        w(os.path.join(editor, rel), edit)
        resume()
        settle()
        ctx.expect(r(os.path.join(L, rel)) == edit and r(os.path.join(C, rel)) == edit, f"the edit wins over the deletion on both sides (local {r(os.path.join(L, rel))!r}, cloud {r(os.path.join(C, rel))!r})")
        both_equal(ctx, d)
    return case


def both_delete(ctx):
    d = f"{BASE}/both-delete"
    for i in range(5):
        w(os.path.join(L, d, f"f{i}.txt"), f"{i}\n")
    settle()
    pause()
    for i in range(3):
        os.remove(os.path.join(L, d, f"f{i}.txt"))
        os.remove(os.path.join(C, d, f"f{i}.txt"))
    resume()
    ok, _, st = settle()
    ctx.expect(sorted(all_bytes(L, d)) == ["f3.txt", "f4.txt"] and sorted(all_bytes(C, d)) == ["f3.txt", "f4.txt"], "files deleted on both sides stay deleted, the others stay")
    ctx.expect(not st.get("stuck"), f"nothing is left in the queue ({st.get('stuck')})")


def rename_vs_edit(ctx):
    d = f"{BASE}/rename-vs-edit"
    w(os.path.join(L, d, "old.txt"), "base content\n")
    settle()
    pause()
    os.rename(os.path.join(L, d, "old.txt"), os.path.join(L, d, "new.txt"))
    edit = b"cloud edited the file under its old name\n"
    w(os.path.join(C, d, "old.txt"), edit)
    resume()
    settle()
    survives(ctx, d, edit, "the cloud's edit")
    both_equal(ctx, d)
    ctx.info(f"layout after a rename here and an edit there: {sorted(all_bytes(L, d))}")


def rename_both(ctx):
    d = f"{BASE}/rename-both"
    w(os.path.join(L, d, "a.txt"), "one file, two new names\n")
    settle()
    pause()
    os.rename(os.path.join(L, d, "a.txt"), os.path.join(L, d, "b.txt"))
    os.rename(os.path.join(C, d, "a.txt"), os.path.join(C, d, "c.txt"))
    resume()
    settle()
    survives(ctx, d, b"one file, two new names\n", "the content")
    both_equal(ctx, d)
    ctx.info(f"layout after two different renames: {sorted(all_bytes(L, d))}")


def create_both(ctx):
    d = f"{BASE}/create-both"
    w(os.path.join(L, d, "seed.txt"), "seed\n")
    settle()
    pause()
    la, cb = b"new file, written here\n", b"new file, written in the cloud\n"
    w(os.path.join(L, d, "new.txt"), la)
    w(os.path.join(C, d, "new.txt"), cb)
    resume()
    settle()
    survives(ctx, d, la, "this device's new file")
    survives(ctx, d, cb, "the cloud's new file")
    both_equal(ctx, d)


def deldir_vs_add(ctx):
    d = f"{BASE}/deldir-vs-add"
    for i in range(10):
        w(os.path.join(L, d, "folder", f"f{i}.txt"), f"{i}\n")
    w(os.path.join(L, d, "keep.txt"), "keep\n")
    settle()
    pause()
    shutil.rmtree(os.path.join(L, d, "folder"))
    new = b"added in the cloud to a folder that was deleted here\n"
    w(os.path.join(C, d, "folder", "added.txt"), new)
    resume()
    settle()
    survives(ctx, d, new, "the file added in the cloud")
    both_equal(ctx, d)
    ctx.info(f"layout: {sorted(all_bytes(L, d))}")


def mvdir_vs_edit(ctx):
    d = f"{BASE}/mvdir-vs-edit"
    for i in range(10):
        w(os.path.join(L, d, "folder", f"f{i}.txt"), f"original {i}\n")
    settle()
    pause()
    os.rename(os.path.join(L, d, "folder"), os.path.join(L, d, "renamed"))
    edit = b"edited in the cloud inside the folder that was renamed here\n"
    w(os.path.join(C, d, "folder", "f3.txt"), edit)
    resume()
    settle()
    survives(ctx, d, edit, "the cloud's edit")
    both_equal(ctx, d)
    ctx.info(f"layout: {sorted(all_bytes(L, d))}")


def file_vs_dir(ctx):
    d = f"{BASE}/file-vs-dir"
    w(os.path.join(L, d, "seed.txt"), "seed\n")
    settle()
    pause()
    w(os.path.join(L, d, "thing"), "a file named thing\n")
    w(os.path.join(C, d, "thing", "inner.txt"), "a folder named thing\n")
    resume()
    ok, _, st = settle()
    ds = [x for x in decisions() if x.get("kind") == "type-mismatch" and d in (x.get("path") or "")]
    ctx.expect(len(ds) == 1, f"a type-mismatch question is open ({len(ds)})")
    ctx.expect(r(os.path.join(L, d, "thing")) == b"a file named thing\n" and r(os.path.join(C, d, "thing", "inner.txt")) == b"a folder named thing\n", "both sides stay untouched")
    ctx.info(f"queue while the question is open: {st.get('stuck')}")


def ping_pong(ctx):
    d = f"{BASE}/ping-pong"
    rel = f"{d}/doc.txt"
    w(os.path.join(L, rel), "start\n")
    settle()
    last = None
    for i in range(15):
        side = L if i % 2 == 0 else C
        last = f"turn {i} on {'this device' if side == L else 'the cloud'}\n".encode()
        w(os.path.join(side, rel), last)
        time.sleep(0.4)
    settle()
    survives(ctx, d, last, "the last write")
    both_equal(ctx, d)
    ctx.info(f"conflict copies after 15 alternating writes 0.4 s apart: {len(copies(L, d))}")
    ctx.info(f"canonical holds: {r(os.path.join(L, rel))!r}")


def ping_pong_slow(ctx):
    d = f"{BASE}/ping-pong-slow"
    rel = f"{d}/doc.txt"
    w(os.path.join(L, rel), "start\n")
    settle()
    last = None
    for i in range(8):
        side = L if i % 2 == 0 else C
        last = f"turn {i} on {'this device' if side == L else 'the cloud'}\n".encode()
        w(os.path.join(side, rel), last)
        time.sleep(6)
    settle()
    ctx.expect(r(os.path.join(L, rel)) == last and r(os.path.join(C, rel)) == last, f"alternating writes 6 s apart: both sides hold the last one (local {r(os.path.join(L, rel))!r})")
    ctx.expect(not copies(L, d), f"no conflict copy for writes that had time to sync ({copies(L, d)})")


def race_now(ctx):
    d = f"{BASE}/race-now"
    rel = f"{d}/doc.txt"
    w(os.path.join(L, rel), "start\n")
    settle()
    la, cb = b"written here at the same instant\n", b"written in the cloud at the same instant!\n"
    w(os.path.join(L, rel), la)
    w(os.path.join(C, rel), cb)
    settle()
    survives(ctx, d, la, "this device's write")
    survives(ctx, d, cb, "the cloud's write")
    both_equal(ctx, d)


def edit_during_download(ctx):
    d = f"{BASE}/edit-during-download"
    rel = f"{d}/big.bin"
    w(os.path.join(L, rel), b"small start\n")
    settle()
    big = os.urandom(1 << 20) * 300
    w(os.path.join(C, rel), big)
    # The download of 300 MiB takes a moment; the local file changes meanwhile.
    time.sleep(1.5)
    la = b"a local edit made while the cloud's version was coming down\n"
    w(os.path.join(L, rel), la)
    settle(300)
    survives(ctx, d, la, "the local edit")
    a = [n for n, data in all_bytes(L, d).items() if data == big]
    b = [n for n, data in all_bytes(C, d).items() if data == big]
    ctx.expect(bool(a) and bool(b), f"the cloud's 300 MiB version survives on both sides (local {a}, cloud {b})")
    both_equal(ctx, d)
    tmp = [n for n in os.listdir(os.path.join(L, d)) if n.startswith(".vapor-tmp")]
    ctx.expect(not tmp, f"no temporary file is left next to it ({tmp})")
    for side in (L, C):
        for n in os.listdir(os.path.join(side, d)):
            os.remove(os.path.join(side, d, n))


def delete_during_upload(ctx):
    d = f"{BASE}/delete-during-upload"
    rel = f"{d}/big.bin"
    w(os.path.join(L, d, "seed.txt"), "seed\n")
    settle()
    w(os.path.join(L, rel), os.urandom(1 << 20) * 300)
    time.sleep(2.0)
    os.remove(os.path.join(L, rel))
    ok, _, st = settle(300)
    left = os.listdir(os.path.join(C, d))
    ctx.expect(left == ["seed.txt"] or sorted(left) == ["seed.txt"], f"a file deleted while it uploaded leaves nothing in the cloud ({left})")
    ctx.expect(not exists(os.path.join(L, rel)), "and it does not come back on this device")
    ctx.expect(not st.get("stuck"), f"nothing is left in the queue ({st.get('stuck')})")


def resolve_flow(ctx):
    d = f"{BASE}/resolve"
    rel = f"{d}/doc.txt"
    w(os.path.join(L, rel), "base\n")
    settle()
    pause()
    la, cb = b"mine\n", b"theirs, from the cloud\n"
    w(os.path.join(L, rel), la)
    w(os.path.join(C, rel), cb)
    resume()
    settle()
    ds = [x for x in decisions() if x.get("kind") == "conflict" and d in (x.get("path") or "")]
    if not ctx.expect(len(ds) == 1, f"one question ({len(ds)})"):
        return
    did = ds[0]["id"]
    cps = copies(L, d)
    if not ctx.expect(len(cps) == 1, f"one copy ({cps})"):
        return
    keep = r(os.path.join(L, d, cps[0]))
    code, out, err = vt.vapor("decisions", "resolve", str(did), "--choose", cps[0])
    ctx.expect(code == 0, f"`vapor decisions resolve` exits 0 ({code}: {(out + err).strip()[:200]})")
    settle()
    ctx.expect(r(os.path.join(L, rel)) == keep and r(os.path.join(C, rel)) == keep, f"the chosen version is under the name on both sides (local {r(os.path.join(L, rel))!r}, cloud {r(os.path.join(C, rel))!r})")
    ctx.expect(not copies(L, d) and not copies(C, d), f"no copy is left (local {copies(L, d)}, cloud {copies(C, d)})")
    t = vt.vapor_json("trash", "list") or {}
    items = [e for e in t.get("entries", t.get("items", [])) if "resolve" in json.dumps(e)]
    ctx.expect(bool(items), f"the version that was not kept is in the trash ({len(items)} entries)")
    left = [x for x in decisions() if x.get("id") == did]
    ctx.expect(not left, "the question is closed")


def cases():
    out = []
    for kind in ("chmod", "touch", "xattr", "samebytes", "chflags"):
        for origin in (L, C):
            for editor in (L, C):
                o, e = ("L" if origin == L else "C"), ("L" if editor == L else "C")
                out.append((f"X-phantom-{kind}-{o}{e}", f"a {kind} with no new bytes on the other side while an edit waits (file from {o}, edit on {e})", phantom(kind, origin, editor)))
    out += [
        ("X-both-paused", "both sides edit while paused", true_conflict("paused")),
        ("X-both-stopped", "both sides edit while the daemon is stopped", true_conflict("stopped")),
        ("X-both-live", "both sides edit while the daemon runs", true_conflict("live")),
        ("X-same-content", "both sides write the same bytes", same_content),
        ("X-del-L-edit-C", "deleted here, edited in the cloud", delete_vs_edit(L)),
        ("X-del-C-edit-L", "deleted in the cloud, edited here", delete_vs_edit(C)),
        ("X-both-delete", "both sides delete", both_delete),
        ("X-rename-vs-edit", "renamed here, edited in the cloud", rename_vs_edit),
        ("X-rename-both", "renamed on both sides to different names", rename_both),
        ("X-create-both", "the same new name on both sides", create_both),
        ("X-deldir-vs-add", "a folder deleted here gets a new file in the cloud", deldir_vs_add),
        ("X-mvdir-vs-edit", "a folder renamed here has a file edited in the cloud", mvdir_vs_edit),
        ("X-file-vs-dir", "a file here and a folder in the cloud under one new name", file_vs_dir),
        ("X-ping-pong", "alternating writes 0.4 s apart", ping_pong),
        ("X-ping-slow", "alternating writes 6 s apart", ping_pong_slow),
        ("X-race-now", "two writes at the same instant", race_now),
        ("X-edit-during-download", "a local edit while a 300 MiB download is in flight", edit_during_download),
        ("X-delete-during-upload", "a delete while a 300 MiB upload is in flight", delete_during_upload),
        ("X-resolve", "answer a conflict from the command line", resolve_flow),
    ]
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("cmd", choices=["list", "run"])
    ap.add_argument("--only", default="")
    a = ap.parse_args()
    cs = cases()
    if a.cmd == "list":
        for cid, title, _ in cs:
            print(cid, title)
        return
    only = [o for o in a.only.split(",") if o]
    if only:
        cs = [c for c in cs if any(c[0].startswith(o) for o in only)]
    start_ms = int(time.time() * 1000)
    out = []
    for cid, title, fn in cs:
        ctx = Ctx(cid, title)
        t0 = time.time()
        try:
            fn(ctx)
        except Exception as e:  # noqa: BLE001
            ctx.fails.append(f"raised {type(e).__name__}: {e}")
            resume()
        status = "FAIL" if ctx.fails else "PASS"
        out.append({"id": cid, "title": title, "status": status, "fails": ctx.fails, "infos": ctx.infos, "notes": ctx.notes, "seconds": round(time.time() - t0, 1)})
        print(f"{status} {cid} ({time.time() - t0:.0f}s): {title}", flush=True)
        for f in ctx.fails:
            print(f"     - {f}", flush=True)
        for i in ctx.infos:
            print(f"     i {i}", flush=True)
    report = {"cases": out, "passed": sum(1 for c in out if c["status"] == "PASS"), "failed": sum(1 for c in out if c["status"] == "FAIL"), "log": vt.logscan(start_ms), "status": vt.status_line()}
    path = os.path.join(vt.OUT, f"conflicts-{int(time.time())}.json")
    with open(path, "w") as fh:
        json.dump(report, fh, indent=1, ensure_ascii=False)
    print(f"== conflicts: {report['passed']} passed, {report['failed']} failed; report {path}")
    print("log:", json.dumps(report["log"], ensure_ascii=False)[:2500])


if __name__ == "__main__":
    main()
