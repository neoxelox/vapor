#!/usr/bin/env python3
"""monstat: a summary of the sampler's CSV (`vt.py mon`) between two
epoch seconds of the guest clock. Runs on the host.

  monstat.py <mon.csv> <from> <to> [rows to print]
"""
import csv, sys, collections
path = sys.argv[1]
t0, t1 = float(sys.argv[2]), float(sys.argv[3])
rows = [r for r in csv.DictReader(open(path)) if t0 <= float(r["ts"]) <= t1]
def nums(k):
    return [float(r[k]) for r in rows if r[k] not in ("", "None")]
cpu, rss, pend = nums("cpu_pct"), nums("rss_mb"), nums("pending")
thr = collections.Counter(r["throttle"] for r in rows)
p = lambda xs, q: sorted(xs)[min(len(xs) - 1, int(len(xs) * q))] if xs else None
print(f"samples={len(rows)} span={t1 - t0:.0f}s")
print(f"daemon cpu% of one core: avg={sum(cpu)/max(len(cpu),1):.1f} p95={p(cpu,0.95)} max={max(cpu, default=None)}")
print(f"rss MB: min={min(rss, default=None)} max={max(rss, default=None)}")
print(f"queue pending: avg={sum(pend)/max(len(pend),1):.0f} p95={p(pend,0.95)} max={max(pend, default=None)} last={pend[-1] if pend else None}")
print("throttle:", dict(thr))
print("db MB max:", max(nums("db_mb"), default=None), "trash MB max:", max(nums("trash_mb"), default=None), "free GB min:", min(nums("free_gb"), default=None), "index max:", max(nums("index"), default=None))
if len(sys.argv) > 4:
    step = max(1, len(rows) // int(sys.argv[4]))
    for r in rows[::step]:
        print(f"  t+{float(r['ts']) - t0:5.0f}s cpu={r['cpu_pct']:>6} rss={r['rss_mb']:>6} pending={r['pending']:>5} thr={r['throttle']:<9} rec={r['reconcile']} load={r['load1']}")
