#!/usr/bin/env python3
"""lifecycle: what a user does to the daemon and to the folders.

  lifecycle.py list
  lifecycle.py run [--only id,id]
"""
import argparse
import json
import os
import shutil
import signal
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import vt  # noqa: E402
from edge import Ctx, L, C, w, r, exists, listing, decisions, sh  # noqa: E402
from conflicts import all_bytes, both_equal, copies  # noqa: E402

BASE = "edge/lc" + os.environ.get("EDGE_TAG", "")


def settle(timeout=300, quiet=8):
    return vt.wait_quiet(timeout, quiet, verbose=False)


def wait_for(cond, timeout, step=0.5):
    t0 = time.time()
    while time.time() - t0 < timeout:
        if cond():
            return time.time() - t0
        time.sleep(step)
    return None


def run_state():
    return (vt.vapor_json("status") or {}).get("run", {}).get("state")


def offline_changes(ctx, d, stop, start):
    """Both sides change different files while the daemon cannot see it."""
    for side, tag in ((L, "l"), (C, "c")):
        for i in range(30):
            w(os.path.join(side, d, f"{tag}-keep-{i:02d}.txt"), f"{tag} keep {i}\n")
        for i in range(6):
            w(os.path.join(side, d, f"{tag}-mod-{i}.txt"), f"{tag} before {i}\n")
            w(os.path.join(side, d, f"{tag}-del-{i}.txt"), f"{tag} to delete {i}\n")
            w(os.path.join(side, d, f"{tag}-ren-{i}.txt"), f"{tag} to rename {i}, unique\n")
        for i in range(8):
            w(os.path.join(side, d, f"{tag}-deldir", f"f{i}.txt"), f"{i}\n")
            w(os.path.join(side, d, f"{tag}-rendir", f"f{i}.txt"), f"{tag} in a folder to rename {i}\n")
    settle()
    if not both_equal(ctx, d):
        return
    stop()
    expected = dict(all_bytes(L, d))
    for side, tag in ((L, "l"), (C, "c")):
        for i in range(6):
            data = f"{tag} after {i}, changed while Vapor was away\n".encode()
            w(os.path.join(side, d, f"{tag}-mod-{i}.txt"), data)
            expected[f"{tag}-mod-{i}.txt"] = data
            os.remove(os.path.join(side, d, f"{tag}-del-{i}.txt"))
            expected.pop(f"{tag}-del-{i}.txt")
            os.rename(os.path.join(side, d, f"{tag}-ren-{i}.txt"), os.path.join(side, d, f"{tag}-renamed-{i}.txt"))
            expected[f"{tag}-renamed-{i}.txt"] = expected.pop(f"{tag}-ren-{i}.txt")
        for i in range(10):
            data = f"{tag} new {i}\n".encode()
            w(os.path.join(side, d, f"{tag}-new-{i}.txt"), data)
            expected[f"{tag}-new-{i}.txt"] = data
        shutil.rmtree(os.path.join(side, d, f"{tag}-deldir"))
        os.rename(os.path.join(side, d, f"{tag}-rendir"), os.path.join(side, d, f"{tag}-rendir-new"))
        for i in range(8):
            expected.pop(f"{tag}-deldir/f{i}.txt")
            expected[f"{tag}-rendir-new/f{i}.txt"] = expected.pop(f"{tag}-rendir/f{i}.txt")
    t0 = time.time()
    start()
    ok, secs, st = settle()
    ctx.info(f"quiet {time.time() - t0:.0f}s after the daemon came back")
    for side, name in ((L, "this device"), (C, "the cloud")):
        got = all_bytes(side, d)
        missing = sorted(set(expected) - set(got))
        extra = sorted(set(got) - set(expected))
        wrong = sorted(n for n in set(got) & set(expected) if got[n] != expected[n])
        ctx.expect(not missing and not extra and not wrong, f"{name} holds exactly the result of both sides' changes (missing {missing[:6]}, extra {extra[:6]}, wrong {wrong[:6]})")
    ds = [x for x in decisions() if d in json.dumps(x)]
    ctx.expect(not ds, f"no question was needed ({[x.get('kind') for x in ds]})")


def lc_pause(ctx):
    offline_changes(ctx, f"{BASE}/pause", lambda: vt.vapor("pause"), lambda: vt.vapor("resume"))


