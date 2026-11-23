#!/usr/bin/env python3
"""drive_conflict: both devices edit one file through Google Drive, in
five variants. `settled`: the base version is on both devices for 25 s
before the edits. `fresh`: the edits come the moment the base arrives
on the second device. `paused`: the daemon is paused while both edit.
Both edits must survive on both devices."""
import json, os, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import vt
from drive_tests import A, B, w, r, tree, wait_equal

def run(tag, settled, paused):
    sub = f"t-conf-{tag}"
    w(os.path.join(A, sub, "doc.txt"), "base\n")
    t0 = time.time()
    while r(os.path.join(B, sub, "doc.txt")) != b"base\n" and time.time() - t0 < 300:
        time.sleep(0.01)
    arrive = time.time() - t0
    if settled:
        time.sleep(25)
    if paused:
        vt.vapor("pause")
    la, lb = f"edited on device A ({tag})\n".encode(), f"edited on device B, longer ({tag})\n".encode()
    w(os.path.join(A, sub, "doc.txt"), la)
    w(os.path.join(B, sub, "doc.txt"), lb)
    if paused:
        vt.vapor("resume")
    t = wait_equal(sub, 400)
    time.sleep(20)
    t2 = wait_equal(sub, 200)
    ta = {k: r(os.path.join(A, sub, k)) for k in tree(A, sub)}
    tb = {k: r(os.path.join(B, sub, k)) for k in tree(B, sub)}
    print(json.dumps({
        "variant": tag, "settled": settled, "paused": paused, "base_arrived_s": round(arrive, 1),
        "equal": ta == tb, "files_A": sorted(ta), "files_B": sorted(tb),
        "A_edit_survives": la in ta.values() and la in tb.values(),
        "B_edit_survives": lb in ta.values() and lb in tb.values(),
        "name_holds_on_A": (ta.get("doc.txt") or b"").decode().strip(),
    }), flush=True)

for tag, settled, paused in (("settled-paused", True, True), ("settled-live", True, False), ("fresh-paused", False, True), ("fresh-live", False, False), ("fresh-paused-2", False, True)):
    run(tag, settled, paused)
