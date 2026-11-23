#!/usr/bin/env python3
"""Answers every open question the way the test wants, and prints what happened."""
import json, os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import vt
ds = (vt.vapor_json("decisions", "list") or {}).get("decisions", [])
print("open:", len(ds))
for d in ds:
    kind, path, did = d.get("kind"), d.get("path") or "", d.get("id")
    opts = [o.get("key") for o in d.get("options", [])]
    if kind == "type-mismatch":
        choice = "prefer-local" if "/ops-local" in path else "prefer-cloud" if "/ops-cloud" in path else "keep-both"
    elif kind == "conflict":
        choice = os.path.basename(path) if os.path.basename(path) in opts else opts[0]
    elif kind == "unsyncable-name":
        choice = "skip"
    else:
        print("leave", did, kind); continue
    code, out, err = vt.vapor("decisions", "resolve", str(did), "--choose", choice)
    print(did, kind, os.path.basename(path), "->", choice, "exit", code, (out + err).strip()[:160].replace("\n", " | "))
ok, secs, st = vt.wait_quiet(300, 8, verbose=False)
print("quiet:", ok, round(secs), json.dumps({k: st[k] for k in ("queued", "failed", "decisions", "db_pending")}), "stuck:", json.dumps(st.get("stuck"))[:600])
