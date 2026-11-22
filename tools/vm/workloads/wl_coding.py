#!/usr/bin/env python3
"""wl_coding: a person or an agent that writes code in a project.

Works in one project folder under a sync root, with a real git
repository. It keeps a model of what it wrote (path -> sha256) and, before
it touches a file again, checks that the file still holds what it wrote:
a sync engine that puts an older version back is caught in the act.

  wl_coding.py seed  --project P [--files N]
  wl_coding.py run   --project P --seconds T --rate R [--seed S] [--git] [--side local|cloud]
  wl_coding.py check --project P            model against the tree, git on both sides
"""
import argparse
import hashlib
import json
import os
import random
import shutil
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import vt  # noqa: E402

WORDS = "sync queue intent planner upload download reconcile throttle debounce watcher provider index hash lease retry profile folder remote local cursor marker".split()
EXTS = [".ts", ".tsx", ".rs", ".py", ".md", ".json", ".css", ".toml", ".yaml", ".html", ".go", ".swift"]
DIRS = ["src", "src/components", "src/lib", "src/server", "src/server/routes", "tests", "tests/unit", "docs", "scripts", "config", "packages/core/src", "packages/ui/src", "packages/ui/src/widgets"]


def sha_bytes(b):
    return hashlib.sha256(b).hexdigest()


