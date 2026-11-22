#!/usr/bin/env python3
"""vt: helpers to judge a Vapor install from the outside, as a user would.

  vt.py st                      one line: run state, throttle, queue, decisions
  vt.py wait [--timeout S]      wait until the daemon has nothing left to do
  vt.py compare                 the tree oracle: local root against cloud root
  vt.py mon start|stop          sample the daemon's CPU, memory, and queue to CSV
  vt.py logscan [--since MS]    warnings and errors in the daemon log
"""
import argparse
import fnmatch
import hashlib
import json
import os
import re
import sqlite3
import stat
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor

HOME = os.path.expanduser("~")
VAPOR = "/usr/local/bin/vapor"
LOCAL = os.environ.get("VT_LOCAL", HOME + "/Vapor")
CLOUD = os.environ.get("VT_CLOUD", HOME + "/CloudDrive/Vapor")
VDIR = os.environ.get("VT_VDIR", HOME + "/.vapor")
DB = os.environ.get("VT_DB", VDIR + "/state/vapor.sqlite")
OUT = os.environ.get("VT_OUT", "/Volumes/My Shared Files/results/out")

# Names Vapor never syncs, whatever the rules say (ignore-rules.md).
INTERNAL_FILES = (".vapor-root",)
INTERNAL_GLOBS = (".vapor-tmp-*", "*.vapor-meta.json")
INTERNAL_DIRS = (".vapor",)


def vapor(*args, timeout=60):
    try:
        p = subprocess.run([VAPOR, *args], capture_output=True, text=True, timeout=timeout)
        return p.returncode, p.stdout, p.stderr
    except subprocess.TimeoutExpired:
        return 124, "", "timeout"


PROFILE = os.environ.get("VT_PROFILE", "")


def vapor_json(*args, timeout=60):
    code, out, _ = vapor(*args, "--json", timeout=timeout)
    if code != 0:
        return None
    try:
        data = json.loads(out)
    except ValueError:
        return None
    if PROFILE and args and args[0] == "status":
        # Judge one profile: its own run state, queue, and reconcile.
        for prof in data.get("profiles", []):
            if prof.get("id") == PROFILE:
                data = {**data, **{k: prof[k] for k in ("run", "reconcile", "queued", "failed", "decisions") if k in prof}}
    return data


def db_scalar(sql, db=None):
    try:
        con = sqlite3.connect(f"file:{db or DB}?mode=ro", uri=True, timeout=5)
        try:
            row = con.execute(sql).fetchone()
            return row[0] if row else None
        finally:
            con.close()
    except sqlite3.Error:
        return None


def db_rows(sql, db=None):
    try:
        con = sqlite3.connect(f"file:{db or DB}?mode=ro", uri=True, timeout=5)
        try:
            return con.execute(sql).fetchall()
        finally:
            con.close()
    except sqlite3.Error:
        return []


def queue_counts():
    return {
        "pending": db_scalar("SELECT COUNT(*) FROM queue_intents WHERE state IN ('pending','leased')"),
        "held": db_scalar("SELECT COUNT(*) FROM queue_intents WHERE state='held'"),
        "failed": db_scalar("SELECT COUNT(*) FROM failed_intents"),
        "hw": db_scalar("SELECT COALESCE((SELECT seq FROM sqlite_sequence WHERE name='queue_intents'),0)"),
        "index": db_scalar("SELECT COUNT(*) FROM sync_index"),
        "decisions": db_scalar("SELECT COUNT(*) FROM pending_decisions WHERE resolved_at_ms IS NULL"),
    }


def status_line():
    s = vapor_json("status") or {}
    q = queue_counts()
    run = s.get("run", {})
    thr = s.get("throttle", {})
    rec = s.get("reconcile", {})
    return {
        "run": run.get("state"),
        "reason": run.get("reason"),
        "throttle": thr.get("state"),
        "throttle_reason": thr.get("reason"),
        "reconcile": rec.get("state"),
        "queued": s.get("queued"),
        "failed": s.get("failed"),
        "decisions": s.get("decisions"),
        **{f"db_{k}": v for k, v in q.items()},
    }


def cmd_st(_args):
    print(json.dumps(status_line()))


def stuck_rows():
    """Queued intents that failed three times or more: they sit in a
    retry backoff, and the queue does not drain while they do."""
    return [
        {"id": r[0], "kind": r[1], "path": r[2], "attempts": r[3], "error": (r[4] or "")[:160]}
        for r in db_rows("SELECT id, kind, path_text, attempt_count, last_error FROM queue_intents WHERE state='pending' AND attempt_count >= 3")
    ]


