#!/usr/bin/env python3
"""drive_tests: two local folders that sync through one Google Drive
folder, as two devices do. Folder A belongs to profile `default`, folder
B to profile `second`. A case writes in one folder and waits until the
other folder holds the same bytes.

  drive_tests.py list
  drive_tests.py run [--only id,id]
"""
import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import vt  # noqa: E402

HOME = os.path.expanduser("~")
A = os.environ.get("DRIVE_A", HOME + "/VaporDrive")
B = os.environ.get("DRIVE_B", HOME + "/VaporDrive2")
RESULTS = []


def w(path, data):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    if isinstance(data, str):
        data = data.encode()
    with open(path, "wb") as fh:
        fh.write(data)


def r(path):
    try:
        with open(path, "rb") as fh:
            return fh.read()
    except OSError:
        return None


def tree(root, sub=""):
    out = {}
    base = os.path.join(root, sub)
    for d, dnames, fnames in os.walk(base):
        dnames[:] = [x for x in dnames if x != ".vapor"]
        for f in fnames:
            if vt.is_internal(f):
                continue
            p = os.path.join(d, f)
            if os.path.islink(p) or not os.path.isfile(p):
                continue
            data = r(p)
            # A file that cannot be read never compares equal to another.
            out[os.path.relpath(p, base)] = hashlib.sha256(data).hexdigest() if data is not None else f"unreadable:{p}"
    return out


def wait_equal(sub, timeout=600, expect=None):
    """Waits until both folders hold the same files under `sub` (and, with
    `expect`, exactly that set of names). Returns seconds or None."""
    t0 = time.time()
    while time.time() - t0 < timeout:
        ta, tb = tree(A, sub), tree(B, sub)
        if ta == tb and (expect is None or sorted(ta) == sorted(expect)):
            return time.time() - t0
        time.sleep(1.0)
    return None


def wait_settled(sub, quiet=30, timeout=900):
    """Waits until both folders hold the same files under `sub` and
    nothing changed for `quiet` seconds. Returns whether they did. Two
    folders that are equal for a moment are not the end of a conflict:
    the copy of the other version comes seconds after the file."""
    t0, last, since = time.time(), None, time.time()
    while time.time() - t0 < timeout:
        ta, tb = tree(A, sub), tree(B, sub)
        if (ta, tb) != last:
            last, since = (ta, tb), time.time()
        elif ta == tb and time.time() - since >= quiet:
            return True
        time.sleep(2)
    return False


def diff(sub):
    ta, tb = tree(A, sub), tree(B, sub)
    return {
        "only_A": sorted(set(ta) - set(tb))[:8],
        "only_B": sorted(set(tb) - set(ta))[:8],
        "differ": sorted(k for k in set(ta) & set(tb) if ta[k] != tb[k])[:8],
        "A": len(ta), "B": len(tb),
    }


def status():
    s = vt.vapor_json("status") or {}
    return {p["id"]: {"run": p["run"]["state"], "queued": p["queued"], "failed": p["failed"], "decisions": p["decisions"]} for p in s.get("profiles", [])}


class Ctx:
    def __init__(self, cid, title):
        self.id, self.title, self.fails, self.infos = cid, title, [], []

    def expect(self, cond, msg):
        if not cond:
            self.fails.append(msg)
        return cond

    def info(self, msg):
        self.infos.append(msg)


def d_first(ctx):
    sub = "t-first"
    for i in range(30):
        w(os.path.join(A, sub, f"note_{i:02d}.md"), f"# note {i}\n" + "text\n" * 50)
    w(os.path.join(A, sub, "blob-20mb.bin"), os.urandom(1 << 20) * 20)
    t = wait_equal(sub, 900)
    ctx.expect(t is not None, f"31 files (20 MiB in one) reach the other device ({diff(sub)})")
    ctx.info(f"A -> Drive -> B: {t and round(t)} s for 31 files, 20 MiB")


def d_latency(ctx):
    sub = "t-latency"
    w(os.path.join(A, sub, "seed.txt"), "seed\n")
    wait_equal(sub, 300)
    up, down = [], []
    for i in range(6):
        data = f"save {i} at {time.time()}\n".encode()
        t0 = time.time()
        w(os.path.join(A, sub, "doc.txt"), data)
        while time.time() - t0 < 300 and r(os.path.join(B, sub, "doc.txt")) != data:
            time.sleep(0.2)
        up.append(round(time.time() - t0, 1))
        data = f"save {i} from B at {time.time()}\n".encode()
        t0 = time.time()
        w(os.path.join(B, sub, "new_from_b_%d.txt" % i), data)
        while time.time() - t0 < 300 and r(os.path.join(A, sub, "new_from_b_%d.txt" % i)) != data:
            time.sleep(0.2)
        down.append(round(time.time() - t0, 1))
    ctx.info(f"one saved file, device to device, seconds: edits A->B {up}; new files B->A {down}")
    ctx.expect(max(up + down) < 300, "every save arrives within 5 minutes")


