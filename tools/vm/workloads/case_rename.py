#!/usr/bin/env python3
"""A folder renamed by capitals only, then work inside it."""
import os, sys, time, json
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import vt
from edge import L, C, w, r, listing
from conflicts import all_bytes, copies

def settle():
    vt.wait_quiet(240, 8, verbose=False)

def tree(side, d):
    return sorted(all_bytes(side, d))

def run(tag, wait_between):
    d = f"edge/case{tag}"
    for i in range(5):
        w(os.path.join(L, d, "A/Folder", f"f{i}.txt"), f"original {i}\n")
        w(os.path.join(L, d, "B/Parent/child", f"g{i}.txt"), f"child file {i}\n")
        w(os.path.join(L, d, "C/Thing", f"h{i}.txt"), f"thing {i}\n")
    settle()
    # A: capitals of a folder, then an edit inside
    os.rename(os.path.join(L, d, "A/Folder"), os.path.join(L, d, "A/FOLDER"))
    if wait_between: settle()
    else: time.sleep(wait_between or 1.5)
    w(os.path.join(L, d, "A/FOLDER/f2.txt"), "edited after the folder changed its capitals\n")
    # B: capitals of a parent, then a rename of the child
    os.rename(os.path.join(L, d, "B/Parent"), os.path.join(L, d, "B/PARENT"))
    if wait_between: settle()
    else: time.sleep(1.5)
    os.rename(os.path.join(L, d, "B/PARENT/child"), os.path.join(L, d, "B/PARENT/child2"))
    # C: capitals of a folder, then a new file and a delete inside
    os.rename(os.path.join(L, d, "C/Thing"), os.path.join(L, d, "C/THING"))
    if wait_between: settle()
    else: time.sleep(1.5)
    w(os.path.join(L, d, "C/THING/new.txt"), "new after the capitals\n")
    os.remove(os.path.join(L, d, "C/THING/h0.txt"))
    settle()
    out = {"tag": tag, "waited_between": bool(wait_between)}
    for side, n in ((L, "local"), (C, "cloud")):
        out[n] = {
            "A": tree(side, d + "/A"), "A_names": listing(os.path.join(side, d, "A")),
            "A_f2": (r(os.path.join(side, d, "A/FOLDER/f2.txt")) or b"").decode(),
            "B": tree(side, d + "/B"), "B_names": listing(os.path.join(side, d, "B")),
            "C": tree(side, d + "/C"), "C_names": listing(os.path.join(side, d, "C")),
        }
    print(json.dumps(out, indent=1))

run(sys.argv[1], sys.argv[2] == "wait")
