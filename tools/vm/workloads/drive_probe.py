#!/usr/bin/env python3
"""drive_probe: asks a real Google Drive account the questions that the
Drive provider answers from the API documents. It sends the requests the
provider sends (`core/providers/src/gdrive/mod.rs`) and prints one line
for each fact: what the provider takes for true, and what Drive did.

  <command that prints the sign-in> | drive_probe.py
  <command that prints the sign-in> | drive_probe.py race [rounds]
  <command that prints the sign-in> | drive_probe.py extra
  <command that prints the sign-in> | drive_probe.py cloud touch <path>
  <command that prints the sign-in> | drive_probe.py cloud rename <path> <name>
  <command that prints the sign-in> | drive_probe.py cloud move <path> <folder>
  <command that prints the sign-in> | drive_probe.py cloud overwrite <path> <text> [<time>]
  <command that prints the sign-in> | drive_probe.py cloud overwrite-after <path> <text>
  <command that prints the sign-in> | drive_probe.py cloud versions <path>
  <command that prints the sign-in> | drive_probe.py cloud list <folder>
  <command that prints the sign-in> | drive_probe.py cloud trash <path>
  <command that prints the sign-in> | drive_probe.py cloud heads <synced folder>

`race` repeats the same-moment overwrite and prints what Drive says
about each round. `extra` prints things no code depends on: the time
of a version whose upload session opened long before its bytes went,
the entries the changes feed has for a folder, and whether the older
API version has a conditional write of content. `cloud` changes one
file of a synced folder as another program does, to see what Vapor
makes of it: a new time with no new bytes, a new name, another folder,
new bytes with a time of the writer's choice, new bytes at the moment
an upload of the file lands. `cloud trash` puts the
folder of a session in the trash of the account when the session is
over. `<path>` starts at the top of the Drive.

The sign-in is one JSON document on stdin: the token that Vapor stores
(`accessToken`, `refreshToken`, `expiresAtMs`) with the OAuth client of
the build next to it (`clientId`, `clientSecret`). Nothing of it is
printed or written to disk. The probe works in a folder of its own,
`VaporProbe-<time>` at the top of the Drive, and puts it in the trash of
the account at the end. Only for the dedicated test account.
"""
import calendar
import hashlib
import json
import os
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import vt  # noqa: E402,F401  (refuses to start outside a guest)

API = "https://www.googleapis.com/drive/v3"
UPLOAD = "https://www.googleapis.com/upload/drive/v3"
FOLDER = "application/vnd.google-apps.folder"
FILE_FIELDS = "id,name,mimeType,size,md5Checksum,appProperties,parents,trashed,modifiedTime"
REVISION_FIELDS = "nextPageToken,revisions(id,md5Checksum,modifiedTime,size)"
TOKEN = {"access": ""}
RESULTS = []


def call(method, url, headers=None, body=None):
    """One request. Returns (status, headers, body bytes); never raises
    for an HTTP status."""
    req = urllib.request.Request(url, data=body, method=method)
    req.add_header("Authorization", "Bearer " + TOKEN["access"])
    for k, v in (headers or {}).items():
        req.add_header(k, v)
    try:
        with urllib.request.urlopen(req, timeout=120) as resp:
            return resp.status, dict(resp.headers), resp.read()
    except urllib.error.HTTPError as e:
        return e.code, dict(e.headers), e.read()


def js(method, url, payload=None):
    body = json.dumps(payload).encode() if payload is not None else None
    status, _, raw = call(method, url, {"Content-Type": "application/json"} if body else None, body)
    if status >= 300:
        raise RuntimeError(f"{method} {url.split('?')[0]} answered {status}: {raw[:300]!r}")
    return json.loads(raw) if raw else {}


def refresh(sign_in):
    form = urllib.parse.urlencode({
        "grant_type": "refresh_token",
        "client_id": sign_in["clientId"],
        "client_secret": sign_in["clientSecret"],
        "refresh_token": sign_in["refreshToken"],
    }).encode()
    req = urllib.request.Request("https://oauth2.googleapis.com/token", data=form, method="POST")
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            TOKEN["access"] = json.loads(resp.read())["access_token"]
    except urllib.error.HTTPError as e:
        # The body of a refusal names the error and holds no secret.
        sys.exit(f"drive_probe: the sign-in was refused ({e.code}): {e.read()[:200]!r}")


