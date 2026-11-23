#!/usr/bin/env python3
"""latency: how long one change takes to reach the other side.

One change at a time, with a gap between changes, so no burst rule of
the daemon applies. Each kind of change is timed in both directions:
from the moment the change is made to the moment the other folder shows
it. Prints one line for each kind and direction.

  latency.py [--samples 8] [--a <folder>] [--b <folder>] [--only new_code,delete] [--timeout 180]

By default the two folders are the local root and the cloud root of the
filesystem provider. For two devices on one cloud, give the two local
folders with --a and --b.
"""
import argparse
import json
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import vt  # noqa: E402


def write(path, data):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as fh:
        fh.write(data)


def read(path):
    try:
        with open(path, "rb") as fh:
            return fh.read()
    except OSError:
        return None


def until(cond, timeout):
    t0 = time.time()
    while time.time() - t0 < timeout:
        if cond():
            return time.time() - t0
        time.sleep(0.02)
    return None


def payload(tag, size):
    seed = (tag + "\n").encode()
    return (seed * (size // len(seed) + 1))[:size]


def sample(kind, src, dst, d, i, timeout):
    """Makes one change under `src/d` and returns the seconds until
    `dst/d` shows it, or None."""
    stamp = f"{kind} {i} {time.time()}"
    if kind in ("new_code", "new_media", "new_big"):
        ext, size = {"new_code": (".ts", 300), "new_media": (".jpg", 200_000), "new_big": (".bin", 5_000_000)}[kind]
        name = f"f{i}{ext}"
        data = payload(stamp, size)
        write(os.path.join(src, d, name), data)
        return until(lambda: read(os.path.join(dst, d, name)) == data, timeout)
    if kind == "edit_code":
        name = "edited.ts"
        data = payload(stamp, 300 + i)
        write(os.path.join(src, d, name), data)
        return until(lambda: read(os.path.join(dst, d, name)) == data, timeout)
    if kind in ("delete", "rename"):
        name = f"victim{i}.ts"
        data = payload(stamp, 300)
        write(os.path.join(src, d, name), data)
        if until(lambda: read(os.path.join(dst, d, name)) == data, timeout) is None:
            return None
        time.sleep(3)
        if kind == "delete":
            os.remove(os.path.join(src, d, name))
            return until(lambda: not os.path.exists(os.path.join(dst, d, name)), timeout)
        os.rename(os.path.join(src, d, name), os.path.join(src, d, f"renamed{i}.ts"))
        return until(lambda: read(os.path.join(dst, d, f"renamed{i}.ts")) == data and not os.path.exists(os.path.join(dst, d, name)), timeout)
    raise ValueError(kind)


KINDS = ["new_code", "edit_code", "new_media", "new_big", "delete", "rename"]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--samples", type=int, default=8)
    ap.add_argument("--a", default=vt.LOCAL)
    ap.add_argument("--b", default=vt.CLOUD)
    ap.add_argument("--names", default="local,cloud")
    ap.add_argument("--only", default="")
    ap.add_argument("--timeout", type=float, default=180)
    ap.add_argument("--gap", type=float, default=2.0)
    ap.add_argument("--tag", default="run")
    a = ap.parse_args()
    kinds = [k for k in a.only.split(",") if k] or KINDS
    na, nb = a.names.split(",")
    rows = []
    for kind in kinds:
        for src, dst, label in ((a.a, a.b, f"{na}->{nb}"), (a.b, a.a, f"{nb}->{na}")):
            d = f"latency-{a.tag}/{kind}-{label.replace('->', '-to-')}"
            times = []
            for i in range(a.samples):
                times.append(sample(kind, src, dst, d, i, a.timeout))
                time.sleep(a.gap)
            got = sorted(t for t in times if t is not None)
            row = {
                "kind": kind, "direction": label, "n": len(times), "timeouts": len(times) - len(got),
                "p50": round(got[len(got) // 2], 2) if got else None,
                "min": round(got[0], 2) if got else None, "max": round(got[-1], 2) if got else None,
            }
            rows.append(row)
            print(f"{kind:<10} {label:<14} n={row['n']} p50={row['p50']}s min={row['min']}s max={row['max']}s not within {a.timeout:.0f}s: {row['timeouts']}", flush=True)
    path = os.path.join(vt.OUT, f"latency-{a.tag}-{int(time.time())}.json")
    with open(path, "w") as fh:
        json.dump(rows, fh, indent=1)


if __name__ == "__main__":
    main()
