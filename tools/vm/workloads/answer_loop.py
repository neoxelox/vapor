#!/usr/bin/env python3
"""A user who answers every mass-delete question with `apply` a few seconds
after it shows. Prints each answer. Runs until killed or --seconds."""
import json, os, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import vt
end = time.time() + float(sys.argv[1]) if len(sys.argv) > 1 else 1e12
n = 0
while time.time() < end:
    for d in (vt.vapor_json("decisions", "list") or {}).get("decisions", []):
        if d.get("kind") == "mass-delete":
            time.sleep(3)
            extra = ["--profile", str(d["profileId"])] if d.get("profileId") else []
            code, out, err = vt.vapor("decisions", "resolve", str(d["id"]), "--choose", "apply", *extra)
            n += 1
            print(f"{time.time():.0f} answered #{d['id']} apply ({n}): {d.get('question', '')[:150]}", flush=True)
        else:
            print(f"{time.time():.0f} other question #{d.get('id')} {d.get('kind')}: {d.get('question', '')[:150]}", flush=True)
    time.sleep(4)