NAMES = ["plain.txt", "with space.txt", "café.txt", "emoji 🔥.txt", "日本語.txt", "a:b.txt", 'quote".txt', "star*.txt", "ques?.txt",
         "trailing dot.", "trailing space ", ".hidden", "pipe|.txt", "<angle>.txt", "percent%20.txt", "hash#.txt", "L" * 200 + ".txt", "UPPER.TXT"]


def d_names(ctx):
    sub = "t-names"
    for n in NAMES:
        w(os.path.join(A, sub, n), f"name {n}\n")
    t = wait_equal(sub, 900)
    d = diff(sub)
    ctx.expect(t is not None, f"{len(NAMES)} unusual names reach the other device (missing there: {d['only_A']})")
    ctx.info(f"names: {t and round(t)} s")


def d_ops(ctx):
    sub = "t-ops"
    for i in range(10):
        w(os.path.join(A, sub, "dir", f"f{i}.txt"), f"file {i}\n" * 20)
    w(os.path.join(A, sub, "rename-me.txt"), "rename\n")
    w(os.path.join(A, sub, "delete-me.txt"), "delete\n")
    w(os.path.join(A, sub, "Case.txt"), "case\n")
    w(os.path.join(A, sub, "edit.txt"), "v1\n")
    ctx.expect(wait_equal(sub, 600) is not None, f"setup syncs ({diff(sub)})")
    os.rename(os.path.join(A, sub, "rename-me.txt"), os.path.join(A, sub, "renamed.txt"))
    os.remove(os.path.join(A, sub, "delete-me.txt"))
    os.rename(os.path.join(A, sub, "dir"), os.path.join(A, sub, "dir-renamed"))
    os.rename(os.path.join(A, sub, "Case.txt"), os.path.join(A, sub, "case.txt"))
    w(os.path.join(A, sub, "edit.txt"), "v2, edited\n")
    expect = ["renamed.txt", "case.txt", "edit.txt"] + [f"dir-renamed/f{i}.txt" for i in range(10)]
    t = wait_equal(sub, 900, expect=expect)
    ctx.expect(t is not None, f"rename, delete, folder rename, capitals, edit from A reach B (B has {sorted(tree(B, sub))[:16]})")
    ctx.info(f"ops A->B: {t and round(t)} s")
    names_b = os.listdir(os.path.join(B, sub))
    ctx.expect("case.txt" in names_b and "Case.txt" not in names_b, f"the capitals of a name follow ({[n for n in names_b if n.lower() == 'case.txt']})")
    # and back: B changes, A follows
    os.rename(os.path.join(B, sub, "dir-renamed"), os.path.join(B, sub, "dir-again"))
    os.remove(os.path.join(B, sub, "renamed.txt"))
    w(os.path.join(B, sub, "edit.txt"), "v3, edited on B\n")
    expect = ["case.txt", "edit.txt"] + [f"dir-again/f{i}.txt" for i in range(10)]
    t = wait_equal(sub, 900, expect=expect)
    ctx.expect(t is not None, f"folder rename, delete, edit from B reach A (A has {sorted(tree(A, sub))[:16]})")
    ctx.info(f"ops B->A: {t and round(t)} s")
    for root in (A, B):
        left = [d for d in os.listdir(os.path.join(root, sub)) if os.path.isdir(os.path.join(root, sub, d))]
        ctx.info(f"folders in {'A' if root == A else 'B'} at the end: {left}")


def d_conflict(ctx):
    sub = "t-conflict"
    w(os.path.join(A, sub, "doc.txt"), "base\n")
    ctx.expect(wait_equal(sub, 600) is not None, "setup syncs")
    vt.vapor("pause")
    la, lb = b"edited on device A\n", b"edited on device B, longer\n"
    w(os.path.join(A, sub, "doc.txt"), la)
    w(os.path.join(B, sub, "doc.txt"), lb)
    vt.vapor("resume")
    t0 = time.time()
    settled = wait_settled(sub)
    ta = {k: r(os.path.join(A, sub, k)) for k in tree(A, sub)}
    ctx.expect(settled, f"both devices end with the same files ({diff(sub)})")
    ctx.expect(la in ta.values() and lb in ta.values(), f"both edits survive ({sorted(ta)})")
    ctx.info(f"files after a conflict: {sorted(ta)}; {round(time.time() - t0)} s with the wait for quiet")


def wait_burst(sub, sender="default", timeout=1800):
    """Waits until both folders hold the same files under `sub`. Also
    says when the queue of the sending profile was last seen to go
    empty before that: about when the uploads were done, so the upload
    and the download can be told apart. Returns (upload seconds or
    None, end-to-end seconds or None)."""
    t0, uploaded, had_work = time.time(), None, False
    while time.time() - t0 < timeout:
        queued = status().get(sender, {}).get("queued", 0)
        if queued > 0:
            had_work, uploaded = True, None
        elif had_work and uploaded is None:
            uploaded = time.time() - t0
        ta, tb = tree(A, sub), tree(B, sub)
        if ta and ta == tb:
            return uploaded, time.time() - t0
        time.sleep(1.0)
    return uploaded, None