def lc_stop(ctx):
    def stop():
        code, out, err = vt.vapor("service", "stop")
        ctx.expect(code == 0, f"`vapor service stop` exits 0 ({code} {(out + err).strip()[:150]})")
        time.sleep(1)

    def start():
        vt.vapor("service", "start")
        wait_for(lambda: run_state() == "running", 30)

    offline_changes(ctx, f"{BASE}/stop", stop, start)


def lc_kill_storm(ctx):
    d = f"{BASE}/kill-storm"
    w(os.path.join(L, d, "seed.txt"), "seed\n")
    settle()
    for i in range(1500):
        w(os.path.join(L, d, f"d{i % 15:02d}", f"f{i:04d}.txt"), f"storm {i}\n" * 30)
    time.sleep(2.5)
    pid = vt.daemon_pid()
    before = vt.queue_counts()
    os.kill(pid, signal.SIGKILL)
    back = wait_for(lambda: vt.daemon_pid() not in (None, pid), 120)
    ctx.expect(back is not None, f"the supervisor starts a new daemon after a kill -9 ({'%.0f s' % back if back else 'never within 120 s'})")
    if back is None:
        vt.vapor("service", "start")
    ctx.info(f"queue at the kill: {before['pending']} pending, index {before['index']}")
    up = wait_for(lambda: run_state() == "running", 60)
    ctx.info(f"running again {('%.0f s' % (back + up)) if back is not None and up is not None else '?'} after the kill")
    ok, secs, st = settle(600)
    n = sum(len(fs) for _, _, fs in os.walk(os.path.join(C, d)))
    ctx.expect(n == 1501, f"all 1501 files reach the cloud after the crash ({n})")
    both_equal(ctx, d)
    tmp = [f for _, _, fs in os.walk(os.path.join(C, d)) for f in fs if f.startswith(".vapor-tmp")]
    ctx.expect(not tmp, f"no temporary file stays in the cloud folder ({len(tmp)})")


def lc_kill_big(ctx):
    d = f"{BASE}/kill-big"
    w(os.path.join(L, d, "seed.txt"), "seed\n")
    settle()
    blob = os.urandom(1 << 20) * 500
    w(os.path.join(L, d, "big.bin"), blob)
    time.sleep(3.0)
    pid = vt.daemon_pid()
    partial = [(f, os.path.getsize(os.path.join(C, d, f))) for f in os.listdir(os.path.join(C, d))] if exists(os.path.join(C, d)) else []
    os.kill(pid, signal.SIGKILL)
    ctx.info(f"cloud folder at the kill: {partial}")
    back = wait_for(lambda: vt.daemon_pid() not in (None, pid), 120)
    ctx.expect(back is not None, f"the supervisor starts a new daemon ({back})")
    if back is None:
        vt.vapor("service", "start")
    wait_for(lambda: run_state() == "running", 60)
    settle(600)
    ctx.expect(r(os.path.join(C, d, "big.bin")) == blob, "the 500 MiB file is whole in the cloud after the crash")
    left = sorted(os.listdir(os.path.join(C, d)))
    ctx.expect(left == ["big.bin", "seed.txt"], f"nothing else is in the cloud folder ({left})")
    for side in (L, C):
        try:
            os.remove(os.path.join(side, d, "big.bin"))
        except OSError:
            pass


def lc_crash_loop(ctx):
    kills = 0
    t0 = time.time()
    states = []
    while kills < 7 and time.time() - t0 < 400:
        pid = wait_for(vt.daemon_pid, 90)
        pid = vt.daemon_pid()
        if pid is None:
            break
        time.sleep(1.0)
        try:
            os.kill(pid, signal.SIGKILL)
            kills += 1
        except OSError:
            pass
        gone = wait_for(lambda: vt.daemon_pid() != pid, 10)
        states.append(round(time.time() - t0))
    code, out, err = vt.vapor("service", "status")
    ctx.info(f"kills: {kills} at seconds {states}")
    ctx.info(f"`vapor service status` after the kills: {out.strip()!r}")
    code2, out2, err2 = vt.vapor("status")
    ctx.info(f"`vapor status` after the kills: {(out2 + err2).strip()[:300]!r}")
    paused = "paus" in out.lower() or "crash" in out.lower()
    ctx.expect(paused, "after repeated crashes the service says that it stopped restarting")
    code, out, err = vt.vapor("service", "acknowledge")
    ctx.info(f"`vapor service acknowledge`: {code} {(out + err).strip()[:200]!r}")
    vt.vapor("service", "start")
    up = wait_for(lambda: run_state() == "running", 60)
    ctx.expect(up is not None, "after `vapor service acknowledge` and a start the daemon runs again")
    settle()