def wait_quiet(timeout=600, quiet=10, verbose=True, label="", allow_stuck=True):
    """Waits until the durable queue is empty, no intent was enqueued for
    `quiet` seconds, and the daemon reports no reconcile in progress.
    Returns (ok, seconds, last status)."""
    start = time.time()
    last_note = start
    while True:
        q = queue_counts()
        now = time.time()
        if now - start > timeout:
            st = status_line()
            if verbose:
                print(f"[wait{label}] TIMEOUT after {int(now - start)}s: {json.dumps(st)}", flush=True)
            return False, now - start, st
        stuck = stuck_rows() if q["pending"] else []
        if q["pending"] == 0 or (allow_stuck and len(stuck) == q["pending"]):
            hw = q["hw"]
            base = q["pending"]
            t0 = time.time()
            still = True
            while time.time() - t0 < quiet:
                time.sleep(0.5)
                q2 = queue_counts()
                if q2["pending"] != base or q2["hw"] != hw:
                    still = False
                    break
            if still:
                s = vapor_json("status") or {}
                rec = (s.get("reconcile") or {}).get("state")
                if rec in (None, "idle") and (s.get("queued") in (0, None, base)):
                    el = time.time() - start
                    st = status_line()
                    st["stuck"] = stuck
                    if verbose:
                        print(f"[wait{label}] quiet after {el:.1f}s (incl. {quiet}s quiet window): {json.dumps(st)}", flush=True)
                    return True, el, st
        if verbose and now - last_note > 20:
            print(f"[wait{label}] {int(now - start)}s: {json.dumps(queue_counts())}", flush=True)
            last_note = now
        time.sleep(0.5)


def cmd_wait(args):
    ok, _, _ = wait_quiet(args.timeout, args.quiet)
    sys.exit(0 if ok else 1)


def is_internal(name):
    return name in INTERNAL_FILES or any(fnmatch.fnmatch(name, g) for g in INTERNAL_GLOBS)


def scan(root, excludes=(), prune_rel=()):
    """rel path -> (size, mtime_ns) for regular files; also lists the
    symlinks, the special files, and the directories."""
    files, links, specials, dirs = {}, [], [], []
    prune = set(prune_rel)
    root = root.rstrip("/")
    for base, dnames, fnames in os.walk(root, followlinks=False):
        rel_base = os.path.relpath(base, root)
        if rel_base == ".":
            rel_base = ""
        keep = []
        for d in dnames:
            rel = os.path.join(rel_base, d) if rel_base else d
            full = os.path.join(base, d)
            if os.path.islink(full):
                links.append(rel)
                continue
            if d in INTERNAL_DIRS or rel in prune or any(fnmatch.fnmatch(d, e) for e in excludes):
                continue
            dirs.append(rel)
            keep.append(d)
        dnames[:] = keep
        for f in fnames:
            rel = os.path.join(rel_base, f) if rel_base else f
            if is_internal(f) or rel in prune or any(fnmatch.fnmatch(f, e) for e in excludes):
                continue
            full = os.path.join(base, f)
            try:
                st = os.lstat(full)
            except OSError:
                continue
            if stat.S_ISLNK(st.st_mode):
                links.append(rel)
            elif stat.S_ISREG(st.st_mode):
                files[rel] = (st.st_size, st.st_mtime_ns, st.st_mode & 0o777)
            else:
                specials.append(rel)
    return files, links, specials, dirs


def sha(path):
    h = hashlib.sha256()
    try:
        with open(path, "rb") as fh:
            while True:
                b = fh.read(1 << 20)
                if not b:
                    break
                h.update(b)
    except OSError as e:
        return f"ERR:{e.errno}"
    return h.hexdigest()


def hash_many(root, rels, workers=8):
    with ThreadPoolExecutor(workers) as ex:
        return dict(zip(rels, ex.map(lambda r: sha(os.path.join(root, r)), rels)))


def ignored_paths():
    data = vapor_json("ignored", timeout=120) or {}
    out = set()
    for item in data.get("entries", []):
        p = item.get("path") if isinstance(item, dict) else item
        if p:
            p = p.rstrip("/")
            if p.startswith(LOCAL + "/"):
                p = p[len(LOCAL) + 1:]
            out.add(p)
    return out