def d_burst(ctx):
    sub = "t-burst"
    t0 = time.time()
    for i in range(400):
        w(os.path.join(A, sub, f"pkg{i % 20:02d}", f"mod_{i:03d}.js"), f"export const v{i} = {i};\n" * 30)
    up, t = wait_burst(sub)
    ctx.expect(t is not None, f"400 small files reach the other device ({diff(sub)})")
    ctx.info(f"400 files, A -> Drive -> B: {t and round(t)} s ({t and round(400 / t, 1)} files/s end to end); "
             f"the queue of the sending profile was empty after {up and round(up)} s ({up and round(400 / up, 1)} uploads/s)")
    st = status()
    ctx.info(f"status after the burst: {st}")
    shutil.rmtree(os.path.join(A, sub))
    time.sleep(40)
    ds = (vt.vapor_json("decisions", "list") or {}).get("decisions", [])
    ctx.info(f"after `rm -rf` of the 400 files: open questions {[(d.get('kind'), d.get('question', '')[:90]) for d in ds]}")
    for d in ds:
        if d.get("kind") == "mass-delete":
            vt.vapor("decisions", "resolve", str(d["id"]), "--choose", "apply", "--profile", str(d.get("profileId", "default")))
    t0 = time.time()
    while time.time() - t0 < 900 and os.path.exists(os.path.join(B, sub)) and tree(B, sub):
        time.sleep(2)
    ctx.expect(not tree(B, sub), f"the deletion reaches the other device ({len(tree(B, sub))} files left there)")
    ctx.info(f"deletion of 400 files, A -> Drive -> B: {round(time.time() - t0)} s after the answer")


def d_git(ctx):
    sub = "t-git"
    repo = os.path.join(A, sub)
    os.makedirs(repo, exist_ok=True)
    g = lambda *a: subprocess.run(["git", "-C", repo, "-c", "user.email=a@example.com", "-c", "user.name=A", *a], capture_output=True, text=True)  # noqa: E731
    subprocess.run(["git", "init", "-q", "-b", "main", repo])
    for i in range(40):
        w(os.path.join(repo, "src", f"m{i}.ts"), f"export const m{i} = {i};\n" * 10)
    g("add", "-A"); g("commit", "-q", "-m", "one")
    for i in range(10):
        w(os.path.join(repo, "src", f"m{i}.ts"), f"export const m{i} = {i * 2};\n" * 12)
    g("add", "-A"); g("commit", "-q", "-m", "two")
    t = wait_equal(sub, 1500)
    ctx.expect(t is not None, f"a git repository reaches the other device ({diff(sub)})")
    gb = lambda *a: subprocess.run(["git", "--no-optional-locks", "-C", os.path.join(B, sub), *a], capture_output=True, text=True)  # noqa: E731
    st, fsck, head = gb("status", "--porcelain"), gb("fsck", "--full"), gb("rev-parse", "HEAD")
    ctx.expect(st.returncode == 0 and not st.stdout.strip(), f"`git status` is clean on the other device ({st.returncode}: {(st.stdout + st.stderr)[:200]!r})")
    ctx.expect(fsck.returncode == 0, f"`git fsck` passes there ({(fsck.stdout + fsck.stderr)[:300]!r})")
    ctx.expect(head.stdout.strip() == g("rev-parse", "HEAD").stdout.strip(), "the same HEAD on both")
    side = [f for d, _, fs in os.walk(os.path.join(B, sub)) for f in fs if f.endswith(".vapor-meta.json")]
    ctx.info(f"git over Drive: {t and round(t)} s; side files in the other device's .git: {len(side)}")


def cases():
    return [
        ("D-first", "the first files", d_first),
        ("D-latency", "one save, device to device", d_latency),
        ("D-names", "unusual names", d_names),
        ("D-ops", "rename, delete, folder rename, capitals, edit, both ways", d_ops),
        ("D-conflict", "both devices edit one file", d_conflict),
        ("D-git", "a git repository", d_git),
        ("D-burst", "400 small files, then their removal", d_burst),
    ]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("cmd", choices=["list", "run"])
    ap.add_argument("--only", default="")
    a = ap.parse_args()
    cs = cases()
    if a.cmd == "list":
        for c in cs:
            print(c[0], c[1])
        return
    only = [o for o in a.only.split(",") if o]
    if only:
        cs = [c for c in cs if c[0] in only]
    start_ms = int(time.time() * 1000)
    for cid, title, fn in cs:
        ctx = Ctx(cid, title)
        t0 = time.time()
        try:
            fn(ctx)
        except Exception as e:  # noqa: BLE001
            import traceback
            ctx.fails.append(f"raised {type(e).__name__}: {e} {traceback.format_exc()[-300:]}")
            vt.vapor("resume")
        print(f"{'FAIL' if ctx.fails else 'PASS'} {cid} ({time.time() - t0:.0f}s): {title}", flush=True)
        for f in ctx.fails:
            print(f"     - {f}", flush=True)
        for i in ctx.infos:
            print(f"     i {i}", flush=True)
    print("status:", json.dumps(status()))
    print("log:", json.dumps(vt.logscan(start_ms), ensure_ascii=False)[:3500])


if __name__ == "__main__":
    main()