class Project:
    def __init__(self, root, name, seed=1):
        self.root = os.path.join(root, name)
        self.name = name
        self.rng = random.Random(seed)
        self.model = {}
        self.model_path = os.path.join(vt.OUT, f"model-coding-{name.replace('/', '_')}.json")
        self.ops_path = os.path.join(vt.OUT, f"ops-coding-{name.replace('/', '_')}.jsonl")
        self.violations = []
        self.counts = {}
        self.serial = 0
        self._ops = None

    def load(self):
        if os.path.exists(self.model_path):
            with open(self.model_path) as fh:
                data = json.load(fh)
            self.model = data["model"]
            self.serial = data.get("serial", 0)

    def save(self):
        os.makedirs(vt.OUT, exist_ok=True)
        with open(self.model_path, "w") as fh:
            json.dump({"model": self.model, "serial": self.serial}, fh)

    def log(self, op, **kw):
        if self._ops is None:
            os.makedirs(vt.OUT, exist_ok=True)
            self._ops = open(self.ops_path, "a", buffering=1)
        self.counts[op] = self.counts.get(op, 0) + 1
        self._ops.write(json.dumps({"t": round(time.time(), 3), "op": op, **kw}) + "\n")

    def full(self, rel):
        return os.path.join(self.root, rel)

    def content(self, rel, size=None):
        self.serial += 1
        rng = self.rng
        n = size or rng.choice([3, 8, 20, 40, 80, 200, 600])
        lines = [f"// {rel} rev {self.serial}"]
        for i in range(n):
            lines.append(f"export const {rng.choice(WORDS)}_{i} = {rng.choice(WORDS)}({rng.randint(0, 99999)}); // {rng.choice(WORDS)} {rng.choice(WORDS)}")
        return ("\n".join(lines) + "\n").encode()

    def new_rel(self):
        rng = self.rng
        d = rng.choice(DIRS)
        if rng.random() < 0.15:
            d = os.path.join(d, rng.choice(WORDS))
        self.serial += 1
        return os.path.join(d, f"{rng.choice(WORDS)}_{rng.choice(WORDS)}_{self.serial}{rng.choice(EXTS)}")

    # -- the check before a file is touched again -----------------------

    def verify_before(self, rel):
        want = self.model.get(rel)
        if want is None:
            return
        try:
            with open(self.full(rel), "rb") as fh:
                got = sha_bytes(fh.read())
        except OSError as e:
            self.violations.append({"t": time.time(), "kind": "vanished", "path": rel, "error": str(e)})
            self.log("VIOLATION", kind="vanished", path=rel, error=str(e))
            return
        if got != want:
            head = b""
            try:
                with open(self.full(rel), "rb") as fh:
                    head = fh.readline()[:120]
            except OSError:
                pass
            self.violations.append({"t": time.time(), "kind": "changed-under-us", "path": rel, "want": want, "got": got, "head": head.decode(errors="replace")})
            self.log("VIOLATION", kind="changed-under-us", path=rel, want=want, got=got, head=head.decode(errors="replace"))

    # -- ways to save a file --------------------------------------------

    def save_in_place(self, rel, data):
        with open(self.full(rel), "wb") as fh:
            fh.write(data)

    def save_atomic(self, rel, data):
        tmp = self.full(rel) + f".{os.getpid()}.{self.rng.randint(1000, 99999999)}"
        with open(tmp, "wb") as fh:
            fh.write(data)
            fh.flush()
            os.fsync(fh.fileno())
        os.replace(tmp, self.full(rel))

    def save_jetbrains(self, rel, data):
        f = self.full(rel)
        tmp, old = f + "___jb_tmp___", f + "___jb_old___"
        with open(tmp, "wb") as fh:
            fh.write(data)
        os.rename(f, old)
        os.rename(tmp, f)
        os.remove(old)

    def save_vim(self, rel, data):
        f = self.full(rel)
        probe = os.path.join(os.path.dirname(f), "4913")
        try:
            open(probe, "wb").close()
            os.remove(probe)
        except OSError:
            pass
        backup = f + "~"
        os.rename(f, backup)
        with open(f, "wb") as fh:
            fh.write(data)
        os.remove(backup)

    def write(self, rel, data, how=None):
        os.makedirs(os.path.dirname(self.full(rel)), exist_ok=True)
        exists = os.path.exists(self.full(rel))
        how = how or (self.rng.choice(["in_place", "in_place", "atomic", "atomic", "jetbrains", "vim"]) if exists else "in_place")
        if not exists:
            how = "in_place"
        getattr(self, "save_" + how)(rel, data)
        self.model[rel] = sha_bytes(data)
        return how

    # -- operations -----------------------------------------------------

    def pick(self):
        return self.rng.choice(list(self.model)) if self.model else None

    def op_edit(self):
        rel = self.pick()
        if not rel:
            return self.op_create()
        self.verify_before(rel)
        how = self.write(rel, self.content(rel))
        self.log("edit", path=rel, how=how)

    def op_append(self):
        rel = self.pick()
        if not rel:
            return
        self.verify_before(rel)
        try:
            with open(self.full(rel), "rb") as fh:
                data = fh.read()
        except OSError:
            return
        tail = self.content(rel, size=self.rng.randint(1, 5))
        with open(self.full(rel), "ab") as fh:
            fh.write(tail)
        self.model[rel] = sha_bytes(data + tail)
        self.log("append", path=rel)

    def op_create(self):
        rel = self.new_rel()
        self.write(rel, self.content(rel))
        self.log("create", path=rel)

    def op_delete(self):
        rel = self.pick()
        if not rel or len(self.model) < 50:
            return
        self.verify_before(rel)
        try:
            os.remove(self.full(rel))
        except OSError:
            pass
        self.model.pop(rel, None)
        self.log("delete", path=rel)

    def op_rename(self):
        rel = self.pick()
        if not rel:
            return
        self.verify_before(rel)
        dst = self.new_rel()
        os.makedirs(os.path.dirname(self.full(dst)), exist_ok=True)
        try:
            os.rename(self.full(rel), self.full(dst))
        except OSError:
            return
        self.model[dst] = self.model.pop(rel)
        self.log("rename", src=rel, dst=dst)

    def op_burst(self):
        """A formatter or an agent that rewrites many files in one go."""
        n = min(len(self.model), self.rng.choice([20, 50, 120, 250]))
        rels = self.rng.sample(list(self.model), n)
        how = self.rng.choice(["in_place", "atomic"])
        for rel in rels:
            self.verify_before(rel)
            self.write(rel, self.content(rel), how)
        self.log("burst", files=n, how=how)

    def op_scaffold(self):
        """A new module: a folder with several files at once."""
        self.serial += 1
        d = os.path.join(self.rng.choice(DIRS), f"{self.rng.choice(WORDS)}_module_{self.serial}")
        n = self.rng.randint(5, 30)
        for i in range(n):
            rel = os.path.join(d, f"part_{i}{self.rng.choice(EXTS)}")
            self.write(rel, self.content(rel))
        self.log("scaffold", dir=d, files=n)

    def op_rename_dir(self):
        dirs = sorted({os.path.dirname(r) for r in self.model if r.count("/") >= 2})
        if not dirs:
            return
        d = self.rng.choice(dirs)
        self.serial += 1
        dst = os.path.join(os.path.dirname(d), f"renamed_{self.serial}")
        if os.path.exists(self.full(dst)):
            return
        try:
            os.rename(self.full(d), self.full(dst))
        except OSError:
            return
        moved = 0
        for rel in list(self.model):
            if rel.startswith(d + "/"):
                self.model[dst + rel[len(d):]] = self.model.pop(rel)
                moved += 1
        self.log("rename_dir", src=d, dst=dst, files=moved)

    # -- git ------------------------------------------------------------

    def git(self, *args, check=False):
        p = subprocess.run(["git", "-C", self.root, *args], capture_output=True, text=True)
        if check and p.returncode != 0:
            self.log("git-error", args=list(args), out=(p.stdout + p.stderr)[-400:])
        return p.returncode, p.stdout + p.stderr

    def op_commit(self):
        self.git("add", "-A")
        code, out = self.git("commit", "-q", "-m", f"work {self.serial}", check=False)
        self.log("git_commit", code=code)

    def resync_model(self):
        """After git changed the working tree, the tree is the model. A
        version that sync put back before the switch is committed with
        the rest and is not reported: the check before each touch
        covers the time between two switches only."""
        code, out = self.git("ls-files", "-z")
        model = {}
        for rel in out.split("\0"):
            if not rel or "probe_" in rel:
                continue
            try:
                with open(self.full(rel), "rb") as fh:
                    model[rel] = sha_bytes(fh.read())
            except OSError:
                pass
        self.model = model

    def op_branch_flip(self):
        """Commit, then switch to another branch and back: git rewrites
        many files of the working tree at once."""
        self.op_commit()
        code, out = self.git("branch", "--show-current")
        cur = out.strip()
        code, out = self.git("branch", "--format=%(refname:short)")
        branches = [b for b in out.split() if b]
        if len(branches) < 3 and self.rng.random() < 0.7:
            self.serial += 1
            new = f"feature-{self.serial}"
            self.git("checkout", "-q", "-b", new, check=True)
            self.log("git_branch_new", branch=new)
        else:
            target = self.rng.choice([b for b in branches if b != cur] or branches)
            self.git("checkout", "-q", target, check=True)
            self.log("git_checkout", branch=target)
        self.resync_model()

    def op_gc(self):
        self.op_commit()
        code, _ = self.git("gc", "-q", "--prune=now")
        self.log("git_gc", code=code)


