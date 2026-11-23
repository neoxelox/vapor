#!/usr/bin/env python3
"""wl_mover: a person who reorganizes a big folder all day.

Moves files and folders, renames in bulk, duplicates, deletes, takes a
folder out of the sync root and puts it back. Keeps a model (path ->
sha256) so the end state can be checked exactly.

  wl_mover.py seed  --name Library [--mb 300]
  wl_mover.py run   --name Library --seconds T [--pause-ms 200] [--seed S] [--side local|cloud]
  wl_mover.py check --name Library
"""
import argparse
import hashlib
import json
import os
import random
import shutil
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import vt  # noqa: E402

TOPICS = ["Photos/2023/Trip", "Photos/2024/Family", "Photos/2025/Work", "Photos/2026/Misc", "Documents/Taxes", "Documents/Contracts", "Documents/Notes", "Music/Albums", "Projects/alpha/assets", "Projects/beta/exports", "Downloads", "Archive/old"]
EXT = [".jpg", ".png", ".pdf", ".docx", ".mp3", ".txt", ".mov", ".zip"]


def blob(tag, size):
    seed = hashlib.sha256(tag.encode()).digest()
    reps = size // len(seed) + 1
    return (seed * reps)[:size]


def sha_bytes(b):
    return hashlib.sha256(b).hexdigest()