def lc_cloud_root_away(ctx):
    d = f"{BASE}/root-away"
    for i in range(20):
        w(os.path.join(L, d, f"f{i}.txt"), f"{i}\n")
    settle()
    away = C + ".away"
    os.rename(C, away)
    held = wait_for(lambda: any(x.get("kind") == "root-missing" for x in decisions()), 60)
    ctx.expect(held is not None, f"a `root-missing` question opens when the cloud folder goes away ({'%.0f s' % held if held else 'not within 60 s'})")
    st = vt.vapor_json("status") or {}
    ctx.info(f"status while the cloud folder is away: {st.get('run')}")
    ctx.expect(not exists(C), "Vapor does not make a new cloud folder on its own")
    for i in range(20, 30):
        w(os.path.join(L, d, f"f{i}.txt"), f"{i}, written while the cloud folder was away\n")
    os.remove(os.path.join(L, d, "f0.txt"))
    time.sleep(20)
    ctx.expect(len(listing(os.path.join(L, d)) or []) == 29, "the local files stay as the user left them")
    ctx.expect(not exists(C), "still no new cloud folder after 20 s")
    os.rename(away, C)
    lifted = wait_for(lambda: run_state() == "running" and not any(x.get("kind") == "root-missing" for x in decisions()), 90)
    ctx.expect(lifted is not None, f"the hold lifts on its own when the folder is back ({'%.0f s' % lifted if lifted else 'not within 90 s'})")
    settle()
    both_equal(ctx, d)
    ctx.expect(len(all_bytes(C, d)) == 29, f"the cloud has the 29 files ({len(all_bytes(C, d))})")


def lc_local_root_replaced(ctx):
    d = f"{BASE}/root-replaced"
    for i in range(20):
        w(os.path.join(L, d, f"f{i}.txt"), f"{i}\n")
    settle()
    n_cloud = sum(len(fs) for _, _, fs in os.walk(C))
    aside = L + ".aside"
    os.rename(L, aside)
    os.makedirs(L)
    w(os.path.join(L, "stranger.txt"), "a different folder at the same path\n")
    held = wait_for(lambda: any(x.get("kind") in ("root-replaced", "root-missing") for x in decisions()), 60)
    kinds = [x.get("kind") for x in decisions() if x.get("kind", "").startswith("root-")]
    ctx.expect(held is not None, f"a question opens when another folder sits at the local path ({kinds})")
    time.sleep(25)
    n_after = sum(len(fs) for _, _, fs in os.walk(C))
    ctx.expect(n_after == n_cloud, f"the cloud keeps every file ({n_cloud} before, {n_after} after)")
    ctx.expect(not exists(os.path.join(C, "stranger.txt")), "the stranger's file does not reach the cloud")
    shutil.rmtree(L)
    os.rename(aside, L)
    lifted = wait_for(lambda: run_state() == "running" and not any(x.get("kind", "").startswith("root-") for x in decisions()), 90)
    ctx.expect(lifted is not None, f"the hold lifts on its own when the real folder is back ({lifted})")
    settle()
    both_equal(ctx, d)