def md5(data):
    return hashlib.md5(data).hexdigest()


def multipart(metadata, content):
    boundary = "vaporprobe" + os.urandom(8).hex()
    body = (f"--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{json.dumps(metadata)}"
            f"\r\n--{boundary}\r\nContent-Type: application/octet-stream\r\n\r\n").encode()
    body += content + f"\r\n--{boundary}--".encode()
    return {"Content-Type": f"multipart/related; boundary={boundary}"}, body


def create(parent, name, content, extra=None):
    meta = {"name": name, "parents": [parent]}
    meta.update(extra or {})
    headers, body = multipart(meta, content)
    status, _, raw = call("POST", f"{UPLOAD}/files?uploadType=multipart&fields={FILE_FIELDS}", headers, body)
    if status >= 300:
        raise RuntimeError(f"create {name} answered {status}: {raw[:300]!r}")
    return json.loads(raw)


def overwrite(file_id, content, metadata=None):
    """The provider's overwrite of a small file: a multipart PATCH whose
    metadata moves only `appProperties`."""
    meta = metadata if metadata is not None else {"appProperties": {"vaporOpId": "probe-" + os.urandom(4).hex()}}
    headers, body = multipart(meta, content)
    status, _, raw = call("PATCH", f"{UPLOAD}/files/{file_id}?uploadType=multipart&fields={FILE_FIELDS}", headers, body)
    if status >= 300:
        raise RuntimeError(f"overwrite answered {status}: {raw[:300]!r}")
    return json.loads(raw)


def resumable(method, url, metadata, content, first_chunk):
    """The provider's resumable upload: `fields` on the request that
    opens the session, then chunks. Returns (statuses, final JSON)."""
    status, headers, raw = call(method, url, {"Content-Type": "application/json", "X-Upload-Content-Length": str(len(content))}, json.dumps(metadata).encode())
    if status >= 300:
        raise RuntimeError(f"the session did not open ({status}): {raw[:300]!r}")
    session = headers.get("Location") or headers.get("location")
    statuses, sent, final = [], 0, {}
    while sent < len(content):
        chunk = content[sent:sent + first_chunk]
        last = sent + len(chunk) - 1
        status, headers, raw = call("PUT", session, {"Content-Range": f"bytes {sent}-{last}/{len(content)}"}, chunk)
        statuses.append((status, headers.get("Range") or headers.get("range")))
        if status == 308:
            sent = last + 1
            continue
        if status in (200, 201):
            final = json.loads(raw) if raw else {}
            break
        raise RuntimeError(f"a chunk answered {status}: {raw[:300]!r}")
    return statuses, final


def revisions(file_id):
    out, token = [], None
    for _ in range(5):
        url = f"{API}/files/{file_id}/revisions?pageSize=1000&fields={REVISION_FIELDS}"
        if token:
            url += "&pageToken=" + urllib.parse.quote(token)
        page = js("GET", url)
        out += page.get("revisions", [])
        token = page.get("nextPageToken")
        if not token:
            break
    return out


def stamp_ms(revision):
    t = revision.get("modifiedTime")
    if not t:
        return None
    return calendar.timegm(time.strptime(t[:19], "%Y-%m-%dT%H:%M:%S")) * 1000 + int((t[20:23] or "0").ljust(3, "0"))


def written_then(revs, base_md5, own_md5, took_ms=2000):
    """`versions_of_the_same_moment` of the provider: the versions that
    another writer put in while an upload of `own_md5` over `base_md5`
    committed, by their place in the list and by their times."""
    sums = [r.get("md5Checksum", "") for r in revs]
    own = max((i for i, s in enumerate(sums) if s == own_md5), default=None)
    if own is None:
        return []
    base = max((i for i, s in enumerate(sums[:own]) if s == base_md5), default=None)
    if base is None:
        return []
    own_at = stamp_ms(revs[own])
    out = []
    for i, r in enumerate(revs):
        s = r.get("md5Checksum", "")
        at = stamp_ms(r)
        if i <= base or i == own or not s or s in (base_md5, own_md5):
            continue
        if own_at is not None and at is not None and not (at + 2 * took_ms + 30000 >= own_at and at <= own_at + took_ms + 5000):
            continue
        out = [x for x in out if x.get("md5Checksum") != s] + [r]
    return out