def seed(args):
    p = Project(args.root, args.project, args.seed)
    if os.path.exists(p.root):
        print("project exists; not seeding again")
        return
    os.makedirs(p.root)
    for d in DIRS:
        os.makedirs(p.full(d), exist_ok=True)
    p.write("README.md", b"# demo project\n")
    p.write("package.json", json.dumps({"name": "demo", "version": "1.0.0", "private": True}, indent=2).encode())
    p.write(".gitignore", b"node_modules/\n*.log\n.env.local\n")
    for _ in range(args.files):
        rel = p.new_rel()
        p.write(rel, p.content(rel))
    if args.git:
        subprocess.run(["git", "init", "-q", "-b", "main", p.root], check=True)
        p.git("config", "user.email", "dev@example.com")
        p.git("config", "user.name", "Dev")
        p.git("config", "gc.auto", "0")
        p.git("add", "-A")
        p.git("commit", "-q", "-m", "initial", check=True)
    p.save()
    print(json.dumps({"seeded": len(p.model), "root": p.root}))


def run(args):
    p = Project(args.root, args.project, args.seed)
    p.load()
    if not p.model:
        print("no model: seed the project first")
        sys.exit(2)
    use_git = os.path.isdir(p.full(".git"))
    weights = [
        (p.op_edit, 46), (p.op_append, 10), (p.op_create, 12), (p.op_delete, 6),
        (p.op_rename, 6), (p.op_burst, 2 if args.rate < 20 else 1), (p.op_scaffold, 3), (p.op_rename_dir, 1),
    ]
    ops, w = zip(*weights)
    start = time.time()
    next_commit = start + args.commit_every
    next_flip = start + args.flip_every
    next_gc = start + args.gc_every
    n = 0
    interval = 1.0 / args.rate
    while time.time() - start < args.seconds:
        t = time.time()
        try:
            p.rng.choices(ops, w)[0]()
        except OSError as e:
            p.log("os-error", error=str(e))
        n += 1
        if use_git and t >= next_gc:
            p.op_gc()
            next_gc = time.time() + args.gc_every
        elif use_git and t >= next_flip:
            p.op_branch_flip()
            next_flip = time.time() + args.flip_every
        elif use_git and t >= next_commit:
            p.op_commit()
            next_commit = time.time() + args.commit_every
        if n % 200 == 0:
            p.save()
        sleep = interval - (time.time() - t)
        if sleep > 0:
            time.sleep(sleep * p.rng.uniform(0.2, 1.8))
    if use_git:
        p.op_commit()
    p.save()
    print(json.dumps({"ops": n, "seconds": round(time.time() - start, 1), "counts": p.counts, "files": len(p.model), "violations": len(p.violations)}))
    for v in p.violations[:20]:
        print("VIOLATION", json.dumps(v))


