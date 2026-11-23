#!/usr/bin/env python3
"""folder_rename: a folder is renamed or removed on one side. Does the
other side keep the old folder, empty?

Each case makes a folder of ten files on side A, waits for side B to
have it, changes the folder on A, and then watches B: when the files
are under the new name, and when the old folder is gone. It watches
for `--watch` seconds, so "still there" has a time next to it.

  folder_rename.py [--a <folder>] [--b <folder>] [--names local,cloud] [--watch 240] [--arrive 300] [--only rename,remove] [--tag run]

The cases: `rename`, `move-files-out` (the files move to a new folder
one by one, then the old one is removed), `remove`, and
`rename-with-ignored` (the folder of the other side holds a
`.DS_Store`).

By default the two folders are the local root and the cloud root of
the filesystem provider, and every case runs in both directions. For
two devices on one cloud, give the two local folders with --a and --b.
"""
import argparse
import json
import os
import shutil
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import vt  # noqa: E402


def write(path, data):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as fh:
        fh.write(data)


def files_under(path):
    out = []
    for base, _, names in os.walk(path):
        out += [os.path.relpath(os.path.join(base, n), path) for n in names if not vt.is_internal(n)]
    return sorted(out)


def until(cond, timeout, step=0.25):
    t0 = time.time()
    while time.time() - t0 < timeout:
        if cond():
            return round(time.time() - t0, 1)
        time.sleep(step)
    return None


def case(kind, src, dst, base, watch, arrive):
    """Returns what the other side did after the change of one folder."""
    old_src, old_dst = os.path.join(src, base, "old"), os.path.join(dst, base, "old")
    new_src, new_dst = os.path.join(src, base, "new"), os.path.join(dst, base, "new")
    for i in range(10):
        write(os.path.join(old_src, f"f{i}.txt"), f"{kind} file {i}\n" * 20)
    if until(lambda: len(files_under(old_dst)) == 10, arrive) is None:
        return {"case": kind, "error": f"the folder did not reach the other side within {arrive}s"}
    if kind == "rename-with-ignored":
        # A file that the default rules ignore sits in the folder of the
        # other side only, as a Finder leaves one.
        write(os.path.join(old_dst, ".DS_Store"), "finder\n")
    time.sleep(8)
    t0 = time.time()
    if kind in ("rename", "rename-with-ignored"):
        os.rename(old_src, new_src)
    elif kind == "remove":
        shutil.rmtree(old_src)
    elif kind == "move-files-out":
        os.makedirs(new_src, exist_ok=True)
        for name in os.listdir(old_src):
            os.rename(os.path.join(old_src, name), os.path.join(new_src, name))
        os.rmdir(old_src)
    out = {"case": kind}
    if kind != "remove":
        out["files_under_new_name_s"] = until(lambda: len(files_under(new_dst)) == 10, arrive)
    out["old_files_gone_s"] = until(lambda: not files_under(old_dst), arrive)
    gone = until(lambda: not os.path.exists(old_dst), watch, step=1.0)
    out["old_folder_gone_s"] = gone if gone is None else round(time.time() - t0, 1)
    out["old_folder_still_there_after_s"] = None if gone is not None else round(time.time() - t0)
    out["left_in_old_folder"] = sorted(os.listdir(old_dst)) if os.path.isdir(old_dst) else None
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--a", default=vt.LOCAL)
    ap.add_argument("--b", default=vt.CLOUD)
    ap.add_argument("--names", default="local,cloud")
    ap.add_argument("--watch", type=float, default=240)
    ap.add_argument("--arrive", type=float, default=300)
    ap.add_argument("--tag", default="run")
    ap.add_argument("--only", default="")
    a = ap.parse_args()
    na, nb = a.names.split(",")
    kinds = [k for k in a.only.split(",") if k] or ["rename", "move-files-out", "remove", "rename-with-ignored"]
    for kind in kinds:
        for src, dst, label in ((a.a, a.b, f"changed on {na}, watched on {nb}"), (a.b, a.a, f"changed on {nb}, watched on {na}")):
            base = f"folder-{a.tag}/{kind}-on-{label.split()[2].rstrip(',')}"
            res = case(kind, src, dst, base, a.watch, a.arrive)
            res["direction"] = label
            print(json.dumps(res), flush=True)


if __name__ == "__main__":
    main()
