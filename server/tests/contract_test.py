#!/usr/bin/env python3
"""Contract tests for MediaDir v3 (docs/API.md + docs/STORAGE.md). Stdlib only (+ ffmpeg).

usage: contract_test.py http://127.0.0.1:8098

Needs a FRESH server (empty DATA_DIR / MEDIA_DIR / INGEST_DIR) on this machine, ideally with
short intervals so the periodic tasks run during the test:
  INGEST_SETTLE_SECS=1 INGEST_POLL_SECS=2 SIDECAR_INTERVAL=2 VERIFY_INTERVAL=3 BACKUP_INTERVAL=3

The test builds unique copies of testmedia/ (remuxed with -c copy, so every file has its own
hash), copies them into the server's INGEST_DIR, uploads more through the API and checks the
store, the items and the rest of the API the Android app uses.
"""
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

BASE = (sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8098").rstrip("/")
TESTMEDIA = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", "testmedia"))
MEDIA_EXTS = {"mkv", "mp4", "m4v", "webm", "mov", "avi", "ts", "mp3", "flac", "m4a", "ogg", "opus", "wav"}
FAILS = []
PASSES = 0


def req(method, path, body=None, headers=None, raw=False, expect=None):
    data = None
    h = dict(headers or {})
    if body is not None and not isinstance(body, (bytes, bytearray)):
        data = json.dumps(body).encode()
        h.setdefault("Content-Type", "application/json")
    elif body is not None:
        data = bytes(body)
    r = urllib.request.Request(BASE + path, data=data, method=method, headers=h)
    try:
        with urllib.request.urlopen(r, timeout=120) as resp:
            status, hdrs, payload = resp.status, resp.headers, resp.read()
    except urllib.error.HTTPError as e:
        status, hdrs, payload = e.code, e.headers, e.read()
    if expect is not None and status != expect:
        raise AssertionError(f"{method} {path}: expected {expect}, got {status}: {payload[:300]!r}")
    if raw:
        return status, hdrs, payload
    try:
        return status, json.loads(payload) if payload else None
    except ValueError:
        raise AssertionError(f"{method} {path}: non-JSON body {payload[:200]!r}")


def get(path, expect=200):
    return req("GET", path, expect=expect)[1]


def check(name, fn):
    global PASSES
    try:
        fn()
        PASSES += 1
        print(f"  ok   {name}")
    except Exception as e:  # noqa
        FAILS.append((name, e))
        import traceback
        tb = traceback.extract_tb(e.__traceback__)[-1]
        print(f"  FAIL {name}: line {tb.lineno}: {tb.line}: {e}")


def has(obj, *keys):
    for k in keys:
        assert k in obj, f"missing key {k!r} in {sorted(obj)}"


def is_id(s):
    return isinstance(s, str) and len(s) == 16 and set(s) <= set("0123456789abcdef")


ITEM_KINDS = {"movie", "show", "season", "episode", "artist", "album", "track", "music_video", "video"}
PLAYABLE = {"movie", "episode", "track", "music_video", "video"}
ASPECT = {"movie": "poster", "show": "poster", "season": "poster", "episode": "landscape", "music_video": "landscape",
          "video": "landscape", "artist": "square", "album": "square", "track": "square"}
PLACEMENTS = {"card_badge", "card_meta", "header", "info", "section", "hidden"}
WIDGETS = {"badge", "chip", "text", "rating", "link", "tags", "people", "markdown", "stat"}
TYPES = {"text", "number", "date", "url", "tags", "rating", "bool", "duration", "person"}
STATES = {"waiting", "hashing", "placing", "probing", "done", "duplicate", "failed", "ignored"}


def check_block(b):
    has(b, "key", "label", "type", "widget", "placement", "order", "color", "icon", "text", "value")
    assert b["type"] in TYPES and b["widget"] in WIDGETS, b
    assert b["placement"] in PLACEMENTS and b["placement"] != "hidden", b
    assert isinstance(b["text"], str) and b["text"], b
    assert b["value"] not in (None, "", []), b
    if b["type"] == "person" or b["widget"] == "people":
        assert isinstance(b.get("people"), list) and b["people"], b


def check_summary(s):
    has(s, "id", "kind", "library_id", "parent_id", "title", "subtitle", "year", "index", "parent_index", "index_end",
        "duration", "playable", "child_count", "aspect", "images", "added_at", "card")
    assert is_id(s["id"]) and is_id(s["library_id"]), (s["id"], s["library_id"])
    assert s["kind"] in ITEM_KINDS
    assert s["playable"] == (s["kind"] in PLAYABLE)
    assert s["aspect"] == ASPECT[s["kind"]], (s["kind"], s["aspect"])
    has(s["images"], "thumb", "poster", "backdrop", "banner", "clearlogo", "landscape", "discart")
    for v in s["images"].values():
        assert v is None or v.startswith("/image/item/"), v
    if s["playable"]:
        assert s["child_count"] is None
    else:
        assert isinstance(s["child_count"], int)
    orders = [b["order"] for b in s["card"]]
    assert orders == sorted(orders), orders
    for b in s["card"]:
        check_block(b)
        assert b["placement"] in ("card_badge", "card_meta")


def check_file(f):
    has(f, "id", "path", "label", "original", "container", "mime", "size", "duration", "bitrate", "video",
        "audio", "subtitles", "stream_url", "part", "edition", "hash", "original_name")
    assert f["stream_url"] == "/stream/" + f["id"]
    assert len(f["hash"]) == 64 and f["path"] == f["original_name"]
    if f["video"] is not None:
        has(f["video"], "codec", "profile", "width", "height", "fps", "hdr", "bit_depth")
    for a in f["audio"]:
        has(a, "index", "codec", "channels", "language", "title")
    for s in f["subtitles"]:
        has(s, "index", "codec", "language", "title", "forced", "external")


def check_detail(d):
    check_summary(d)
    has(d, "overview", "sort_title", "genres", "locked", "breadcrumbs", "files", "credits", "children", "extras",
        "fields", "display")
    for f in d["files"]:
        check_file(f)
    hs = [(f["video"] or {}).get("height") or 0 for f in d["files"]]
    assert hs == sorted(hs, reverse=True), hs
    for c in d["credits"]:
        has(c, "id", "role", "character", "order", "person")
        has(c["person"], "id", "name", "image")
    for c in d["children"]:
        check_summary(c)
    for b in d["display"]:
        check_block(b)
    for bc in d["breadcrumbs"]:
        has(bc, "id", "kind", "title")


def items(**q):
    return get("/api/items?" + urllib.parse.urlencode(q))


def find(lst, **kv):
    for x in lst:
        if all(x.get(k) == v for k, v in kv.items()):
            return x
    raise AssertionError(f"no item matching {kv} in {[x.get('title') for x in lst]}")


def status():
    return get("/api/ingest/status")


def wait_idle(timeout=180, min_records=0):
    """Until nothing is settling or queued and the records are stable."""
    t0 = time.time()
    last, stable = None, 0
    while time.time() - t0 < timeout:
        s = status()
        n = len(get("/api/ingest?limit=1000"))
        if s["queue"] == 0 and s["settling"] == 0 and s["active"] == 0 and n >= min_records:
            stable = stable + 1 if n == last else 0
            if stable >= 3:
                return
        last = n
        time.sleep(0.5)
    raise AssertionError("ingest did not become idle")


def wait_for(fn, timeout=30, what="condition"):
    t0 = time.time()
    while time.time() - t0 < timeout:
        v = fn()
        if v:
            return v
        time.sleep(0.5)
    raise AssertionError(f"timed out waiting for {what}")


def ffmpeg(*args):
    subprocess.run(["ffmpeg", "-v", "error", "-y", *args], check=True, stdin=subprocess.DEVNULL)


def make_video(path, w, h, freq):
    ffmpeg("-f", "lavfi", "-i", f"testsrc=size={w}x{h}:rate=25:duration=2", "-f", "lavfi",
           "-i", f"sine=frequency={freq}:duration=2", "-c:v", "libx264", "-preset", "veryfast",
           "-pix_fmt", "yuv420p", "-c:a", "aac", path)


def build_fixtures(dst):
    """Unique copies of every testmedia file (remux, no re-encode); other files copied as-is."""
    tag = uuid.uuid4().hex
    media = []
    for root, dirs, files in os.walk(TESTMEDIA):
        for name in files:
            src = os.path.join(root, name)
            rel = os.path.relpath(src, TESTMEDIA)
            out = os.path.join(dst, rel)
            os.makedirs(os.path.dirname(out), exist_ok=True)
            ext = name.rsplit(".", 1)[-1].lower() if "." in name else ""
            hidden = any(p.startswith(".") for p in rel.split(os.sep))
            if ext in MEDIA_EXTS and not hidden:
                ffmpeg("-i", src, "-map", "0", "-c", "copy", "-map_metadata", "-1", "-metadata", f"comment={tag}:{rel}", out)
                media.append(rel)
            else:
                shutil.copy2(src, out)
    return media


def blob_path(media_dir, f):
    h = f["hash"]
    return os.path.join(media_dir, h[:2], h[2:4], f"{h}.{f['container']}")


def main():
    print(f"contract test against {BASE}")
    state = {}

    def t_health():
        h = get("/api/health")
        assert h["ok"] is True and h["version"] == 3, h
    check("health", t_health)

    def t_fresh_and_status():
        s = status()
        has(s, "watching", "ingest_dir", "media_dir", "queue", "last_sidecar_sync", "last_verify", "last_backup", "problems")
        assert os.path.isdir(s["ingest_dir"]) and os.path.isdir(s["media_dir"]), s
        assert stat.S_IMODE(os.stat(s["media_dir"]).st_mode) == 0o700
        assert items(top="false")["total"] == 0, "needs a fresh server"
        assert get("/api/libraries") == []
        state.update(ingest_dir=s["ingest_dir"], media_dir=s["media_dir"], data_dir=s.get("data_dir"))
    check("fresh server + status shape", t_fresh_and_status)
    if FAILS:
        return

    work = tempfile.mkdtemp(prefix="mediadir-contract-", dir=os.path.dirname(state["ingest_dir"]))
    fixtures = os.path.join(work, "fixtures")
    media = build_fixtures(fixtures)
    state["media"] = media
    print(f"  ..   built {len(media)} unique media fixtures; copying into the ingest dir")
    shutil.copytree(fixtures, state["ingest_dir"], dirs_exist_ok=True)
    wait_idle(min_records=len(media))

    def t_ingest_records():
        recs = get("/api/ingest?limit=1000")
        for r in recs:
            has(r, "id", "source", "size", "hash", "state", "error", "item_id", "ingested_at", "origin", "name", "placement")
            assert r["state"] in STATES
        by_src = {os.path.relpath(r["source"], state["ingest_dir"]): r for r in recs}
        assert set(by_src) == set(media), (set(by_src) ^ set(media))
        for rel, r in by_src.items():
            assert r["state"] == "done", (rel, r["state"], r["error"])
            assert r["placement"] in ("hardlink", "reflink"), r["placement"]
            assert is_id(r["item_id"]) and len(r["hash"]) == 64
        # newest first, state filter
        ts = [r["ingested_at"] for r in recs]
        assert ts == sorted(ts, reverse=True)
        assert get("/api/ingest?state=failed") == []
        assert req("GET", "/api/ingest?state=bogus")[0] == 400
        state["records"] = by_src
    check("ingest dir: every media file done, non-media / partial / hidden ignored", t_ingest_records)
    if FAILS:
        return

    def t_store():
        for rel, r in state["records"].items():
            orig = os.path.join(state["ingest_dir"], rel)
            f = get(f"/api/items/{r['item_id']}")["files"]
            f = find(f, id=r["file_id"])
            blob = blob_path(state["media_dir"], f)
            so, sb = os.stat(orig), os.stat(blob)
            if r["placement"] == "hardlink":
                assert so.st_ino == sb.st_ino and sb.st_nlink == 2, rel
            assert stat.S_IMODE(sb.st_mode) == 0o444, oct(sb.st_mode)
            assert stat.S_IMODE(os.stat(os.path.dirname(blob)).st_mode) == 0o700
            assert sb.st_size == f["size"] == r["size"]
        assert os.listdir(os.path.join(state["media_dir"], ".staging")) == []
    check("store: content-addressed, hardlinked, 0444 files, 0700 dirs", t_store)

    def t_libraries():
        libs = get("/api/libraries")
        for l in libs:
            has(l, "id", "name", "kind", "path", "item_count", "scanning")
            assert is_id(l["id"]) and l["path"] is None and l["scanning"] is False
        kinds = {l["kind"]: l for l in libs}
        assert set(kinds) == {"movies", "shows", "music", "music_videos", "videos"}, kinds
        expect = {"movies": 7, "shows": 3, "music": 8, "music_videos": 5, "videos": 8}
        for k, n in expect.items():
            lib = kinds[k]
            r = items(library_id=lib["id"], limit=200)
            assert lib["item_count"] == r["total"] == n, (k, lib["item_count"], r["total"], [i["title"] for i in r["items"]])
            assert all(i["library_id"] == lib["id"] for i in r["items"])
            assert get(f"/api/libraries/{lib['id']}")["kind"] == k
        state.update({k: v for k, v in kinds.items()})
        for m in ("POST", "PATCH", "DELETE"):
            s, b = req(m, "/api/libraries", {"name": "x", "kind": "movies", "path": "/tmp"})
            assert s == 405 and "error" in b, (m, s)
            s, _ = req(m, f"/api/libraries/{kinds['movies']['id']}", {"name": "x"})
            assert s == 405, (m, s)
        assert req("POST", "/api/scan")[0] in (404, 405)
        assert req("POST", f"/api/libraries/{kinds['movies']['id']}/scan")[0] in (404, 405)
        assert req("GET", "/api/libraries/0000000000000000")[0] == 404
    check("virtual libraries (path null, stable ids, CRUD/scan gone)", t_libraries)
    if FAILS:
        return

    def t_movies():
        r = items(library_id=state["movies"]["id"])
        for s in r["items"]:
            check_summary(s)
            assert s["kind"] == "movie" and s["parent_id"] is None and s["year"]
        titles = sorted(i["title"] for i in r["items"])
        assert titles == ["Big Buck Test", "Elephants Dream", "Ignored Movie", "Sintel Test", "Stacked Test", "Versions Test", "cake"], titles
        vt = get(f"/api/items/{find(r['items'], title='Versions Test')['id']}")
        check_detail(vt)
        assert vt["year"] == 2015 and len(vt["files"]) == 3, [f["path"] for f in vt["files"]]
        assert all(f["edition"] is None and f["part"] is None and f["original"] for f in vt["files"])
        assert all(" · " in f["label"] for f in vt["files"])
        bb = get(f"/api/items/{find(r['items'], title='Big Buck Test')['id']}")
        assert sorted(f["path"] for f in bb["files"]) == ["Big Buck Test (2008) - 270p.mkv", "Big Buck Test (2008)-trailer.mp4", "Big Buck Test (2008).mp4"]
        assert [f["video"]["height"] for f in bb["files"]] == [360, 270, 240]
        assert bb["files"][1]["label"] == "270p · H264" and bb["files"][1]["path"] == "Big Buck Test (2008) - 270p.mkv"
        assert bb["images"]["thumb"] and bb["images"]["poster"] is None
        keys = {b["key"]: b for b in bb["display"]}
        for k in ("year", "resolution", "duration", "video_codec", "size"):
            assert k in keys, (k, sorted(keys))
        assert bb["overview"] is None and bb["credits"] == [] and bb["extras"] == [], "no nfo / tags / extras"
        assert len(get(f"/api/items/{find(r['items'], title='Stacked Test')['id']}")["files"]) == 2
        state["movie"] = bb
        state["versions"] = vt
    check("movies: Title (Year), scene names, versions grouped by title+year", t_movies)

    def t_shows():
        r = items(library_id=state["shows"]["id"])
        assert sorted(i["title"] for i in r["items"]) == ["Anime Test", "Some Show", "Test Show"]
        show = get(f"/api/items/{find(r['items'], title='Test Show')['id']}")
        check_detail(show)
        assert [c["title"] for c in show["children"]] == ["Specials", "Season 1", "Season 2"]
        assert show["images"]["thumb"], "container falls back to a child's thumb"
        s1 = get(f"/api/items/{show['children'][1]['id']}")
        check_detail(s1)
        assert s1["subtitle"] == "Test Show" and s1["index"] == 1
        assert [c["title"] for c in s1["children"]] == ["Pilot", "The Second", "Episode 3", "Double"]
        assert [(c["index"], c["index_end"]) for c in s1["children"]] == [(1, None), (2, None), (3, None), (4, 5)]
        assert [c["subtitle"] for c in s1["children"]] == ["S01E01", "S01E02", "S01E03", "S01E04-E05"]
        ep = get(f"/api/items/{s1['children'][0]['id']}")
        check_detail(ep)
        assert [b["title"] for b in ep["breadcrumbs"]] == ["Test Show", "Season 1"]
        anime = get(f"/api/items/{find(r['items'], title='Anime Test')['id']}")
        eps = get(f"/api/items/{anime['children'][0]['id']}")["children"]
        assert [(e["index"], e["index_end"]) for e in eps] == [(1, 2), (3, None)]
        some = get(f"/api/items/{find(r['items'], title='Some Show')['id']}")
        assert some["children"][0]["title"] == "Season 3"
        assert items(library_id=state["shows"]["id"], kind="episode")["total"] == 9
        state.update(show=show, season1=s1)
    check("shows: SxxEyy / NxNN / epNN / multi-episode -> show > season > episode", t_shows)

    def t_music():
        r = items(library_id=state["music"]["id"])
        tops = {i["title"]: i for i in r["items"]}
        assert set(tops) == {"Other Band", "Artist Two", "Disc One", "Disc Two", "Opening", "Middle", "Closing", "Embedded"}, set(tops)
        artist = get(f"/api/items/{tops['Other Band']['id']}")
        check_detail(artist)
        assert artist["kind"] == "artist" and [c["title"] for c in artist["children"]] == ["Single"]
        assert [(c["role"], c["person"]["name"], c["source"]) for c in artist["credits"]] == [("artist", "Other Band", "scan")]
        tr = get(f"/api/items/{artist['children'][0]['id']}")
        check_detail(tr)
        assert tr["kind"] == "track" and tr["subtitle"] == "Other Band" and tr["files"][0]["video"] is None
        assert tops["Opening"]["kind"] == "track" and tops["Opening"]["index"] == 1 and tops["Closing"]["index"] == 3
        state["artist"] = artist
        state["track_person"] = artist["credits"][0]["person"]["id"]
    check("music: audio-only -> track, Artist - Title under artist", t_music)

    def t_music_videos():
        r = items(library_id=state["music_videos"]["id"])
        mvs = {i["title"]: i for i in r["items"]}
        assert set(mvs) == {"First Hit", "Nfo Song", "Pipe Song", "Second Hit", "Clip"}, set(mvs)
        assert mvs["First Hit"]["subtitle"] == "Test Singer" and mvs["Pipe Song"]["subtitle"] == "Some Film · Lead Star"
        pipe = get(f"/api/items/{mvs['Pipe Song']['id']}")
        assert pipe["fields"].get("youtube") == "https://youtu.be/abcdefghijk", pipe["fields"]
        yt = [b for b in pipe["display"] if b["key"] == "youtube"]
        assert yt and yt[0]["widget"] == "link"
        mv = get(f"/api/items/{mvs['First Hit']['id']}")
        check_detail(mv)
        assert mv["credits"] == [] and len(mv["files"]) == 1, "no .transcodes, no tags"
        state["mv"] = mv
    check("music videos: Artist - Title, ｜ uploads with [id]", t_music_videos)

    def t_videos():
        r = items(library_id=state["videos"]["id"])
        titles = sorted(i["title"] for i in r["items"])
        assert titles == ["Bloopers", "Daily Show 2024.10.31", "Daily Show 2024.11.01", "Making Of", "beach day", "holiday clip", "mountain", "sample"], titles
        assert all(i["kind"] == "video" and i["images"]["thumb"] for i in r["items"])
        state["videos_list"] = r["items"]
    check("videos: everything else, titled from the cleaned file name", t_videos)

    def t_query_params():
        s, b = req("GET", "/api/items/0000000000000000")
        assert s == 404 and "error" in b
        assert req("GET", "/api/items?sort=nope")[0] == 400
        assert req("GET", "/api/items?kind=extra")[0] == 400
        allt = items(library_id=state["shows"]["id"])
        assert all(i["parent_id"] is None for i in allt["items"])
        notop = items(library_id=state["shows"]["id"], top="false")
        assert notop["total"] == 3 + 5 + 9, notop["total"]  # shows + seasons + episodes
        added = items(library_id=state["videos"]["id"], sort="added")
        ts = [i["added_at"] for i in added["items"]]
        assert ts == sorted(ts, reverse=True)
        yr = items(library_id=state["movies"]["id"], sort="year", order="asc")
        assert [i["year"] for i in yr["items"]] == [2000, 2005, 2006, 2008, 2010, 2015, 2022]
        byp = items(person_id=state["track_person"])
        assert [i["title"] for i in byp["items"]] == ["Other Band"]
        page = items(library_id=state["videos"]["id"], limit=3, offset=3)
        assert len(page["items"]) == 3 and page["total"] == 8
    check("items query params", t_query_params)

    def t_stream():
        f = state["mv"]["files"][0]
        st, h, body = req("GET", f["stream_url"], headers={"Range": "bytes=0-99"}, raw=True)
        assert st == 206 and len(body) == 100, st
        assert h["Content-Range"] == f"bytes 0-99/{f['size']}", h["Content-Range"]
        assert h["Content-Type"] == "video/x-matroska" and h.get("Accept-Ranges") == "bytes"
        with open(blob_path(state["media_dir"], f), "rb") as fh:
            assert fh.read(100) == body
        st, h, _ = req("HEAD", f["stream_url"], raw=True)
        assert st == 200 and int(h["Content-Length"]) == f["size"]
        st, h, body = req("GET", f["stream_url"], headers={"Range": f"bytes={f['size'] - 10}-"}, raw=True)
        assert st == 206 and len(body) == 10
        st, h, _ = req("GET", f["stream_url"], headers={"Range": "bytes=0-9", "Accept-Encoding": "gzip"}, raw=True)
        assert h.get("Content-Encoding") is None
        assert req("GET", "/stream/0000000000000000", raw=True)[0] == 404
        assert req("GET", f"/subtitle/{f['id']}/0", raw=True)[0] == 404
    check("stream: Range 206, HEAD, no gzip, bytes from the store", t_stream)

    def t_images():
        mv = state["movie"]
        st, h, body = req("GET", mv["images"]["thumb"], raw=True, expect=200)
        assert h["Content-Type"] == "image/jpeg" and body[:3] == b"\xff\xd8\xff" and "max-age" in h.get("Cache-Control", "")
        assert req("GET", f"/image/item/{mv['id']}/poster", raw=True)[0] == 404
        assert req("GET", f"/image/item/{mv['id']}/bogus", raw=True)[0] == 400
        tr = state["artist"]["children"][0]
        assert tr["images"]["thumb"] is None and req("GET", f"/image/item/{tr['id']}/thumb", raw=True)[0] == 404
        st, h, _ = req("GET", "/api/fields", headers={"Accept-Encoding": "gzip"}, raw=True, expect=200)
        assert h.get("Content-Encoding") == "gzip"
    check("images: generated thumbs, cache headers, gzip JSON", t_images)

    def t_frames():
        f = state["mv"]["files"][0]
        t = 1.02
        st, h, body = req("GET", f"/frame/{f['id']}?t={t}", raw=True, expect=200)
        assert h["Content-Type"] == "image/jpeg" and body[:3] == b"\xff\xd8\xff"
        assert int(h["X-Frame-Width"]) == f["video"]["width"] and int(h["X-Frame-Height"]) == f["video"]["height"]
        pts = float(h["X-Frame-Pts"])
        assert t <= pts < t + 1 / f["video"]["fps"] + 0.002, pts
        assert "max-age=31536000" in h["Cache-Control"]
        st, h2, body2 = req("GET", f"/frame/{f['id']}?t={t}", raw=True, expect=200)
        assert body2 == body and h2["X-Frame-Cache"] == "hit"
        w = 320
        st, h, body = req("GET", f"/frame/{f['id']}?t=0.5&max_w={w}&fmt=webp", raw=True, expect=200)
        exp_h = round(f["video"]["height"] * w / f["video"]["width"] / 2) * 2
        assert h["Content-Type"] == "image/webp" and body[:4] == b"RIFF" and h["X-Frame-Width"] == str(w)
        assert abs(int(h["X-Frame-Height"]) - exp_h) <= 2, (h["X-Frame-Height"], exp_h)
        for q in ("t=-1", "t=99999", "", "t=1&fmt=gif"):
            s, b = req("GET", f"/frame/{f['id']}?{q}")
            assert s == 400 and "error" in b, (q, s)
        audio = state["artist"]["children"][0]
        af = get(f"/api/items/{audio['id']}")["files"][0]
        assert req("GET", f"/frame/{af['id']}?t=0.5")[0] == 400
        assert req("GET", "/frame/0000000000000000?t=1")[0] == 404
    check("still frames", t_frames)

    def t_fields_list():
        fl = get("/api/fields")
        keys = [f["key"] for f in fl]
        for k in ("resolution", "fps", "hdr", "video_codec", "audio_codec", "container", "size", "bitrate",
                  "duration", "year", "genres", "rating", "content_rating", "studio", "album", "track_number",
                  "overview", "tagline", "language", "youtube"):
            assert k in keys, k
        for f in fl:
            has(f, "key", "label", "type", "builtin", "derived", "applies_to", "display")
            has(f["display"], "placement", "widget", "order", "format", "color", "icon")
        orders = [f["display"]["order"] for f in fl]
        assert orders == sorted(orders)
        fps = find(fl, key="fps")
        assert fps["builtin"] and fps["derived"] and fps["display"]["placement"] == "card_badge"
    check("fields list", t_fields_list)

    def t_edit():
        mv = state["mv"]
        d = req("PATCH", f"/api/items/{mv['id']}", {"title": "Edited Hit", "year": 1999, "genres": ["Pop"]}, expect=200)[1]
        check_detail(d)
        assert d["title"] == "Edited Hit" and d["year"] == 1999 and set(d["locked"]) >= {"title", "year", "genres"}
        d = req("PUT", f"/api/items/{mv['id']}/fields/rating", {"value": 8.5}, expect=200)[1]
        assert d["fields"]["rating"] == 8.5
        s, b = req("PUT", f"/api/items/{mv['id']}/fields/resolution", {"value": "8K"})
        assert s == 400 and "error" in b
        assert req("PATCH", f"/api/items/{mv['id']}", {"bogus": 1})[0] == 400
        d = req("PATCH", f"/api/items/{mv['id']}", {"title": "First Hit", "year": None, "genres": []}, expect=200)[1]
        for k in ("title", "year", "genres"):
            req("DELETE", f"/api/items/{mv['id']}/fields/{k}", expect=200)
        d = req("DELETE", f"/api/items/{mv['id']}/fields/rating", expect=200)[1]
        assert "rating" not in d["fields"] and d["locked"] == [] and d["title"] == "First Hit"
        # kind is editable on top-level playable items, which moves them between virtual libraries
        beach = find(state["videos_list"], title="beach day")
        d = req("PATCH", f"/api/items/{beach['id']}", {"kind": "movie", "year": 2023}, expect=200)[1]
        assert d["kind"] == "movie" and d["library_id"] == state["movies"]["id"] and "kind" in d["locked"]
        assert items(library_id=state["movies"]["id"])["total"] == 8
        d = req("PATCH", f"/api/items/{beach['id']}", {"kind": "video", "year": None}, expect=200)[1]
        assert d["library_id"] == state["videos"]["id"]
        ep = state["season1"]["children"][0]
        assert req("PATCH", f"/api/items/{ep['id']}", {"kind": "movie"})[0] == 400
        assert req("PATCH", f"/api/items/{beach['id']}", {"kind": "bogus"})[0] == 400
    check("edits: PATCH core fields + kind, locked, derived fields read-only", t_edit)

    def t_custom_field():
        key = "ct_mood"
        s, f = req("POST", "/api/fields", {"key": key, "label": "Vibe", "type": "text", "applies_to": ["music_video"],
                                          "display": {"placement": "card_badge", "widget": "badge", "order": 1,
                                                      "format": "~{value}~", "color": "#112233", "icon": "mood"}})
        assert s == 201 and f["key"] == key and f["builtin"] is False, (s, f)
        assert req("POST", "/api/fields", {"key": key, "label": "dup", "type": "text"})[0] == 409
        assert req("POST", "/api/fields", {"key": "fps", "label": "x", "type": "text"})[0] in (400, 409)
        mv = state["mv"]
        d = req("PUT", f"/api/items/{mv['id']}/fields/{key}", {"value": "chill"}, expect=200)[1]
        blk = [b for b in d["display"] if b["key"] == key]
        assert blk and blk[0]["text"] == "~chill~" and blk[0]["color"] == "#112233", blk
        card = find(items(library_id=state["music_videos"]["id"])["items"], id=mv["id"])["card"]
        assert card[0]["key"] == key, card
        d2 = req("PUT", f"/api/items/{state['movie']['id']}/fields/{key}", {"value": "x"}, expect=200)[1]
        assert not [b for b in d2["display"] if b["key"] == key]
        req("DELETE", f"/api/items/{state['movie']['id']}/fields/{key}", expect=200)
        f2 = req("PUT", f"/api/fields/{key}", {"label": "Mood!", "display": {"placement": "info", "widget": "text"}}, expect=200)[1]
        assert f2["label"] == "Mood!" and f2["display"]["placement"] == "info" and f2["display"]["color"] == "#112233"
        fps = req("PUT", "/api/fields/fps", {"display": {"color": "#00FF00"}}, expect=200)[1]
        assert fps["display"]["color"] == "#00FF00"
        assert req("PUT", "/api/fields/fps", {"applies_to": ["movie"]})[0] == 400
        req("PUT", "/api/fields/fps", {"display": {"color": "#FF4D6D"}}, expect=200)
        assert req("DELETE", "/api/fields/fps")[0] == 400
        req("DELETE", f"/api/fields/{key}", expect=200)
        d = get(f"/api/items/{mv['id']}")
        assert key not in d["fields"] and key not in [f["key"] for f in get("/api/fields")]
    check("custom field create -> set -> display -> delete", t_custom_field)

    def t_people_credits():
        s, p = req("POST", "/api/people", {"name": "Contract Person", "bio": "Bio text", "birth_date": "1990-01-02"})
        assert s == 201, (s, p)
        has(p, "id", "name", "sort_name", "image", "credit_count", "bio", "birth_date", "fields", "display", "credits")
        assert p["sort_name"] == "Person, Contract" and p["credit_count"] == 0 and p["image"] is None
        assert req("POST", "/api/people", {"name": "contract  person"})[0] == 409
        pid = p["id"]
        p = req("PATCH", f"/api/people/{pid}", {"bio": "New bio", "sort_name": "CP"}, expect=200)[1]
        assert p["bio"] == "New bio" and p["sort_name"] == "CP"
        png = bytes.fromhex("89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c4890000000d49444154789c6360000002000154a24f5d0000000049454e44ae426082")
        p = req("PUT", f"/api/people/{pid}/image", png, headers={"Content-Type": "image/png"}, expect=200)[1]
        st, h, body = req("GET", p["image"], raw=True, expect=200)
        assert body == png and h["Content-Type"] == "image/png"
        assert req("PUT", f"/api/people/{pid}/image", b"not an image")[0] == 400
        mv = state["mv"]
        d = req("POST", f"/api/items/{mv['id']}/credits", {"person_id": pid, "role": "dancer", "character": "Lead"}, expect=200)[1]
        c = [c for c in d["credits"] if c["person"]["id"] == pid][0]
        assert c["role"] == "dancer" and c["character"] == "Lead" and c["source"] == "manual"
        d = req("POST", f"/api/items/{mv['id']}/credits", {"person_name": "New Composer", "role": "Composer", "order": 9}, expect=200)[1]
        nc = [c for c in d["credits"] if c["person"]["name"] == "New Composer"][0]
        assert nc["role"] == "composer" and nc["order"] == 9
        assert req("POST", f"/api/items/{mv['id']}/credits", {"role": "guest"})[0] == 400
        assert req("POST", f"/api/items/{mv['id']}/credits", {"person_id": "0000000000000000", "role": "guest"})[0] == 404
        key = "ct_dancers"
        req("POST", "/api/fields", {"key": key, "label": "Dancers", "type": "person", "applies_to": [],
                                    "display": {"placement": "section", "widget": "people", "order": 150}}, expect=201)
        d = req("PUT", f"/api/items/{mv['id']}/fields/{key}", {"value": [pid]}, expect=200)[1]
        blk = [b for b in d["display"] if b["key"] == key][0]
        assert blk["people"][0]["id"] == pid and blk["text"] == "Contract Person"
        req("DELETE", f"/api/fields/{key}", expect=200)
        p = get(f"/api/people/{pid}")
        assert p["credit_count"] == 1 and p["credits"][0]["item"]["id"] == mv["id"]
        check_summary(p["credits"][0]["item"])
        roles = get("/api/people?role=composer")
        assert [x["name"] for x in roles["people"]] == ["New Composer"]
        lst = get("/api/people?q=contract")
        assert lst["total"] == 1 and lst["people"][0]["id"] == pid
        assert len(get("/api/people?limit=2")["people"]) == 2
        d = req("DELETE", f"/api/items/{mv['id']}/credits/{c['id']}", expect=200)[1]
        assert not any(x["id"] == c["id"] for x in d["credits"])
        assert req("DELETE", f"/api/items/{mv['id']}/credits/{c['id']}")[0] == 404
        req("DELETE", f"/api/people/{pid}", expect=200)
        assert req("GET", f"/api/people/{pid}")[0] == 404
        req("DELETE", f"/api/people/{nc['person']['id']}", expect=200)
        assert get(f"/api/items/{mv['id']}")["credits"] == []
    check("people CRUD, image upload, credits, person fields", t_people_credits)

    def t_search():
        r = get("/api/search?q=test")
        has(r, "items", "people")
        assert any(i["title"] == "Test Show" for i in r["items"])
        for i in r["items"]:
            check_summary(i)
        assert any(p["name"] == "Other Band" for p in get("/api/search?q=band")["people"])
        assert get("/api/search?q=") == {"items": [], "people": []}
    check("search", t_search)

    def t_playback():
        s1 = state["season1"]
        pb = get(f"/api/items/{s1['children'][1]['id']}/playback")
        has(pb, "start_index", "queue")
        assert pb["start_index"] == 0
        assert [q["item"]["title"] for q in pb["queue"]] == ["The Second", "Episode 3", "Double"]
        for q in pb["queue"]:
            has(q, "part", "item", "files")
            check_summary(q["item"])
            assert q["files"] and q["part"] is None
            for f in q["files"]:
                check_file(f)
        pb = get(f"/api/items/{state['show']['id']}/playback")
        assert [(q["item"]["parent_index"], q["item"]["index"]) for q in pb["queue"]] == [(0, 1), (1, 1), (1, 2), (1, 3), (1, 4), (2, 1)]
        pb = get(f"/api/items/{state['versions']['id']}/playback")
        assert len(pb["queue"]) == 1 and len(pb["queue"][0]["files"]) == 3
        pb = get(f"/api/items/{state['artist']['id']}/playback")
        assert [q["item"]["title"] for q in pb["queue"]] == ["Single"]
    check("playback queues", t_playback)

    # ---- uploads ----
    def upload(path, name, headers=None):
        with open(path, "rb") as fh:
            data = fh.read()
        h = {"X-Filename": urllib.parse.quote(name), "Content-Type": "application/octet-stream"}
        h.update(headers or {})
        return req("POST", "/api/ingest/upload", data, headers=h)

    def t_upload():
        a = os.path.join(work, "up-a.mkv")
        b = os.path.join(work, "up-b.mkv")
        make_video(a, 640, 360, 300)
        make_video(b, 320, 180, 350)
        s, rec = upload(a, "Uploaded Film (2021).mkv")
        assert s == 201, (s, rec)
        assert rec["state"] == "done" and rec["origin"] == "upload" and rec["placement"] == "move", rec
        assert rec["source"] == "upload:Uploaded Film (2021).mkv" and rec["size"] == os.path.getsize(a)
        d = get(f"/api/items/{rec['item_id']}")
        assert (d["kind"], d["title"], d["year"]) == ("movie", "Uploaded Film", 2021)
        f = d["files"][0]
        blob = blob_path(state["media_dir"], f)
        assert stat.S_IMODE(os.stat(blob).st_mode) == 0o444 and os.stat(blob).st_nlink == 1
        assert os.listdir(os.path.join(state["media_dir"], ".staging")) == []
        assert get(f"/api/ingest/{rec['id']}")["state"] == "done"
        # a second file of the same movie: another version
        s, rec2 = upload(b, "Uploaded Film (2021) - small.mkv")
        assert s == 201 and rec2["item_id"] == rec["item_id"], rec2
        assert [x["video"]["height"] for x in get(f"/api/items/{rec['item_id']}")["files"]] == [360, 180]
        # the same bytes again: duplicate, nothing stored
        s, dup = upload(a, "Another Name.mkv")
        assert s == 200 and dup["state"] == "duplicate" and dup["item_id"] == rec["item_id"] and dup["file_id"] == f["id"], dup
        assert len(get(f"/api/items/{rec['item_id']}")["files"]) == 2
        # unicode names
        c = os.path.join(work, "up-c.mkv")
        make_video(c, 320, 180, 400)
        s, rc = upload(c, "Singer ｜ Großes Lied [abcdefghij0].mkv")
        assert s == 201 and get(f"/api/items/{rc['item_id']}")["title"] == "Singer", rc
        # not media
        s, ig = upload(a, "notes.txt")
        assert s == 200 and ig["state"] == "ignored", ig
        with open(os.path.join(work, "junk.mkv"), "wb") as fh:
            fh.write(b"definitely not a video" * 100)
        s, bad = upload(os.path.join(work, "junk.mkv"), "Junk (2001).mkv")
        assert s == 200 and bad["state"] == "ignored", bad
        assert req("POST", "/api/ingest/upload", b"x")[0] == 400, "X-Filename required"
        assert os.listdir(os.path.join(state["media_dir"], ".staging")) == []
        state["uploaded"] = rec
    check("upload: streamed to .staging, moved into the store, versions, duplicates", t_upload)

    def t_duplicate_ingest():
        before = items(top="false")["total"]
        src = os.path.join(fixtures, "movies", "Sintel Test (2010)", "Sintel Test (2010).mkv")
        dst = os.path.join(state["ingest_dir"], "dupes", "Sintel Again.mkv")
        os.makedirs(os.path.dirname(dst))
        shutil.copy(src, dst)
        rec = wait_for(lambda: [r for r in get("/api/ingest?limit=1000") if r["source"] == dst and r["state"] in ("duplicate", "done", "failed")],
                       what="duplicate record")[0]
        assert rec["state"] == "duplicate", rec
        assert rec["item_id"] == state["records"][os.path.join("movies", "Sintel Test (2010)", "Sintel Test (2010).mkv")]["item_id"]
        assert items(top="false")["total"] == before
        assert os.path.exists(dst) and os.stat(dst).st_nlink == 1, "duplicates are left alone"
        # a partial download is never taken, even with a media name inside
        part = os.path.join(state["ingest_dir"], "dupes", "Later (2020).mkv.part")
        shutil.copy(src, part)
        time.sleep(3)
        assert not [r for r in get("/api/ingest?limit=1000") if r["source"] == part]
    check("ingest dir: duplicate content recorded, original untouched; .part ignored", t_duplicate_ingest)

    def t_delete():
        mountain = find(state["videos_list"], title="mountain")
        d = get(f"/api/items/{mountain['id']}")
        f = d["files"][0]
        blob = blob_path(state["media_dir"], f)
        rel = os.path.join("videos", "Holiday 2023", "mountain.mkv")
        orig = os.path.join(state["ingest_dir"], rel)
        assert os.stat(blob).st_ino == os.stat(orig).st_ino
        r = req("DELETE", f"/api/items/{mountain['id']}", expect=200)[1]
        assert r["ok"] is True and r["files"] == 1, r
        assert not os.path.exists(blob) and os.path.exists(orig)
        assert req("GET", f"/api/items/{mountain['id']}")[0] == 404
        assert req("GET", f["stream_url"])[0] == 404
        # the original stays in the ingest dir but is never ingested again
        req("POST", "/api/ingest/scan", expect=202)
        time.sleep(4)
        wait_idle()
        recs = [x for x in get("/api/ingest?limit=1000") if x["source"] == orig]
        assert len(recs) == 1, recs
        assert not [i for i in items(q="mountain")["items"]]
        # deleting a container removes its children and their blobs; empty parents go too
        some = find(items(library_id=state["shows"]["id"])["items"], title="Some Show")
        season = get("/api/items/" + some["id"])["children"][0]
        ep = get("/api/items/" + season["id"])["children"][0]
        epf = get(f"/api/items/{ep['id']}")["files"][0]
        r = req("DELETE", f"/api/items/{some['id']}", expect=200)[1]
        assert r["items"] == 3 and r["files"] == 1, r
        assert not os.path.exists(blob_path(state["media_dir"], epf))
        # one version
        up = get(f"/api/items/{state['uploaded']['item_id']}")
        small = up["files"][1]
        r = req("DELETE", f"/api/files/{small['id']}", expect=200)[1]
        assert r["files"] == 1 and r["items"] == 0
        assert [x["id"] for x in get(f"/api/items/{up['id']}")["files"]] == [up["files"][0]["id"]]
        assert not os.path.exists(blob_path(state["media_dir"], small))
        r = req("DELETE", f"/api/files/{up['files'][0]['id']}", expect=200)[1]
        assert r["items"] == 1 and req("GET", f"/api/items/{up['id']}")[0] == 404
        assert req("DELETE", "/api/files/0000000000000000")[0] == 404
        assert req("DELETE", "/api/items/0000000000000000")[0] == 404
        libs = {l["kind"]: l["item_count"] for l in get("/api/libraries")}
        assert libs["shows"] == 2 and libs["videos"] == 7 and libs["movies"] == 7, libs
    check("delete: item / container / version remove blobs; originals not re-ingested", t_delete)

    def t_relations():
        pool = [i for lib in ("movies", "videos") for i in items(library_id=state[lib]["id"])["items"] if i["playable"]]
        pool = [i for i in pool if i["title"] != "sample" and not i["title"].startswith("Sintel")]
        assert len(pool) >= 4, pool
        a, b, c, d = pool[:4]
        before = get("/api/relations")["total"]
        # validation
        assert req("POST", "/api/relations", {"type": "collection", "members": []})[0] == 400
        assert req("POST", "/api/relations", {"type": "collection"})[0] == 400
        assert req("POST", "/api/relations", {"members": [{"item_id": a["id"]}]})[0] == 400
        assert req("POST", "/api/relations", {"type": "bad-type!", "members": [{"item_id": a["id"]}]})[0] == 400
        assert req("POST", "/api/relations", {"type": "x", "props": [1], "members": [{"item_id": a["id"]}]})[0] == 400
        assert req("POST", "/api/relations", {"type": "x", "members": [{"item_id": a["id"]}, {"item_id": a["id"]}]})[0] == 400
        assert req("POST", "/api/relations", {"type": "x", "members": [{"item_id": "0000000000000000"}]})[0] == 404
        assert get("/api/relations")["total"] == before, "failed creates must not leave relations behind"
        # create (type is normalized)
        rel = req("POST", "/api/relations", {
            "type": "  Soundtrack Of ", "name": "My Pack", "props": {"color": "red"},
            "members": [{"item_id": a["id"], "role": "from"}, {"item_id": b["id"], "ord": 5, "role": " to "}]}, expect=201)[1]
        has(rel, "id", "type", "name", "props", "member_count", "created_at", "updated_at", "members")
        assert rel["type"] == "soundtrack_of" and rel["name"] == "My Pack" and rel["props"] == {"color": "red"}
        assert rel["member_count"] == 2
        ms = rel["members"]
        assert [m["item_id"] for m in ms] == [a["id"], b["id"]], ms
        assert [m["ord"] for m in ms] == [0, 5] and [m["role"] for m in ms] == ["from", "to"]
        for m in ms:
            has(m, "item_id", "ord", "role", "item")
            check_summary(m["item"])
        rid = rel["id"]
        # list / filter / types
        lst = get("/api/relations?type=Soundtrack%20Of")
        assert lst["total"] == 1 and lst["relations"][0]["id"] == rid and lst["relations"][0]["member_count"] == 2, lst
        assert "members" not in lst["relations"][0]
        assert get("/api/relations?q=pack")["total"] == 1 and get("/api/relations?q=nomatch")["total"] == 0
        assert get("/api/relations?type=other_type")["total"] == 0
        assert get("/api/relations?limit=1&offset=5")["relations"] == []
        assert {"type": "soundtrack_of", "count": 1} in get("/api/relation-types")
        # detail + item views
        assert get(f"/api/relations/{rid}")["members"][0]["item_id"] == a["id"]
        assert req("GET", "/api/relations/0000000000000000")[0] == 404
        ir = get(f"/api/items/{b['id']}/relations")
        assert len(ir) == 1 and ir[0]["id"] == rid and ir[0]["ord"] == 5 and ir[0]["role"] == "to", ir
        assert [r["id"] for r in get(f"/api/items/{a['id']}")["relations"]] == [rid]
        assert get(f"/api/items/{c['id']}/relations") == []
        assert req("GET", "/api/items/0000000000000000/relations")[0] == 404
        # an item can be in several relations
        rel2 = req("POST", "/api/relations", {"type": "collection", "members": [{"item_id": a["id"]}]}, expect=201)[1]
        assert rel2["name"] == "" and rel2["props"] == {}
        assert {r["id"] for r in get(f"/api/items/{a['id']}/relations")} == {rid, rel2["id"]}
        # relations never show up as items
        assert not any(i["id"] in (rid, rel2["id"]) for i in items(top="false")["items"])
        # patch
        p = req("PATCH", f"/api/relations/{rid}", {"name": "Renamed", "type": "Album", "props": {"n": 1}}, expect=200)[1]
        assert p["name"] == "Renamed" and p["type"] == "album" and p["props"] == {"n": 1} and p["member_count"] == 2
        assert req("PATCH", f"/api/relations/{rid}", {"type": ""})[0] == 400
        assert req("PATCH", "/api/relations/0000000000000000", {"name": "x"})[0] == 404
        # member upsert
        u = req("POST", f"/api/relations/{rid}/members", {"item_id": c["id"], "role": "extra"}, expect=200)[1]
        assert u["member_count"] == 3 and u["members"][-1]["item_id"] == c["id"] and u["members"][-1]["ord"] == 6, u["members"]
        u = req("POST", f"/api/relations/{rid}/members", {"item_id": c["id"], "ord": 1}, expect=200)[1]
        mc = [m for m in u["members"] if m["item_id"] == c["id"]][0]
        assert u["member_count"] == 3 and mc["ord"] == 1 and mc["role"] == "extra"
        assert [m["item_id"] for m in u["members"]] == [a["id"], c["id"], b["id"]]
        assert req("POST", f"/api/relations/{rid}/members", {"item_id": "0000000000000000"})[0] == 404
        assert req("POST", "/api/relations/0000000000000000/members", {"item_id": a["id"]})[0] == 404
        # member removal
        r = req("DELETE", f"/api/relations/{rid}/members/{c['id']}", expect=200)[1]
        assert r["member_count"] == 2 and c["id"] not in [m["item_id"] for m in r["members"]]
        assert req("DELETE", f"/api/relations/{rid}/members/{c['id']}")[0] == 404
        # replace
        r = req("PUT", f"/api/relations/{rid}/members", {"members": [{"item_id": c["id"]}, {"item_id": d["id"], "role": "to"}]}, expect=200)[1]
        assert [m["item_id"] for m in r["members"]] == [c["id"], d["id"]] and r["members"][1]["role"] == "to"
        assert get(f"/api/items/{a['id']}/relations")[0]["id"] == rel2["id"]
        assert req("PUT", f"/api/relations/{rid}/members", {"members": [{"item_id": "0000000000000000"}]})[0] == 404
        assert get(f"/api/relations/{rid}")["member_count"] == 2, "failed replace must roll back"
        # last member removed via the API -> relation deleted
        s, _ = req("DELETE", f"/api/relations/{rel2['id']}/members/{a['id']}", expect=204)
        assert req("GET", f"/api/relations/{rel2['id']}")[0] == 404
        # empty replace -> relation deleted
        req("PUT", f"/api/relations/{rid}/members", {"members": []}, expect=204)
        assert req("GET", f"/api/relations/{rid}")[0] == 404
        # delete relation leaves items alone
        r3 = req("POST", "/api/relations", {"type": "collection", "members": [{"item_id": a["id"]}, {"item_id": b["id"]}]}, expect=201)[1]
        req("DELETE", f"/api/relations/{r3['id']}", expect=204)
        assert req("GET", f"/api/relations/{r3['id']}")[0] == 404
        assert req("DELETE", f"/api/relations/{r3['id']}")[0] == 404
        get(f"/api/items/{a['id']}"); get(f"/api/items/{b['id']}")
        # deleting an item removes its membership; a relation left empty is deleted
        r4 = req("POST", "/api/relations", {"type": "collection", "members": [{"item_id": c["id"]}, {"item_id": d["id"]}]}, expect=201)[1]
        r5 = req("POST", "/api/relations", {"type": "collection", "members": [{"item_id": d["id"]}]}, expect=201)[1]
        req("DELETE", f"/api/items/{d['id']}", expect=200)
        assert get(f"/api/relations/{r4['id']}")["member_count"] == 1
        assert req("GET", f"/api/relations/{r5['id']}")[0] == 404
        req("DELETE", f"/api/items/{c['id']}", expect=200)
        assert req("GET", f"/api/relations/{r4['id']}")[0] == 404
        assert get("/api/relations")["total"] == before
    check("relations: CRUD, members, types, item views, auto-delete, validation", t_relations)

    def t_tasks():
        s = status()
        if not (s["last_sidecar_sync"] or s["last_verify"] or s["last_backup"]):
            wait_for(lambda: status()["last_backup"], timeout=20, what="periodic tasks (run the server with short intervals)")
        # sidecars: written for stored files, refreshed when the item changes
        sintel = find(items(library_id=state["movies"]["id"])["items"], title="Sintel Test")
        f = get(f"/api/items/{sintel['id']}")["files"][0]
        side = blob_path(state["media_dir"], f)[: -len(f["container"])] + "json"
        doc = wait_for(lambda: os.path.exists(side) and json.load(open(side)), what="sidecar")
        assert doc["hash"] == f["hash"] and doc["size"] == f["size"] and doc["original_name"] == "Sintel Test (2010).mkv"
        assert doc["item"]["title"] == "Sintel Test" and doc["item"]["kind"] == "movie" and doc["item"]["year"] == 2010
        req("PATCH", f"/api/items/{sintel['id']}", {"title": "Sintel Renamed"}, expect=200)
        wait_for(lambda: json.load(open(side))["item"]["title"] == "Sintel Renamed", what="sidecar refresh")
        ep = state["season1"]["children"][0]
        ef = get(f"/api/items/{ep['id']}")["files"][0]
        edoc = json.load(open(blob_path(state["media_dir"], ef)[: -len(ef["container"])] + "json"))
        assert [p["title"] for p in edoc["item"]["parents"]] == ["Test Show", "Season 1"], edoc
        req("DELETE", f"/api/items/{sintel['id']}/fields/title", expect=200)
        # backups
        if state.get("data_dir"):
            bdir = os.path.join(state["data_dir"], "backups")
            wait_for(lambda: os.path.isdir(bdir) and [n for n in os.listdir(bdir) if n.startswith("mediadir-") and n.endswith(".db")], what="backup")
        # integrity: a stored file that disappears is reported, and clears once its item is gone
        sample = find(state["videos_list"], title="sample")
        sf = get(f"/api/items/{sample['id']}")["files"][0]
        os.unlink(blob_path(state["media_dir"], sf))
        probs = wait_for(lambda: [p for p in status()["problems"] if p["file_id"] == sf["id"]], what="missing file reported")
        assert probs[0]["problem"] == "missing" and probs[0]["item_id"] == sample["id"], probs
        req("DELETE", f"/api/items/{sample['id']}", expect=200)
        wait_for(lambda: not [p for p in status()["problems"] if p["file_id"] == sf["id"]], what="problem cleared")
        assert status()["problems"] == []
    check("periodic tasks: sidecars (changed only), backups, integrity verify", t_tasks)

    shutil.rmtree(work, ignore_errors=True)
    print(f"\n{PASSES} passed, {len(FAILS)} failed")


if __name__ == "__main__":
    main()
    sys.exit(1 if FAILS else 0)
