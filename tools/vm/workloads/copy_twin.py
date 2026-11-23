#!/usr/bin/env python3
"""copy_twin: a synced file is copied on one side with its modification
time kept (`cp -p`, a Finder duplicate). Does the other side get the
copy and keep the original?

  copy_twin.py [--a <folder>] [--b <folder>] [--names local,cloud] [--watch 180] [--arrive 120] [--tag run]

Each case runs in both directions, once with `cp -p` and once with a
plain `cp`. Prints one JSON line for each: the names of the pair that
each side holds, at each moment that changed, for `--watch` seconds.
"""
import argparse
import json
import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import vt  # noqa: E402


def until(cond, timeout, step=0.25):
    t0 = time.time()
    while time.time() - t0 < timeout:
        if cond():
            return round(time.time() - t0, 1)
        time.sleep(step)
    return None


def case(src, dst, base, watch, arrive, keep_time):
    a_src, a_dst = os.path.join(src, base, "original.bin"), os.path.join(dst, base, "original.bin")
    c_src = os.path.join(src, base, "copy.bin")
    os.makedirs(os.path.dirname(a_src), exist_ok=True)
    with open(a_src, "wb") as fh:
        fh.write(os.urandom(50_000))
    if until(lambda: os.path.exists(a_dst), arrive) is None:
        return {"error": f"the original did not reach the other side within {arrive}s"}
    time.sleep(10)
    subprocess.run(["cp", "-p", a_src, c_src] if keep_time else ["cp", a_src, c_src], check=True)
    t0 = time.time()

    def names(folder):
        return " ".join(n for n in ("original.bin", "copy.bin") if os.path.exists(os.path.join(folder, base, n))) or "-"

    timeline, last = [], None
    while time.time() - t0 < watch:
        now = (names(dst), names(src))
        if now != last:
            timeline.append({"s": round(time.time() - t0, 1), "other_side": now[0], "changed_side": now[1]})
            last = now
        time.sleep(0.2)
    return {
        "copy_keeps_time": keep_time,
        "other_side_at_end": last[0],
        "changed_side_at_end": last[1],
        "timeline": timeline[:14],
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--a", default=vt.LOCAL)
    ap.add_argument("--b", default=vt.CLOUD)
    ap.add_argument("--names", default="local,cloud")
    ap.add_argument("--watch", type=float, default=180)
    ap.add_argument("--arrive", type=float, default=120)
    ap.add_argument("--tag", default="run")
    a = ap.parse_args()
    na, nb = a.names.split(",")
    for keep_time in (True, False):
        for src, dst, made, seen in ((a.b, a.a, nb, na), (a.a, a.b, na, nb)):
            base = f"copy-twin-{a.tag}/{'same-time' if keep_time else 'new-time'}-made-on-{made}"
            res = case(src, dst, base, a.watch, a.arrive, keep_time)
            res["direction"] = f"copied on {made}, watched on {seen}"
            print(json.dumps(res), flush=True)


if __name__ == "__main__":
    main()