def kept(revs, base_md5, own_md5, head_md5):
    """What the provider keeps as copies: the versions of that moment
    that do not hold the name."""
    return [r for r in written_then(revs, base_md5, own_md5) if r.get("md5Checksum") != head_md5]


def version_of(file_id):
    return int(js("GET", f"{API}/files/{file_id}?fields=version,headRevisionId").get("version", 0))


def fact(fid, holds, assumed, saw):
    RESULTS.append(holds)
    print(f"{'HOLDS ' if holds else 'BREAKS'} {fid}: {assumed}\n         saw: {saw}", flush=True)


def note(fid, text):
    print(f"NOTE   {fid}: {text}", flush=True)


def f_resumable(root):
    content = os.urandom(6 * 1024 * 1024 + 123)
    statuses, final = resumable("POST", f"{UPLOAD}/files?uploadType=resumable&fields={FILE_FIELDS}", {"name": "big.bin", "parents": [root]}, content, 4 * 1024 * 1024)
    fact("F1", final.get("md5Checksum") == md5(content) and bool(final.get("modifiedTime")) and statuses[0][0] == 308 and statuses[0][1] == f"bytes=0-{4 * 1024 * 1024 - 1}",
         "a resumable upload answers 308 with the stored range for a chunk, and its last answer holds the checksum and the time (`fields` goes on the request that opens the session)",
         f"statuses {statuses}; fields of the last answer {sorted(final)}; checksum equal: {final.get('md5Checksum') == md5(content)}")
    second = os.urandom(6 * 1024 * 1024 + 7)
    statuses, patched = resumable("PATCH", f"{UPLOAD}/files/{final['id']}?uploadType=resumable&fields={FILE_FIELDS}", {"appProperties": {"vaporOpId": "probe-resumable"}}, second, 8 * 1024 * 1024)
    fact("F2", patched.get("md5Checksum") == md5(second) and patched.get("id") == final["id"],
         "a resumable overwrite keeps the file id and answers with the checksum of the new bytes",
         f"statuses {statuses}; same id: {patched.get('id') == final['id']}; checksum equal: {patched.get('md5Checksum') == md5(second)}")
    return final["id"], content, second


def f_revisions(root):
    base, x, y = b"base\n", b"edited on device A\n", b"edited on device B, longer\n"
    doc = create(root, "doc.txt", base)
    v0 = version_of(doc["id"])
    overwrite(doc["id"], x)
    v1 = version_of(doc["id"])
    overwrite(doc["id"], y)
    v2 = version_of(doc["id"])
    revs = revisions(doc["id"])
    sums = [r.get("md5Checksum") for r in revs]
    fact("F3", sums == [md5(base), md5(x), md5(y)],
         "two overwrites of one file by one account, one right after the other, are two stored versions, and `revisions.list` gives the versions oldest first",
         f"{len(revs)} versions, in the order base/x/y: {sums == [md5(base), md5(x), md5(y)]}; times {[r.get('modifiedTime') for r in revs]}")
    fact("F4", all(r.get("id") and r.get("md5Checksum") and r.get("size") and r.get("modifiedTime") for r in revs),
         "each version has an id, a checksum, a size, and a time",
         f"fields {[sorted(r) for r in revs]}")
    unseen = kept(revs, md5(base), md5(y), md5(y))
    fact("F5", [r.get("md5Checksum") for r in unseen] == [md5(x)],
         "the rule of the provider names the version that the last upload replaced",
         f"named {len(unseen)} version(s); the one of x: {[r.get('md5Checksum') for r in unseen] == [md5(x)]}")
    if unseen:
        status, _, raw = call("GET", f"{API}/files/{doc['id']}/revisions/{unseen[0]['id']}?alt=media")
        fact("F6", status == 200 and raw == x,
             "`revisions.get` with `alt=media` answers the bytes of a version that is no longer the head",
             f"status {status}, bytes equal: {raw == x}")
    note("version", f"`files.version` went {v0} -> {v1} -> {v2} over two overwrites of content")

    # One version written with an old time, as Drive for desktop does.
    # The wait makes the versions above the history of the file: the
    # rule takes a version for a write of the moment of an upload only
    # inside half a minute before it.
    time.sleep(40)
    z, w = b"written with the time of a file on disk\n", b"the last write\n"
    overwrite(doc["id"], z, {"modifiedTime": "2001-01-01T00:00:00.000Z"})
    overwrite(doc["id"], w)
    revs = revisions(doc["id"])
    sums = [r.get("md5Checksum") for r in revs]
    times = [r.get("modifiedTime") for r in revs]
    by_time = times == sorted(times) and sums[0] == md5(z)
    fact("F7", by_time,
         "`revisions.list` gives the versions by their times, and a writer can set the time of its version: one written last with an old time is listed first",
         f"sorted by time: {times == sorted(times)}; the version written with the time 2001-01-01 is listed at place {sums.index(md5(z)) + 1 if md5(z) in sums else '?'} of {len(sums)}")
    history = kept(revs, md5(z), md5(w), md5(w))
    fact("F7b", history == [],
         "the rule of the provider takes none of the history of a file for a replaced version when the base carries an old time and is listed before that history",
         f"an upload over the version dated 2001 would keep {len(history)} version(s) as copies")
    return doc["id"]


