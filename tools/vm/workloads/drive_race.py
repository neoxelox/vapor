#!/usr/bin/env python3
"""drive_race: both devices edit many files at the same moment through
Google Drive, and every edit must survive on both. One run is many
races: the daemon is paused, each device edits each file, and the
resume sends both edits of a file in one tick.

  drive_race.py [--rounds N] [--files M] [--quiet S] [--size BYTES]

One round is one folder of M files (default 30). With `--files 1` both
uploads of the file go at the same moment, which is the race that the
copy of a replaced version is for; with many files most pairs go one
after the other and end as an ordinary conflict.

A file ends in one of three ways. `replaced`: both uploads landed and
the replaced version is a `~conflict-replaced` copy. `conflict`: the
second upload saw the first and its edit is a copy named after its
device. `lost`: one edit is on neither device, which is the failure.
"""
import argparse
import json
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import vt  # noqa: E402
from drive_tests import A, B, r, tree, w, wait_equal, wait_settled  # noqa: E402


def one_round(count, quiet, size):
    sub = f"t-race-{int(time.time())}"
    names = [f"doc{i:02d}.txt" for i in range(count)]
    pad = lambda: os.urandom(size) if size else b""  # noqa: E731
    for n in names:
        w(os.path.join(A, sub, n), f"base of {n}\n".encode() + pad())
    if wait_equal(sub, 900, expect=names) is None:
        sys.exit(f"drive_race: the {count} base files did not reach the other device")
    vt.vapor("pause")
    edits = {}
    for n in names:
        edits[n] = (f"edited on device A: {sub}/{n}\n".encode() + pad(), f"edited on device B, longer: {sub}/{n}\n".encode() + pad())
        w(os.path.join(A, sub, n), edits[n][0])
        w(os.path.join(B, sub, n), edits[n][1])
    vt.vapor("resume")
    equal = wait_settled(sub, quiet)
    kinds, lost = {"replaced": 0, "conflict": 0, "lost": 0}, []
    for n in names:
        stem = n[:-4]
        sides = []
        for root in (A, B):
            held = {f: r(os.path.join(root, sub, f)) for f in os.listdir(os.path.join(root, sub)) if f.startswith(stem)}
            sides.append(held)
        both = all(edits[n][0] in side.values() and edits[n][1] in side.values() for side in sides)
        if not both:
            kinds["lost"] += 1
            first_line = lambda v: (v or b"").split(b"\n")[0].decode(errors="replace")  # noqa: E731
            lost.append({"file": f"{sub}/{n}", "A": {f: first_line(v) for f, v in sides[0].items()},
                         "B": {f: first_line(v) for f, v in sides[1].items()}})
        elif any("~conflict-replaced-" in f for f in sides[0] if f != n):
            kinds["replaced"] += 1
        else:
            kinds["conflict"] += 1
    print(json.dumps({"folder": sub, "files": count, "both_devices_equal": equal, **kinds}), flush=True)
    for item in lost:
        print("LOST", json.dumps(item, ensure_ascii=False), flush=True)
    return kinds, equal


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--rounds", type=int, default=1)
    ap.add_argument("--files", type=int, default=30, help="files in one round; with 1, both uploads of the file go at the same moment")
    ap.add_argument("--quiet", type=int, default=45, help="seconds with no change before a round is judged")
    ap.add_argument("--size", type=int, default=0, help="random bytes after the first line of each version; above 5 MiB the uploads and the copy go in chunks")
    a = ap.parse_args()
    start_ms = int(time.time() * 1000)
    total, all_equal = {"replaced": 0, "conflict": 0, "lost": 0}, True
    for _ in range(a.rounds):
        kinds, equal = one_round(a.files, a.quiet, a.size)
        all_equal = all_equal and equal
        for k, v in kinds.items():
            total[k] += v
    print("total:", json.dumps({"rounds": a.rounds, **total, "both_devices_equal": all_equal}), flush=True)
    print("log:", json.dumps(vt.logscan(start_ms), ensure_ascii=False)[:2500], flush=True)
    sys.exit(1 if total["lost"] or not all_equal else 0)


if __name__ == "__main__":
    main()