def compare(local=None, cloud=None, sub="", excludes=(), use_ignored=True, max_list=15):
    local = os.path.join(local or LOCAL, sub) if sub else (local or LOCAL)
    cloud = os.path.join(cloud or CLOUD, sub) if sub else (cloud or CLOUD)
    prune = set()
    if use_ignored:
        for p in ignored_paths():
            if sub:
                if p.startswith(sub.rstrip("/") + "/"):
                    prune.add(p[len(sub.rstrip("/")) + 1:])
            else:
                prune.add(p)
    lf, ll, ls, ld = scan(local, excludes, prune)
    cf, cl, cs, cd = scan(cloud, excludes, prune)
    only_local = sorted(set(lf) - set(cf))
    only_cloud = sorted(set(cf) - set(lf))
    both = sorted(set(lf) & set(cf))
    size_diff = [r for r in both if lf[r][0] != cf[r][0]]
    same_size = [r for r in both if lf[r][0] == cf[r][0]]
    lh = hash_many(local, same_size)
    ch = hash_many(cloud, same_size)
    content_diff = [r for r in same_size if lh[r] != ch[r]]
    mode_diff = [r for r in both if lf[r][2] != cf[r][2]]
    conflicts = sorted(r for r in set(lf) | set(cf) if "~conflict-" in r)
    dirs_only_local = sorted(set(ld) - set(cd))
    dirs_only_cloud = sorted(set(cd) - set(ld))
    res = {
        "local_files": len(lf),
        "cloud_files": len(cf),
        "local_bytes": sum(v[0] for v in lf.values()),
        "cloud_bytes": sum(v[0] for v in cf.values()),
        "only_local": len(only_local),
        "only_cloud": len(only_cloud),
        "size_diff": len(size_diff),
        "content_diff": len(content_diff),
        "mode_diff": len(mode_diff),
        "conflict_copies": len(conflicts),
        "local_symlinks": len(ll),
        "cloud_symlinks": len(cl),
        "local_specials": len(ls),
        "dirs_only_local": len(dirs_only_local),
        "dirs_only_cloud": len(dirs_only_cloud),
        "pruned_ignored": len(prune),
        "converged": not (only_local or only_cloud or size_diff or content_diff),
        "samples": {
            "only_local": only_local[:max_list],
            "only_cloud": only_cloud[:max_list],
            "size_diff": size_diff[:max_list],
            "content_diff": content_diff[:max_list],
            "mode_diff": mode_diff[:max_list],
            "conflict_copies": conflicts[:max_list],
            "dirs_only_local": dirs_only_local[:max_list],
            "dirs_only_cloud": dirs_only_cloud[:max_list],
        },
    }
    return res


def cmd_compare(args):
    res = compare(sub=args.sub, excludes=args.exclude, use_ignored=not args.no_ignored, max_list=args.max_list)
    if args.brief:
        res = {k: v for k, v in res.items() if k != "samples" or not res["converged"]}
        res.pop("samples", None) if res.get("converged") else None
    print(json.dumps(res, indent=1))
    sys.exit(0 if res["converged"] else 1)


def daemon_pid():
    try:
        out = subprocess.run(["pgrep", "-x", "vapord"], capture_output=True, text=True).stdout.split()
        return int(out[0]) if out else None
    except (ValueError, OSError):
        return None


def cputime_seconds(pid):
    """(cpu seconds, rss KiB) of a process, from ps."""
    try:
        out = subprocess.run(["ps", "-o", "cputime=,rss=", "-p", str(pid)], capture_output=True, text=True).stdout.split()
        if len(out) < 2:
            return None, None
        t = out[0]
        days = 0
        if "-" in t:
            d, t = t.split("-")
            days = int(d)
        parts = [float(x) for x in t.split(":")]
        secs = 0.0
        for p in parts:
            secs = secs * 60 + p
        return secs + days * 86400, int(out[1])
    except (ValueError, OSError):
        return None, None


def dir_size_mb(path):
    try:
        out = subprocess.run(["du", "-sk", path], capture_output=True, text=True).stdout.split()
        return int(out[0]) / 1024 if out else 0
    except (ValueError, OSError):
        return 0


