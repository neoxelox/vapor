#!/usr/bin/env python3
"""probe: how long one saved file takes to reach the other side.

Every `--every` seconds it saves one new small file and rewrites one
existing file in each probe folder, then watches the other side for the
same bytes. It prints one line per probe and a summary.

  probe.py --dirs code/demo/src,probe-outside --seconds 300 --every 10 [--side local]
"""
import argparse, json, os, sys, threading, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import vt

LATEST = {}


def watch(path, want, t0, limit, out, label, key=None):
    while time.time() - t0 < limit:
        try:
            with open(path, "rb") as fh:
                if fh.read() == want:
                    out.append((label, round(time.time() - t0, 2)))
                    print(f"{label} -> {time.time() - t0:.2f}s", flush=True)
                    return
        except OSError:
            pass
        if key is not None and LATEST.get(key) != t0:
            # A later save of the same file replaced this one before it synced.
            out.append((label, -1.0))
            print(f"{label} -> superseded after {time.time() - t0:.1f}s", flush=True)
            return
        time.sleep(0.05)
    out.append((label, None))
    print(f"{label} -> NOT WITHIN {limit:.0f}s", flush=True)

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dirs", required=True)
    ap.add_argument("--seconds", type=float, default=120)
    ap.add_argument("--every", type=float, default=10)
    ap.add_argument("--limit", type=float, default=900)
    ap.add_argument("--side", choices=["local", "cloud"], default="local")
    ap.add_argument("--tag", default="p")
    a = ap.parse_args()
    src, dst = (vt.LOCAL, vt.CLOUD) if a.side == "local" else (vt.CLOUD, vt.LOCAL)
    dirs = a.dirs.split(",")
    results, threads = [], []
    start = time.time()
    i = 0
    while time.time() - start < a.seconds:
        for d in dirs:
            os.makedirs(os.path.join(src, d), exist_ok=True)
            for kind, name in (("new", f"probe_{a.tag}_{i}.ts"), ("edit", f"probe_{a.tag}_edit.ts")):
                data = f"// probe {d} {kind} {i} {time.time()}\nexport const x = {i};\n".encode()
                t0 = time.time()
                with open(os.path.join(src, d, name), "wb") as fh:
                    fh.write(data)
                key = (d, name) if kind == "edit" else None
                if key:
                    LATEST[key] = t0
                th = threading.Thread(target=watch, args=(os.path.join(dst, d, name), data, t0, a.limit, results, f"{d}|{kind}|{round(t0 - start)}", key), daemon=True)
                th.start()
                threads.append(th)
        i += 1
        time.sleep(a.every)
    for th in threads:
        th.join(a.limit + 5)
    by = {}
    for label, secs in sorted(results, key=lambda r: (r[0].split("|")[0], int(r[0].split("|")[2]))):
        d, kind, at = label.split("|")
        by.setdefault(d, []).append(secs)
    for d, xs in by.items():
        got = sorted(x for x in xs if x is not None and x >= 0)
        lost = sum(1 for x in xs if x is None)
        sup = sum(1 for x in xs if x is not None and x < 0)
        print(f"   {d}: superseded edits (a later save came first): {sup}")
        if got:
            print(f"== {d}: n={len(xs)} p50={got[len(got)//2]:.1f}s p90={got[min(len(got)-1, int(len(got)*0.9))]:.1f}s max={got[-1]:.1f}s never={lost}")
        else:
            print(f"== {d}: n={len(xs)} none arrived")

if __name__ == "__main__":
    main()