def f_race(root):
    """Two writers that both read the base and then write, inside one
    round trip, as two profiles of one daemon do."""
    base = b"base of the race\n"
    doc = create(root, "race.txt", base)
    edits = [b"edit of writer one\n", b"edit of writer two, longer\n"]
    answers, gate = [None, None], threading.Barrier(2)

    def writer(i):
        current = js("GET", f"{API}/files/{doc['id']}?fields={FILE_FIELDS}")
        gate.wait()
        if current.get("md5Checksum") == md5(base):
            answers[i] = overwrite(doc["id"], edits[i]).get("md5Checksum")

    threads = [threading.Thread(target=writer, args=(i,)) for i in range(2)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    revs = revisions(doc["id"])
    sums = [r.get("md5Checksum") for r in revs]
    head = js("GET", f"{API}/files/{doc['id']}?fields={FILE_FIELDS}").get("md5Checksum")
    both_landed = answers == [md5(edits[0]), md5(edits[1])]
    fact("F8", both_landed and sorted(sums[1:]) == sorted(md5(e) for e in edits) and sums[0] == md5(base),
         "two guarded overwrites that commit at the same moment both land (Drive has no conditional write of content), and both are stored versions",
         f"both answered 200: {both_landed}; {len(revs)} versions; the head is the edit of writer {1 + [md5(e) for e in edits].index(head) if head in [md5(e) for e in edits] else '?'}")
    if both_landed and len(sums) == 3:
        loser = [md5(e) for e in edits if md5(e) != head][0]
        winner_view = kept(revs, md5(base), head, head)
        loser_view = kept(revs, md5(base), loser, head)
        fact("F9", [r.get("md5Checksum") for r in winner_view] == [loser] and loser_view == [],
             "the writer whose upload holds the name keeps the other version, and the other writer keeps none, wherever Drive lists the head",
             f"the head's writer keeps {len(winner_view)}, the replaced writer keeps {len(loser_view)}; the head is listed at place {sums.index(head) + 1} of 3")


def f_race_detail(root, rounds):
    """The race, several times, with what Drive says about each one: the
    order of the versions, their times, and which one is the head."""
    head_last = 0
    for n in range(rounds):
        base = f"base {n}\n".encode()
        doc = create(root, f"race-{n}.txt", base)
        edits = [f"edit of writer one, round {n}\n".encode(), f"edit of writer two, longer, round {n}\n".encode()]
        done, gate = [None, None], threading.Barrier(2)

        def writer(i):
            gate.wait()
            headers, body = multipart({"appProperties": {"vaporOpId": f"probe-{n}-{i}"}}, edits[i])
            status, _, raw = call("PATCH", f"{UPLOAD}/files/{doc['id']}?uploadType=multipart&fields={FILE_FIELDS},headRevisionId", headers, body)
            done[i] = (time.time(), status, json.loads(raw) if status < 300 else {})

        threads = [threading.Thread(target=writer, args=(i,)) for i in range(2)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        time.sleep(1.0)
        url = f"{API}/files/{doc['id']}/revisions?pageSize=1000&fields=revisions(id,md5Checksum,modifiedTime,size)"
        revs = js("GET", url).get("revisions", [])
        now = js("GET", f"{API}/files/{doc['id']}?fields=md5Checksum,headRevisionId,modifiedTime,version")
        who = {md5(base): "base", md5(edits[0]): "one", md5(edits[1]): "two"}
        order = [who.get(r.get("md5Checksum"), "?") for r in revs]
        head_at = [r["id"] for r in revs].index(now["headRevisionId"]) if now.get("headRevisionId") in [r["id"] for r in revs] else None
        answered_own = [done[i][2].get("headRevisionId") in [r["id"] for r in revs if r.get("md5Checksum") == md5(edits[i])] for i in range(2)]
        first_back = "one" if done[0][0] < done[1][0] else "two"
        head_last += head_at == len(revs) - 1
        print(json.dumps({
            "round": n, "statuses": [done[0][1], done[1][1]], "listed": order, "times": [r.get("modifiedTime", "")[11:] for r in revs],
            "head_is": who.get(now.get("md5Checksum"), "?"), "head_listed_at": head_at, "of": len(revs),
            "answer_names_own_version": answered_own, "answered_first": first_back, "file_time": now.get("modifiedTime", "")[11:], "version": now.get("version"),
        }), flush=True)
    note("race", f"the head was the last version listed in {head_last} of {rounds} rounds")


def f_stamp(root):
    """When Drive gives a version its time: a resumable overwrite whose
    session opens 20 s before its bytes go."""
    doc = create(root, "stamp.bin", b"base\n")
    content = os.urandom(6 * 1024 * 1024)
    status, headers, raw = call("PATCH", f"{UPLOAD}/files/{doc['id']}?uploadType=resumable&fields={FILE_FIELDS}",
                                {"Content-Type": "application/json", "X-Upload-Content-Length": str(len(content))}, b"{}")
    opened = headers.get("Date") or headers.get("date")
    session = headers.get("Location") or headers.get("location")
    time.sleep(20)
    status, headers, raw = call("PUT", session, {"Content-Range": f"bytes 0-{len(content) - 1}/{len(content)}"}, content)
    sent = headers.get("Date") or headers.get("date")
    final = json.loads(raw) if status < 300 else {}
    note("stamp", f"the session opened at {opened}, the bytes were answered at {sent}, and the version carries {final.get('modifiedTime')}")


def f_stamp_slow(root):
    """Whether a version gets its time when the request starts or when
    its bytes are in: one overwrite whose body takes 20 s to send."""
    import http.client
    doc = create(root, "slow.bin", b"base\n")
    headers, body = multipart({"appProperties": {"vaporOpId": "probe-slow"}}, os.urandom(512 * 1024))
    _, first, _ = call("GET", f"{API}/files/{doc['id']}?fields=id")
    started = first.get("Date") or first.get("date")
    conn = http.client.HTTPSConnection("www.googleapis.com", timeout=120)
    conn.putrequest("PATCH", f"/upload/drive/v3/files/{doc['id']}?uploadType=multipart&fields={FILE_FIELDS}")
    conn.putheader("Authorization", "Bearer " + TOKEN["access"])
    conn.putheader("Content-Type", headers["Content-Type"])
    conn.putheader("Content-Length", str(len(body)))
    conn.endheaders()
    piece = len(body) // 10
    for i in range(10):
        conn.send(body[i * piece:(i + 1) * piece if i < 9 else len(body)])
        if i < 9:
            time.sleep(2.2)
    resp = conn.getresponse()
    raw = resp.read()
    final = json.loads(raw) if resp.status < 300 else {}
    started_at = calendar.timegm(time.strptime(started, "%a, %d %b %Y %H:%M:%S GMT")) * 1000
    answered_at = calendar.timegm(time.strptime(resp.getheader("Date"), "%a, %d %b %Y %H:%M:%S GMT")) * 1000
    at = stamp_ms(final) or 0
    fact("F13", resp.status == 200 and at - started_at >= 15000 and answered_at + 1000 - at <= 5000,
         "a version gets its time when its bytes are in, not when its request starts, so two writes of one moment carry times that close",
         f"the request started at {started}, its last bytes went 20 s later, the answer came at {resp.getheader('Date')}, and the version carries {final.get('modifiedTime')}")
    conn.close()


def f_folder_entries(root):
    """Which entries the changes feed has for a folder: when a file is
    put in it, and when it gets another name."""
    folder = js("POST", f"{API}/files?fields=id,name", {"name": "Inbox", "mimeType": FOLDER, "parents": [root]})
    create(folder["id"], "first.txt", b"first\n")
    time.sleep(3)

    def entries_after(action):
        token = js("GET", f"{API}/changes/startPageToken").get("startPageToken")
        action()
        time.sleep(4)
        page = js("GET", f"{API}/changes?pageToken={token}&fields=changes(fileId,file(name,mimeType))&pageSize=100")
        return [("folder " if c.get("file", {}).get("mimeType") == FOLDER else "file ") + c.get("file", {}).get("name", "?") for c in page.get("changes", [])]

    added = entries_after(lambda: create(folder["id"], "second.txt", b"second\n"))
    renamed = entries_after(lambda: js("PATCH", f"{API}/files/{folder['id']}?fields=id", {"name": "inbox"}))
    note("folder-entries", f"a file put in a folder gives the entries {added}; a folder that gets another name gives {renamed}")


def f_if_match(root):
    """Whether the older API version refuses a write of content whose
    condition no longer holds. The provider uses v3, which has none."""
    doc = create(root, "guarded.txt", b"base\n")
    v2 = "https://www.googleapis.com/drive/v2/files/" + doc["id"]
    etag = js("GET", v2 + "?fields=etag,md5Checksum").get("etag")
    overwrite(doc["id"], b"another device wrote\n")
    up = "https://www.googleapis.com/upload/drive/v2/files/" + doc["id"] + "?uploadType=media"
    stale, _, raw = call("PUT", up, {"If-Match": etag or "", "Content-Type": "application/octet-stream"}, b"a write with a stale condition\n")
    fresh_tag = js("GET", v2 + "?fields=etag").get("etag")
    fresh, _, _ = call("PUT", up, {"If-Match": fresh_tag or "", "Content-Type": "application/octet-stream"}, b"a write with a current condition\n")
    head = js("GET", f"{API}/files/{doc['id']}?fields=md5Checksum").get("md5Checksum")
    note("if-match", f"v2 has an etag: {bool(etag)}; an upload with a stale `If-Match` answered {stale}; with a current one {fresh}; "
                     f"the file holds the {'stale write' if head == md5(b'a write with a stale condition' + bytes([10])) else 'current write' if head == md5(b'a write with a current condition' + bytes([10])) else 'other content'}")


def f_if_match_race(root, rounds):
    """Two writers with one condition, at the same moment, through the
    older API version: whether exactly one of them lands."""
    outcomes = []
    for n in range(rounds):
        doc = create(root, f"guarded-race-{n}.txt", f"base {n}\n".encode())
        v2 = "https://www.googleapis.com/drive/v2/files/" + doc["id"]
        etag = js("GET", v2 + "?fields=etag").get("etag") or ""
        up = "https://www.googleapis.com/upload/drive/v2/files/" + doc["id"] + "?uploadType=media"
        edits = [f"edit of writer one, round {n}\n".encode(), f"edit of writer two, longer, round {n}\n".encode()]
        statuses, gate = [None, None], threading.Barrier(2)

        def writer(i):
            gate.wait()
            statuses[i], _, _ = call("PUT", up, {"If-Match": etag, "Content-Type": "application/octet-stream"}, edits[i])

        threads = [threading.Thread(target=writer, args=(i,)) for i in range(2)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        time.sleep(1.0)
        listed = len(revisions(doc["id"]))
        outcomes.append((sorted(statuses), listed))
    one_landed = sum(1 for st, _ in outcomes if st == [200, 412])
    note("if-match-race", f"two uploads with one `If-Match` at the same moment, {rounds} rounds: exactly one landed in {one_landed}; "
                          f"statuses and versions of each round: {outcomes}")


def f_ranges(big_id, first, second):
    revs = revisions(big_id)
    old = [r for r in revs if r.get("md5Checksum") == md5(first)]
    if not old:
        fact("F10", False, "a version of more than 5 MiB that an overwrite replaced is listed", f"{len(revs)} versions, none with the first checksum")
        return
    chunk = 256 * 1024
    status, headers, raw = call("GET", f"{API}/files/{big_id}/revisions/{old[0]['id']}?alt=media", {"Range": f"bytes={chunk}-{2 * chunk - 1}"})
    fact("F10", status == 206 and raw == first[chunk:2 * chunk],
         "a range of a stored version answers 206 with exactly those bytes (the copy of a large version moves one range for each poll)",
         f"status {status}, {len(raw)} bytes, equal: {raw == first[chunk:2 * chunk]}, Content-Range {headers.get('Content-Range') or headers.get('content-range')}")


def f_folder_case(root):
    folder = js("POST", f"{API}/files?fields=id,name", {"name": "Reports", "mimeType": FOLDER, "parents": [root]})
    kids = [create(folder["id"], f"f{i}.txt", f"file {i}\n".encode()) for i in range(3)]
    renamed = js("PATCH", f"{API}/files/{folder['id']}?fields=id,name", {"name": "reports"})
    q = urllib.parse.quote(f"'{folder['id']}' in parents and trashed = false")
    listed = js("GET", f"{API}/files?q={q}&fields=files({FILE_FIELDS})&pageSize=100").get("files", [])
    fact("F11", renamed.get("id") == folder["id"] and renamed.get("name") == "reports" and sorted(f["id"] for f in listed) == sorted(k["id"] for k in kids),
         "a folder rename that changes only the capitals keeps the folder id, and its children stay under it",
         f"name now {renamed.get('name')!r}; same folder id: {renamed.get('id') == folder['id']}; children listed: {len(listed)} of {len(kids)}, same ids: {sorted(f['id'] for f in listed) == sorted(k['id'] for k in kids)}")
    for spelling in ("reports", "Reports", "REPORTS"):
        q = urllib.parse.quote(f"'{root}' in parents and name = '{spelling}' and trashed = false")
        found = js("GET", f"{API}/files?q={q}&fields=files(id,name)&pageSize=100").get("files", [])
        note("name-query", f"`name = '{spelling}'` finds {[f['name'] for f in found]}")


def f_copy(root):
    data = b"the bytes of a replaced version\n"
    headers, body = multipart({"name": "doc~conflict-replaced-1795428005100.txt", "parents": [root]}, data)
    status, _, raw = call("POST", f"{UPLOAD}/files?uploadType=multipart&fields=id", headers, body)
    made = json.loads(raw) if status < 300 else {}
    back = js("GET", f"{API}/files/{made['id']}?fields={FILE_FIELDS}") if made.get("id") else {}
    fact("F12", status < 300 and back.get("md5Checksum") == md5(data) and back.get("parents") == [root],
         "the copy of a version is a new file under the name of a conflict copy, in the folder of its file",
         f"status {status}; name {back.get('name')!r}; checksum equal: {back.get('md5Checksum') == md5(data)}")


def find(path):
    """The file or folder at a path from the top of the Drive."""
    if not [part for part in path.split("/") if part]:
        return {"id": "root", "name": "My Drive"}
    current = {"id": "root", "name": ""}
    for segment in [part for part in path.split("/") if part]:
        escaped = segment.replace("\\", "\\\\").replace("'", "\\'")
        q = urllib.parse.quote(f"'{current['id']}' in parents and name = '{escaped}' and trashed = false")
        found = [f for f in js("GET", f"{API}/files?q={q}&fields=files(id,name,mimeType,md5Checksum)&pageSize=100").get("files", []) if f["name"] == segment]
        if not found:
            sys.exit(f"drive_probe: nothing is at {segment!r} of {path!r}")
        current = min(found, key=lambda f: f["id"])
    return current


def cloud(args):
    """A change made in the cloud by something that is not Vapor."""
    action, path = args[0], args[1]
    target = find(path)
    if action == "touch":
        now = time.strftime("%Y-%m-%dT%H:%M:%S.000Z", time.gmtime())
        done = js("PATCH", f"{API}/files/{target['id']}?fields=id,name,modifiedTime,md5Checksum", {"modifiedTime": now})
        print(f"touched {done['name']}: the time is {done['modifiedTime']}, the checksum is unchanged: {done.get('md5Checksum') == target.get('md5Checksum')}")
    elif action == "rename":
        done = js("PATCH", f"{API}/files/{target['id']}?fields=id,name", {"name": args[2]})
        print(f"renamed {target['name']} to {done['name']}")
    elif action == "move":
        into = find(args[2])
        parents = js("GET", f"{API}/files/{target['id']}?fields=parents").get("parents", [])
        js("PATCH", f"{API}/files/{target['id']}?addParents={into['id']}&removeParents={','.join(parents)}&fields=id,parents", {})
        print(f"moved {target['name']} into {into['name']}")
    elif action == "overwrite":
        metadata = {"modifiedTime": args[3]} if len(args) > 3 else {}
        done = overwrite(target["id"], args[2].encode() + bytes([10]), metadata)
        print(f"overwrote {done['name']}: the version carries the time {done['modifiedTime']}")
    elif action == "overwrite-after":
        # A program that writes without a look at what it replaces, at
        # the moment another upload of the file lands.
        before = target.get("md5Checksum")
        deadline = time.time() + float(args[3]) if len(args) > 3 else time.time() + 120
        while time.time() < deadline:
            now = js("GET", f"{API}/files/{target['id']}?fields=md5Checksum").get("md5Checksum")
            if now != before:
                done = overwrite(target["id"], args[2].encode() + bytes([10]), {})
                print(f"the file changed, and {done['name']} was overwritten at once: the version carries the time {done['modifiedTime']}")
                return
            time.sleep(0.1)
        sys.exit("drive_probe: the file did not change in time")
    elif action == "versions":
        head = js("GET", f"{API}/files/{target['id']}?fields=md5Checksum,headRevisionId")
        for r in revisions(target["id"]):
            print(json.dumps({"time": r.get("modifiedTime"), "size": r.get("size"), "head": r.get("md5Checksum") == head.get("md5Checksum")}))
    elif action == "trash":
        done = js("PATCH", f"{API}/files/{target['id']}?fields=id,name,trashed", {"trashed": True})
        print(f"{done['name']} is in the trash of the account")
    elif action == "list":
        q = urllib.parse.quote(f"'{target['id']}' in parents and trashed = false")
        for f in sorted(js("GET", f"{API}/files?q={q}&fields=files(name,mimeType,size)&pageSize=1000").get("files", []), key=lambda f: f["name"]):
            print(("folder " if f.get("mimeType") == FOLDER else "file   ") + f["name"])
    elif action == "heads":
        # Every file under the folders of `drive_race.py`: where Drive
        # lists the version that holds the name.
        q = urllib.parse.quote(f"'{target['id']}' in parents and name contains 't-race-' and trashed = false")
        folders = js("GET", f"{API}/files?q={q}&fields=files(id,name)&pageSize=1000").get("files", [])
        both, head_first = 0, []
        for folder in sorted(folders, key=lambda f: f["name"]):
            q = urllib.parse.quote(f"'{folder['id']}' in parents and trashed = false")
            for doc in js("GET", f"{API}/files?q={q}&fields=files(id,name,md5Checksum)&pageSize=1000").get("files", []):
                if "~conflict-" in doc["name"]:
                    continue
                sums = [r.get("md5Checksum") for r in revisions(doc["id"])]
                if len(sums) < 3:
                    continue
                both += 1
                if sums[-1] != doc.get("md5Checksum"):
                    head_first.append(f"{folder['name']}/{doc['name']}")
        print(f"files where both uploads landed: {both}; the version that holds the name is not the last one listed in {len(head_first)}: {head_first}")
    else:
        sys.exit(f"drive_probe: no cloud action {action!r}")


def main():
    sign_in = json.loads(sys.stdin.read())
    refresh(sign_in)
    if sys.argv[1:2] == ["cloud"]:
        cloud(sys.argv[2:])
        return
    about = js("GET", f"{API}/about?fields=storageQuota(limit,usage)")
    quota = about.get("storageQuota", {})
    print(f"quota: {int(quota.get('usage', 0)) >> 20} MiB used of {int(quota.get('limit', 0)) >> 20} MiB", flush=True)
    root = js("POST", f"{API}/files?fields=id,name", {"name": f"VaporProbe-{int(time.time())}", "mimeType": FOLDER, "parents": ["root"]})
    print(f"probe folder: {root['name']}", flush=True)
    try:
        if sys.argv[1:2] == ["extra"]:
            f_stamp(root["id"])
            f_folder_entries(root["id"])
            f_if_match(root["id"])
            f_if_match_race(root["id"], 10)
            return
        if sys.argv[1:2] == ["race"]:
            f_race_detail(root["id"], int(sys.argv[2]) if len(sys.argv) > 2 else 8)
            return
        big_id, first, second = f_resumable(root["id"])
        f_revisions(root["id"])
        f_race(root["id"])
        f_ranges(big_id, first, second)
        f_folder_case(root["id"])
        f_copy(root["id"])
        f_stamp_slow(root["id"])
    finally:
        js("PATCH", f"{API}/files/{root['id']}?fields=id,trashed", {"trashed": True})
        print("the probe folder is in the trash of the account", flush=True)
    print(f"{sum(RESULTS)} of {len(RESULTS)} facts hold", flush=True)
    sys.exit(0 if all(RESULTS) else 1)


if __name__ == "__main__":
    main()