def mon_loop(path, interval):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    new = not os.path.exists(path)
    fh = open(path, "a", buffering=1)
    if new:
        fh.write("ts,pid,cpu_pct,rss_mb,pending,held,failed,index,hw,throttle,run,reconcile,free_gb,db_mb,trash_mb,load1,app_cpu_pct\n")
    last = {}
    throttle = run = rec = ""
    trash_mb = 0.0
    n = 0
    while True:
        t = time.time()
        pid = daemon_pid()
        cpu_pct = ""
        rss_mb = ""
        if pid:
            secs, rss = cputime_seconds(pid)
            if secs is not None:
                prev = last.get(("d", pid))
                if prev:
                    cpu_pct = f"{100 * (secs - prev[1]) / max(t - prev[0], 0.001):.1f}"
                last = {k: v for k, v in last.items() if k[0] != "d"}
                last[("d", pid)] = (t, secs)
                rss_mb = f"{rss / 1024:.1f}"
        app_pct = ""
        try:
            apid = subprocess.run(["pgrep", "-x", "Vapor"], capture_output=True, text=True).stdout.split()
            if apid:
                secs, _ = cputime_seconds(int(apid[0]))
                prev = last.get(("a", apid[0]))
                if prev and secs is not None:
                    app_pct = f"{100 * (secs - prev[1]) / max(t - prev[0], 0.001):.1f}"
                if secs is not None:
                    last[("a", apid[0])] = (t, secs)
        except (ValueError, OSError):
            pass
        q = queue_counts()
        if n % 5 == 0:
            s = vapor_json("status", timeout=10) or {}
            throttle = (s.get("throttle") or {}).get("state", "?")
            run = (s.get("run") or {}).get("state", "?")
            rec = (s.get("reconcile") or {}).get("state", "?")
        if n % 30 == 0:
            trash_mb = dir_size_mb(VDIR + "/trash")
        st = os.statvfs(HOME)
        free_gb = st.f_bavail * st.f_frsize / 1e9
        db_mb = 0.0
        for suffix in ("", "-wal", "-shm"):
            try:
                db_mb += os.path.getsize(DB + suffix) / 1e6
            except OSError:
                pass
        load1 = os.getloadavg()[0]
        fh.write(
            f"{t:.1f},{pid or ''},{cpu_pct},{rss_mb},{q['pending']},{q['held']},{q['failed']},{q['index']},{q['hw']},"
            f"{throttle},{run},{rec},{free_gb:.2f},{db_mb:.1f},{trash_mb:.0f},{load1:.2f},{app_pct}\n"
        )
        n += 1
        time.sleep(max(0.1, interval - (time.time() - t)))


def cmd_mon(args):
    pidfile = HOME + "/.vt-mon.pid"
    if args.action == "stop":
        try:
            os.kill(int(open(pidfile).read()), 15)
            os.remove(pidfile)
            print("stopped")
        except (OSError, ValueError) as e:
            print("not running:", e)
        return
    if os.path.exists(pidfile):
        try:
            os.kill(int(open(pidfile).read()), 0)
            print("already running")
            return
        except (OSError, ValueError):
            pass
    if os.fork() > 0:
        print("started ->", args.csv)
        return
    os.setsid()
    if os.fork() > 0:
        os._exit(0)
    with open(pidfile, "w") as fh:
        fh.write(str(os.getpid()))
    devnull = os.open(os.devnull, os.O_RDWR)
    for fd in (0, 1, 2):
        os.dup2(devnull, fd)
    mon_loop(args.csv, args.interval)


LINE = re.compile(r"^(\d+) \[(\w+)\] \(([^)]*)\): (.*)$")


def logscan(since_ms=0, path=None):
    path = path or VDIR + "/logs/vapord.log"
    counts = {}
    samples = {}
    try:
        with open(path, errors="replace") as fh:
            for line in fh:
                m = LINE.match(line.rstrip("\n"))
                if not m:
                    continue
                ts, lvl, _, msg = m.groups()
                if int(ts) < since_ms or lvl not in ("WARNING", "WARN", "ERROR"):
                    continue
                key = (lvl, re.sub(r"[=:]\S+", "=…", msg)[:110])
                counts[key] = counts.get(key, 0) + 1
                samples.setdefault(key, line.strip()[:400])
    except OSError as e:
        return {"error": str(e)}
    return [
        {"level": k[0], "count": v, "sample": samples[k]}
        for k, v in sorted(counts.items(), key=lambda kv: -kv[1])
    ]


def cmd_logscan(args):
    print(json.dumps(logscan(args.since), indent=1))


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("st").set_defaults(fn=cmd_st)
    w = sub.add_parser("wait")
    w.add_argument("--timeout", type=float, default=600)
    w.add_argument("--quiet", type=float, default=10)
    w.set_defaults(fn=cmd_wait)
    c = sub.add_parser("compare")
    c.add_argument("--sub", default="")
    c.add_argument("--exclude", action="append", default=[])
    c.add_argument("--no-ignored", action="store_true")
    c.add_argument("--max-list", type=int, default=15)
    c.add_argument("--brief", action="store_true")
    c.set_defaults(fn=cmd_compare)
    m = sub.add_parser("mon")
    m.add_argument("action", choices=["start", "stop"])
    m.add_argument("--csv", default=OUT + "/mon.csv")
    m.add_argument("--interval", type=float, default=2.0)
    m.set_defaults(fn=cmd_mon)
    l = sub.add_parser("logscan")
    l.add_argument("--since", type=int, default=0)
    l.set_defaults(fn=cmd_logscan)
    args = ap.parse_args()
    args.fn(args)


if __name__ == "__main__":
    main()