class Lib:
    def __init__(self, root, name, seed=1):
        self.base = root
        self.name = name
        self.root = os.path.join(root, name)
        self.rng = random.Random(seed)
        self.model = {}
        self.serial = 0
        self.stashed = []
        tag = name.replace("/", "_")
        self.model_path = os.path.join(vt.OUT, f"model-mover-{tag}.json")
        self.ops_path = os.path.join(vt.OUT, f"ops-mover-{tag}.jsonl")
        self.stash = os.path.expanduser(f"~/stash-mover-{tag}")
        self.counts = {}
        self.violations = []
        self._ops = None

    def load(self):
        if os.path.exists(self.model_path):
            with open(self.model_path) as fh:
                d = json.load(fh)
            self.model, self.serial, self.stashed = d["model"], d["serial"], d.get("stashed", [])

    def save(self):
        os.makedirs(vt.OUT, exist_ok=True)
        with open(self.model_path, "w") as fh:
            json.dump({"model": self.model, "serial": self.serial, "stashed": self.stashed}, fh)

    def log(self, op, **kw):
        if self._ops is None:
            os.makedirs(vt.OUT, exist_ok=True)
            self._ops = open(self.ops_path, "a", buffering=1)
        self.counts[op] = self.counts.get(op, 0) + 1
        self._ops.write(json.dumps({"t": round(time.time(), 3), "op": op, **kw}) + "\n")

    def full(self, rel):
        return os.path.join(self.root, rel)

    def size(self):
        r = self.rng.random()
        if r < 0.70:
            return self.rng.randint(1_000, 64_000)
        if r < 0.95:
            return self.rng.randint(64_000, 2_000_000)
        return self.rng.randint(2_000_000, 12_000_000)

    def put(self, rel, size=None):
        self.serial += 1
        data = blob(f"{self.name}/{rel}/{self.serial}", size if size is not None else self.size())
        os.makedirs(os.path.dirname(self.full(rel)), exist_ok=True)
        with open(self.full(rel), "wb") as fh:
            fh.write(data)
        self.model[rel] = sha_bytes(data)
        return len(data)

    def dirs(self, min_files=1, max_files=10**9):
        counts = {}
        for rel in self.model:
            d = os.path.dirname(rel)
            while d:
                counts[d] = counts.get(d, 0) + 1
                d = os.path.dirname(d)
        return [d for d, n in counts.items() if min_files <= n <= max_files]

    def move_prefix(self, src, dst):
        n = 0
        for rel in list(self.model):
            if rel == src or rel.startswith(src + "/"):
                self.model[dst + rel[len(src):]] = self.model.pop(rel)
                n += 1
        return n

    def verify_before(self, rel):
        want = self.model.get(rel)
        if want is None:
            return
        try:
            if os.path.getsize(self.full(rel)) > 2_000_000:
                return
        except OSError as e:
            self.violations.append({"t": time.time(), "kind": "vanished", "path": rel, "error": str(e)})
            self.log("VIOLATION", kind="vanished", path=rel)
            return
        got = vt.sha(self.full(rel))
        if got != want:
            self.violations.append({"t": time.time(), "kind": "changed-under-us", "path": rel})
            self.log("VIOLATION", kind="changed-under-us", path=rel, want=want, got=got)

    # -- operations -----------------------------------------------------

    def op_move_file(self):
        rel = self.rng.choice(list(self.model))
        ds = self.dirs()
        if not ds:
            return
        self.verify_before(rel)
        dst = os.path.join(self.rng.choice(ds), os.path.basename(rel))
        if dst in self.model or os.path.exists(self.full(dst)):
            return
        os.rename(self.full(rel), self.full(dst))
        self.model[dst] = self.model.pop(rel)
        self.log("move_file", src=rel, dst=dst)

    def op_rename_dir(self):
        ds = self.dirs(1, 600)
        if not ds:
            return
        d = self.rng.choice(ds)
        self.serial += 1
        dst = os.path.join(os.path.dirname(d), f"{os.path.basename(d).split(' #')[0]} #{self.serial}")
        if os.path.exists(self.full(dst)):
            return
        os.rename(self.full(d), self.full(dst))
        n = self.move_prefix(d, dst)
        self.log("rename_dir", src=d, dst=dst, files=n)

    def op_move_dir(self):
        ds = self.dirs(1, 600)
        if len(ds) < 2:
            return
        d = self.rng.choice(ds)
        parents = [p for p in ds if p != d and not p.startswith(d + "/") and os.path.dirname(d) != p]
        if not parents:
            return
        parent = self.rng.choice(parents)
        dst = os.path.join(parent, os.path.basename(d))
        if os.path.exists(self.full(dst)):
            return
        os.rename(self.full(d), self.full(dst))
        n = self.move_prefix(d, dst)
        self.log("move_dir", src=d, dst=dst, files=n)

    def op_batch_rename(self):
        ds = self.dirs(5, 400)
        if not ds:
            return
        d = self.rng.choice(ds)
        self.serial += 1
        tag = f"r{self.serial}_"
        rels = [r for r in self.model if os.path.dirname(r) == d]
        for rel in rels:
            dst = os.path.join(d, tag + os.path.basename(rel).split("_", 1)[-1])
            if dst in self.model or os.path.exists(self.full(dst)):
                continue
            os.rename(self.full(rel), self.full(dst))
            self.model[dst] = self.model.pop(rel)
        self.log("batch_rename", dir=d, files=len(rels))

    def op_duplicate_dir(self):
        ds = self.dirs(1, 150)
        if not ds:
            return
        d = self.rng.choice(ds)
        self.serial += 1
        dst = f"{d} copy {self.serial}"
        if os.path.exists(self.full(dst)):
            return
        shutil.copytree(self.full(d), self.full(dst), symlinks=True)
        n = 0
        for rel in list(self.model):
            if rel.startswith(d + "/"):
                self.model[dst + rel[len(d):]] = self.model[rel]
                n += 1
        self.log("duplicate_dir", src=d, dst=dst, files=n)

    def op_delete_dir(self):
        ds = self.dirs(1, 40)
        if not ds or len(self.model) < 400:
            return
        d = self.rng.choice(ds)
        shutil.rmtree(self.full(d), ignore_errors=True)
        n = 0
        for rel in list(self.model):
            if rel.startswith(d + "/"):
                del self.model[rel]
                n += 1
        self.log("delete_dir", dir=d, files=n)

    def op_stash_out(self):
        ds = self.dirs(1, 200)
        if not ds or len(self.stashed) >= 3:
            return
        d = self.rng.choice(ds)
        self.serial += 1
        slot = f"s{self.serial}"
        os.makedirs(self.stash, exist_ok=True)
        os.rename(self.full(d), os.path.join(self.stash, slot))
        files = {}
        for rel in list(self.model):
            if rel.startswith(d + "/"):
                files[rel[len(d) + 1:]] = self.model.pop(rel)
        self.stashed.append({"slot": slot, "name": os.path.basename(d), "files": files})
        self.log("stash_out", dir=d, files=len(files))

    def op_stash_back(self):
        if not self.stashed:
            return
        item = self.stashed.pop(self.rng.randrange(len(self.stashed)))
        ds = self.dirs() or [""]
        parent = self.rng.choice(ds)
        dst = os.path.join(parent, item["name"])
        if os.path.exists(self.full(dst)):
            self.serial += 1
            dst = f"{dst} back {self.serial}"
        os.makedirs(os.path.dirname(self.full(dst)), exist_ok=True)
        os.rename(os.path.join(self.stash, item["slot"]), self.full(dst))
        for rel, h in item["files"].items():
            self.model[os.path.join(dst, rel)] = h
        self.log("stash_back", dst=dst, files=len(item["files"]))

    def op_swap(self):
        if len(self.model) < 2:
            return
        a, b = self.rng.sample(list(self.model), 2)
        self.verify_before(a)
        self.verify_before(b)
        tmp = self.full(a) + ".swap"
        os.rename(self.full(a), tmp)
        os.rename(self.full(b), self.full(a))
        os.rename(tmp, self.full(b))
        self.model[a], self.model[b] = self.model[b], self.model[a]
        self.log("swap", a=a, b=b)

    def op_overwrite(self):
        rel = self.rng.choice(list(self.model))
        self.verify_before(rel)
        self.put(rel)
        self.log("overwrite", path=rel)

    def op_import(self):
        self.serial += 1
        d = os.path.join(self.rng.choice(TOPICS), f"import {self.serial}")
        n = self.rng.randint(10, 80)
        total = 0
        for i in range(n):
            total += self.put(os.path.join(d, f"IMG_{self.serial}_{i:04d}{self.rng.choice(EXT)}"), self.rng.randint(2_000, 300_000))
        self.log("import", dir=d, files=n, bytes=total)

    def op_case(self):
        ds = self.dirs(1, 300)
        if not ds:
            return
        d = self.rng.choice(ds)
        base = os.path.basename(d)
        new = base.swapcase()
        if new == base:
            return
        dst = os.path.join(os.path.dirname(d), new)
        os.rename(self.full(d), self.full(dst))
        n = self.move_prefix(d, dst)
        self.log("case_rename", src=d, dst=dst, files=n)