def lc_ignore_live(ctx):
    d = f"{BASE}/ignore"
    w(os.path.join(L, d, "a.secret"), "synced before the rule\n")
    w(os.path.join(L, d, "plain.txt"), "plain\n")
    settle()
    ctx.expect(exists(os.path.join(C, d, "a.secret")), "setup: a.secret synced")
    code, out, err = vt.vapor("config", "set", "postIgnoreRules", '["*.secret"]')
    ctx.info(f"`vapor config set postIgnoreRules`: {(out + err).strip()[:200]}")
    time.sleep(6)
    w(os.path.join(L, d, "b.secret"), "written after the rule\n")
    w(os.path.join(L, d, "a.secret"), "changed after the rule\n")
    w(os.path.join(C, d, "c.secret"), "written in the cloud after the rule\n")
    settle()
    ctx.expect(not exists(os.path.join(C, d, "b.secret")), "a new file that the rule hides does not upload")
    ctx.expect(not exists(os.path.join(L, d, "c.secret")), "a cloud file that the rule hides does not download")
    ctx.expect(r(os.path.join(C, d, "a.secret")) == b"synced before the rule\n", "a file that synced before the rule stays as it was in the cloud")
    ig = vt.vapor_json("ignored") or {}
    ctx.info(f"`vapor ignored` lists {len(ig.get('entries', []))} entries; names: {[os.path.basename(e.get('path', '')) for e in ig.get('entries', [])][:8]}")
    vt.vapor("config", "set", "postIgnoreRules", "[]")
    settle()
    time.sleep(3)
    settle()
    ctx.expect(r(os.path.join(C, d, "b.secret")) == b"written after the rule\n", "with the rule gone, the hidden file uploads without a restart")
    ctx.expect(r(os.path.join(L, d, "c.secret")) == b"written in the cloud after the rule\n", "and the hidden cloud file downloads")
    # .gitignore and .vaporignore inside a project
    w(os.path.join(L, d, "proj/.gitignore"), "secret/\n*.key\n")
    time.sleep(5)
    w(os.path.join(L, d, "proj/secret/x.txt"), "ignored by .gitignore\n")
    w(os.path.join(L, d, "proj/id.key"), "ignored by .gitignore\n")
    w(os.path.join(L, d, "proj/main.rs"), "fn main() {}\n")
    settle()
    ctx.expect(not exists(os.path.join(C, d, "proj/secret")) and not exists(os.path.join(C, d, "proj/id.key")), "what a .gitignore hides does not upload")
    ctx.expect(exists(os.path.join(C, d, "proj/main.rs")) and exists(os.path.join(C, d, "proj/.gitignore")), "the rest of the project uploads, with the .gitignore")
    w(os.path.join(L, d, "proj/.vaporignore"), "!*.key\n")
    settle()
    time.sleep(3)
    settle()
    ctx.expect(exists(os.path.join(C, d, "proj/id.key")), "a `!rule` in .vaporignore brings a file back")


def cases():
    return [
        ("LC-pause", "both sides change while paused", lc_pause),
        ("LC-stop", "both sides change while the daemon is stopped", lc_stop),
        ("LC-kill-storm", "kill -9 while 1500 files upload", lc_kill_storm),
        ("LC-kill-big", "kill -9 while a 500 MiB file uploads", lc_kill_big),
        ("LC-cloud-away", "the cloud folder goes away and comes back", lc_cloud_root_away),
        ("LC-local-replaced", "another folder sits at the local path", lc_local_root_replaced),
        ("LC-ignore", "ignore rules change while Vapor runs", lc_ignore_live),
        ("LC-crash-loop", "the daemon is killed again and again", lc_crash_loop),
    ]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("cmd", choices=["list", "run"])
    ap.add_argument("--only", default="")
    a = ap.parse_args()
    cs = cases()
    if a.cmd == "list":
        for cid, title, _ in cs:
            print(cid, title)
        return
    only = [o for o in a.only.split(",") if o]
    if only:
        cs = [c for c in cs if any(c[0] == o or c[0].startswith(o) for o in only)]
    start_ms = int(time.time() * 1000)
    out = []
    for cid, title, fn in cs:
        ctx = Ctx(cid, title)
        t0 = time.time()
        try:
            fn(ctx)
        except Exception as e:  # noqa: BLE001
            import traceback
            ctx.fails.append(f"raised {type(e).__name__}: {e} {traceback.format_exc()[-300:]}")
        status = "FAIL" if ctx.fails else "PASS"
        out.append({"id": cid, "title": title, "status": status, "fails": ctx.fails, "infos": ctx.infos, "notes": ctx.notes, "seconds": round(time.time() - t0, 1)})
        print(f"{status} {cid} ({time.time() - t0:.0f}s): {title}", flush=True)
        for f in ctx.fails:
            print(f"     - {f}", flush=True)
        for i in ctx.infos:
            print(f"     i {i}", flush=True)
    report = {"cases": out, "log": vt.logscan(start_ms), "status": vt.status_line()}
    path = os.path.join(vt.OUT, f"lifecycle-{int(time.time())}.json")
    with open(path, "w") as fh:
        json.dump(report, fh, indent=1, ensure_ascii=False)
    print(f"== lifecycle: report {path}")
    print("log:", json.dumps(report["log"], ensure_ascii=False)[:3000])


if __name__ == "__main__":
    main()
