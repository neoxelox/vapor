#!/usr/bin/env python3
"""extras: big files, a fresh git repository, and the same tree on both
sides before the first sync (the filesystem provider).

  extras.py git big200 big1g pre
"""
import hashlib, json, os, shutil, subprocess, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import vt
from edge import L, C, w, r, listing
from conflicts import copies

def settle(t=900):
    return vt.wait_quiet(t, 8, verbose=False)

def timed_arrival(src_path, dst_path, size, limit=900):
    t0 = time.time()
    while time.time() - t0 < limit:
        try:
            if os.path.getsize(dst_path) == size:
                return time.time() - t0
        except OSError:
            pass
        time.sleep(0.2)
    return None

def big(which):
    out = {}
    for label, src, dst in (("upload", L, C), ("download", C, L)):
        rel = f"big/{label}-{which}.bin"
        os.makedirs(os.path.join(src, "big"), exist_ok=True)
        size = (1 << 30) if which == "1g" else (200 << 20)
        with open(os.path.join(src, rel), "wb") as fh:
            block = os.urandom(1 << 20)
            for i in range(size >> 20):
                fh.write(block[i % 251:] + block[:i % 251])
        t = timed_arrival(os.path.join(src, rel), os.path.join(dst, rel), size)
        settle()
        same = vt.sha(os.path.join(src, rel)) == vt.sha(os.path.join(dst, rel))
        out[label] = {"mib": size >> 20, "seconds": round(t, 1) if t else None, "mib_per_s": round((size >> 20) / t, 1) if t else None, "same_bytes": same}
        for side in (L, C):
            try: os.remove(os.path.join(side, rel))
            except OSError: pass
        settle()
    print("big", which, json.dumps(out))

def git_init():
    d = "gitfresh/repo"
    os.makedirs(os.path.join(L, d), exist_ok=True)
    subprocess.run(["git", "init", "-q", "-b", "main", os.path.join(L, d)], check=True)
    w(os.path.join(L, d, "README.md"), "# fresh\n")
    settle()
    g = subprocess.run(["git", "--no-optional-locks", "-C", os.path.join(C, d), "status", "--short"], capture_output=True, text=True)
    print("git init, no commit yet: `git status` in the cloud copy:", g.returncode, (g.stdout + g.stderr).strip()[:160])
    print("  cloud .git holds:", sorted(os.listdir(os.path.join(C, d, ".git"))) if os.path.isdir(os.path.join(C, d, ".git")) else None)
    subprocess.run(["git", "-C", os.path.join(L, d), "-c", "user.email=a@example.com", "-c", "user.name=A", "add", "-A"])
    subprocess.run(["git", "-C", os.path.join(L, d), "-c", "user.email=a@example.com", "-c", "user.name=A", "commit", "-q", "-m", "first"])
    settle()
    g = subprocess.run(["git", "--no-optional-locks", "-C", os.path.join(C, d), "status", "--short"], capture_output=True, text=True)
    print("after the first commit: `git status` in the cloud copy:", g.returncode, repr((g.stdout + g.stderr).strip()[:160]))

def same_tree_both_sides():
    """A user who already has the same files in both folders adds Vapor."""
    d = "preexisting"
    vt.vapor("service", "stop")
    time.sleep(1)
    for i in range(2000):
        w(os.path.join(L, d, f"dir{i % 40:02d}", f"doc_{i:04d}.txt"), (f"document {i}\n" * 40))
    shutil.copytree(os.path.join(L, d), os.path.join(C, d))
    # 20 files differ on purpose, and 20 exist on one side only.
    for i in range(20):
        w(os.path.join(C, d, "dir00", f"doc_{i * 40:04d}.txt"), f"the cloud has other content for {i}\n")
        w(os.path.join(L, d, "only-here", f"l{i}.txt"), f"only here {i}\n")
        w(os.path.join(C, d, "only-there", f"c{i}.txt"), f"only there {i}\n")
    hw0 = vt.queue_counts()["hw"]
    t0 = time.time()
    vt.vapor("service", "start")
    time.sleep(3)
    ok, secs, st = settle()
    res = vt.compare(sub=d, max_list=5)
    print("same tree on both sides before the first sync:", json.dumps({
        "seconds": round(time.time() - t0, 1), "intents": vt.queue_counts()["hw"] - hw0,
        "converged": res["converged"], "files": res["local_files"], "conflict_copies": res["conflict_copies"],
        "only_local": res["only_local"], "only_cloud": res["only_cloud"], "content_diff": res["content_diff"],
        "decisions": st.get("decisions"),
    }))

for name in sys.argv[1:]:
    {"big200": lambda: big("200m"), "big1g": lambda: big("1g"), "git": git_init, "pre": same_tree_both_sides}[name]()