def seed(a):
    lib = Lib(a.root, a.name, a.seed)
    if os.path.exists(lib.root):
        print("exists; not seeding again")
        return
    total = 0
    i = 0
    while total < a.mb * 1_000_000:
        topic = lib.rng.choice(TOPICS)
        sub = f"set {lib.rng.randint(1, 8)}"
        total += lib.put(os.path.join(topic, sub, f"file_{i:05d}{lib.rng.choice(EXT)}"))
        i += 1
    lib.save()
    print(json.dumps({"seeded_files": len(lib.model), "mb": round(total / 1e6)}))


def run(a):
    lib = Lib(a.root, a.name, a.seed)
    lib.load()
    if not lib.model:
        print("seed first")
        sys.exit(2)
    weights = [
        (lib.op_move_file, 25), (lib.op_rename_dir, 10), (lib.op_move_dir, 10), (lib.op_batch_rename, 6),
        (lib.op_duplicate_dir, 4), (lib.op_delete_dir, 5), (lib.op_stash_out, 3), (lib.op_stash_back, 4),
        (lib.op_swap, 6), (lib.op_overwrite, 10), (lib.op_import, 5), (lib.op_case, 3),
    ]
    ops, w = zip(*weights)
    start = time.time()
    n = 0
    while time.time() - start < a.seconds:
        try:
            lib.rng.choices(ops, w)[0]()
        except OSError as e:
            lib.log("os-error", error=str(e))
        n += 1
        if n % 50 == 0:
            lib.save()
        if a.pause_ms:
            time.sleep(a.pause_ms / 1000 * lib.rng.uniform(0.2, 1.8))
    while lib.stashed:
        lib.op_stash_back()
    lib.save()
    print(json.dumps({"ops": n, "seconds": round(time.time() - start, 1), "counts": lib.counts, "files": len(lib.model), "violations": len(lib.violations)}))


def check(a):
    """Model against tree. Names compare without case: the volume folds
    case, and the model keeps the spelling of the request. `extra` is a
    file that the workload deleted or moved away and that is there
    again; a payload count that is not zero is content that was lost or
    that nobody wrote."""
    lib = Lib(a.root, a.name, a.seed)
    lib.load()
    files, _, _, _ = vt.scan(lib.root)
    hashes = vt.hash_many(lib.root, list(files))
    model = {k.casefold(): v for k, v in lib.model.items()}
    tree = {k.casefold(): hashes[k] for k in files}
    spelled = {k.casefold(): k for k in files}
    missing = sorted(set(model) - set(tree))
    extra = sorted(set(tree) - set(model))
    wrong = sorted(k for k in set(model) & set(tree) if model[k] != tree[k])
    lost = set(model.values()) - set(tree.values())
    unknown = set(tree.values()) - set(model.values())
    out = {
        "model_files": len(model), "tree_files": len(tree), "missing": len(missing), "extra": len(extra), "wrong": len(wrong),
        "payloads_lost": len(lost), "payloads_nobody_wrote": len(unknown),
        "samples": {"missing": missing[:10], "extra": [spelled[k] for k in extra[:10]], "wrong": [spelled[k] for k in wrong[:10]]},
        # An empty model is a wrong name, not a clean result.
        "ok": bool(model) and not (missing or extra or wrong),
    }
    print(json.dumps(out, indent=1))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("cmd", choices=["seed", "run", "check"])
    ap.add_argument("--name", required=True)
    ap.add_argument("--side", choices=["local", "cloud"], default="local")
    ap.add_argument("--mb", type=int, default=300)
    ap.add_argument("--seconds", type=float, default=60)
    ap.add_argument("--pause-ms", type=float, default=200)
    ap.add_argument("--seed", type=int, default=1)
    a = ap.parse_args()
    a.root = vt.LOCAL if a.side == "local" else vt.CLOUD
    {"seed": seed, "run": run, "check": check}[a.cmd](a)


if __name__ == "__main__":
    main()