def check(args):
    p = Project(args.root, args.project, args.seed)
    p.load()
    out = {"model_files": len(p.model)}
    # 1. The side that was written holds what the workload wrote.
    wrong, missing = [], []
    for rel, want in p.model.items():
        got = vt.sha(p.full(rel))
        if got.startswith("ERR:"):
            missing.append(rel)
        elif got != want:
            wrong.append(rel)
    out["model_missing"] = len(missing)
    out["model_wrong"] = len(wrong)
    out["samples"] = {"missing": missing[:10], "wrong": wrong[:10]}
    # 2. Files the workload did not write (outside .git).
    extra = []
    for base, dnames, fnames in os.walk(p.root):
        if ".git" in dnames:
            dnames.remove(".git")
        for f in fnames:
            rel = os.path.relpath(os.path.join(base, f), p.root)
            if rel not in p.model:
                extra.append(rel)
    out["extra_files"] = len(extra)
    out["samples"]["extra"] = extra[:15]
    # 3. git on both sides.
    other_root = vt.CLOUD if args.root == vt.LOCAL else vt.LOCAL
    for label, root in (("written", p.root), ("other", os.path.join(other_root, args.project))):
        if not os.path.isdir(os.path.join(root, ".git")):
            out[f"git_{label}"] = "no .git folder"
            continue
        g = lambda *a: subprocess.run(["git", "--no-optional-locks", "-C", root, *a], capture_output=True, text=True)  # noqa: E731
        st = g("status", "--porcelain")
        fsck = g("fsck", "--full", "--no-dangling")
        head = g("rev-parse", "HEAD")
        out[f"git_{label}"] = {
            "head": head.stdout.strip()[:12],
            "status_code": st.returncode,
            "dirty": len([l for l in st.stdout.splitlines() if l.strip()]),
            "dirty_sample": st.stdout.splitlines()[:8] + st.stderr.splitlines()[:4],
            "fsck_code": fsck.returncode,
            "fsck": (fsck.stdout + fsck.stderr).strip().splitlines()[:8],
        }
    git_ok = all(
        isinstance(out.get(f"git_{label}"), dict)
        and out[f"git_{label}"]["status_code"] == 0
        and out[f"git_{label}"]["dirty"] == 0
        and out[f"git_{label}"]["fsck_code"] == 0
        and not out[f"git_{label}"]["fsck"]
        for label in ("written", "other")
    ) and out["git_written"]["head"] == out["git_other"]["head"] if os.path.isdir(p.full(".git")) else True
    out["git_ok"] = git_ok
    # An empty model is a wrong project name, not a clean result.
    out["ok"] = bool(p.model) and git_ok and not (missing or wrong or extra)
    print(json.dumps(out, indent=1))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("cmd", choices=["seed", "run", "check"])
    ap.add_argument("--project", required=True)
    ap.add_argument("--side", choices=["local", "cloud"], default="local")
    ap.add_argument("--files", type=int, default=400)
    ap.add_argument("--seconds", type=float, default=60)
    ap.add_argument("--rate", type=float, default=5)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--git", action="store_true")
    ap.add_argument("--commit-every", type=float, default=25)
    ap.add_argument("--flip-every", type=float, default=140)
    ap.add_argument("--gc-every", type=float, default=600)
    a = ap.parse_args()
    a.root = vt.LOCAL if a.side == "local" else vt.CLOUD
    {"seed": seed, "run": run, "check": check}[a.cmd](a)


if __name__ == "__main__":
    main()
