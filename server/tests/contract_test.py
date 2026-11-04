#!/usr/bin/env python3
"""Contract tests for MediaDir v3 (docs/API.md + docs/STORAGE.md). Stdlib only (+ ffmpeg).

usage: contract_test.py http://127.0.0.1:8098

Needs a FRESH server (empty DATA_DIR / MEDIA_DIR / INGEST_DIR) on this machine, ideally with
short intervals so the periodic tasks run during the test:
  INGEST_SETTLE_SECS=1 INGEST_POLL_SECS=2 SIDECAR_INTERVAL=2 VERIFY_INTERVAL=3 BACKUP_INTERVAL=3
  ASSET_GC_INTERVAL=2 ASSET_GC_GRACE_SECS=6 ASSET_DIR=<fresh dir> ASSET_DEFAULT_WIDTH=1280
  ANALYTICS_PLAYBACK_IDLE_SECS=3 ANALYTICS_DEVICE_IDLE_SECS=4 ANALYTICS_SNAPSHOT_SECS=2
INGEST_DIR and MEDIA_DIR must be on one filesystem (placement is a rename); the test's scratch
dir is created next to INGEST_DIR, so symlink/hardlink fixtures live on that filesystem too.

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
WIDGETS = {"badge", "chip", "text", "rating", "link", "tags", "people", "markdown", "stat", "image"}
TYPES = {"text", "number", "date", "url", "tags", "rating", "bool", "duration", "person", "image"}
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
        "duration", "playable", "child_count", "aspect", "added_at", "card")
    assert "images" not in s or (isinstance(s["images"], dict) and s["images"]), "images map is omitted when empty"
    assert is_id(s["id"]) and is_id(s["library_id"]), (s["id"], s["library_id"])
    assert s["kind"] in ITEM_KINDS
    assert s["playable"] == (s["kind"] in PLAYABLE)
    assert s["aspect"] == ASPECT[s["kind"]], (s["kind"], s["aspect"])
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
        has(c["person"], "id", "name")
        assert "image" not in c["person"] and ("images" not in c["person"] or c["person"]["images"])
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


def make_image(work, ext, w, h):
    """A unique test image (random colour + noise) made with ffmpeg."""
    out = os.path.join(work, f"img-{uuid.uuid4().hex[:8]}.{ext}")
    colour = uuid.uuid4().hex[:6]
    ffmpeg("-f", "lavfi", "-i", f"color=c=0x{colour}:s={w}x{h},noise=alls=40:allf=t", "-frames:v", "1", out)
    data = open(out, "rb").read()
    os.unlink(out)
    return data


def probe_size(work, data, ext):
    p = os.path.join(work, f"probe-{uuid.uuid4().hex[:6]}.{ext}")
    open(p, "wb").write(data)
    out = subprocess.run(["ffprobe", "-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height",
                          "-of", "csv=p=0", p], capture_output=True, check=True, text=True).stdout.strip()
    os.unlink(p)
    w, h = out.split(",")
    return int(w), int(h)


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
            assert r["placement"] == "move", r["placement"]
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
            sb = os.lstat(blob)
            assert stat.S_ISREG(sb.st_mode) and sb.st_nlink == 1, rel
            assert not os.path.lexists(orig), f"{rel} must have left the ingest dir"
            assert stat.S_IMODE(os.stat(os.path.dirname(blob)).st_mode) == 0o700
            assert sb.st_size == f["size"] == r["size"]
        assert os.listdir(os.path.join(state["media_dir"], ".staging")) == []
        # the ingest dir is a pure drop zone: no media files and no leftover (empty) folders
        left = [os.path.join(d, n) for d, _, fs_ in os.walk(state["ingest_dir"]) for n in fs_
                if n.rsplit(".", 1)[-1].lower() in MEDIA_EXTS and not n.startswith(".") and not n.endswith(".part")
                and not any(p.startswith(".") for p in os.path.relpath(d, state["ingest_dir"]).split(os.sep))]
        assert left == [], left
    check("store: content-addressed, renamed in (ingest dir emptied of media), 0700 dirs", t_store)

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
        assert "images" not in bb, "no image fields yet -> no images map"
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
        assert all(i["kind"] == "video" for i in r["items"])
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

    # ---- analytics (needs ANALYTICS_PLAYBACK_IDLE_SECS=3 ANALYTICS_DEVICE_IDLE_SECS=4 ANALYTICS_SNAPSHOT_SECS=2) ----
    def an(path, expect=200):
        return get("/api/analytics" + path, expect)

    def t_an_traffic():
        f = state["mv"]["files"][0]
        t0 = int(time.time()) - 2
        H = {"X-MediaDir-Device": "ct-dev-1", "X-MediaDir-User": "Alice"}
        req("GET", "/api/items?limit=1", headers=H, expect=200)
        req("GET", "/api/items?limit=1", headers=H, expect=200)
        st, _, body = req("GET", f["stream_url"], headers={**H, "Range": "bytes=0-99"}, raw=True)
        assert st == 206 and len(body) == 100
        st, _, body = req("GET", f["stream_url"], headers=H, raw=True)
        assert st == 200 and len(body) == f["size"]
        req("HEAD", f["stream_url"], headers=H, raw=True)
        req("GET", "/stream/0000000000000000", headers=H, raw=True)  # 404 is logged too
        an("/overview")  # must not be logged

        def rows():
            r = an(f"/traffic?from={t0}&to={int(time.time()) + 5}&bucket=hour&group_by=route")
            by = {(x["key"]["route"], x["key"]["group"]): x for x in r["rows"]}
            s = by.get(("/stream/{file_id}", "stream"))
            return r, by if s and s["requests"] >= 4 else None
        r, by = wait_for(lambda: rows()[1] and rows(), timeout=15, what="traffic rows")
        has(r, "from", "to", "bucket", "group_by", "totals", "rows")
        s = by[("/stream/{file_id}", "stream")]
        # 100 (Range) + full file + 0 (HEAD) + small JSON error body for the 404
        assert s["bytes"] >= 100 + f["size"] and s["bytes"] < 100 + f["size"] + 200, s
        has(s, "t", "key", "requests", "bytes", "avg_ms", "p50_ms", "p95_ms", "source")
        assert s["source"] == "raw" and s["p50_ms"] is not None
        assert ("/api/items", "api") in by and by[("/api/items", "api")]["requests"] >= 2
        assert not [k for k in by if k[0].startswith("/api/analytics")], list(by)
        # exact bytes of a single Range request, by device+status
        r = an(f"/traffic?from={t0}&to={int(time.time()) + 5}&bucket=day&group_by=status")
        assert {x["key"]["status"] for x in r["rows"]} >= {"2xx", "4xx"}
        r = an(f"/traffic?from={t0}&to={int(time.time()) + 5}&bucket=day&group_by=device")
        d = [x for x in r["rows"] if x["key"]["device"] == "ct-dev-1"]
        assert d and d[0]["requests"] == 6, d  # 2 items + range + full + head + 404
        assert req("GET", "/api/analytics/traffic?bucket=year")[0] == 400
        assert req("GET", "/api/analytics/traffic?group_by=x")[0] == 400
    check("analytics: traffic rows (bytes incl. Range/HEAD/404), analytics endpoints not logged", t_an_traffic)

    def t_an_identity():
        req("GET", "/api/health?device=ct-dev-q&user=Bob", headers={"User-Agent": "ct-agent/1.0"}, expect=200)
        req("GET", "/api/health", headers={"User-Agent": "anon-agent/2"}, expect=200)
        req("GET", "/api/health?device=ignored", headers={"X-MediaDir-Device": "ct-hdr"}, expect=200)

        def devs():
            d = {x["id"]: x for x in an("/devices")["devices"]}
            return d if "ct-dev-q" in d and "ct-hdr" in d and any(k.startswith("anon:") for k in d) else None
        d = wait_for(devs, timeout=15, what="devices")
        q = d["ct-dev-q"]
        has(q, "id", "first_seen", "last_seen", "requests", "bytes", "user_agent", "ip", "users", "last_user", "active")
        assert q["users"] == ["Bob"] and q["user_agent"] == "ct-agent/1.0" and q["ip"] == "127.0.0.1", q
        anon = [x for k, x in d.items() if k.startswith("anon:")][0]
        assert anon["id"] == "anon:127.0.0.1" and anon["user_agent"]
        us = {u["name"]: u for u in an("/users")["users"]}
        assert "Bob" in us and "Alice" in us and "ct-dev-q" in us["Bob"]["devices"], us.keys()
        has(us["Alice"], "name", "first_seen", "last_seen", "requests", "bytes", "devices", "playback_sessions", "watch_time_s")
    check("analytics: device/user from header, query, anon:<ip>; user agent + ip; users/devices", t_an_identity)

    def t_an_inferred_playback():
        f = state["mv"]["files"][0]
        item = state["mv"]["id"]
        H = {"X-MediaDir-Device": "ct-play-1", "X-MediaDir-User": "Carol"}
        half = f["size"] // 2
        req("GET", f"{f['stream_url']}?device=ct-play-1&user=Carol", headers={"Range": f"bytes=0-{half - 1}"}, raw=True, expect=206)
        req("GET", f["stream_url"], headers={**H, "Range": f"bytes={half}-"}, raw=True, expect=206)

        def sess():
            r = an("/sessions?type=playback&device=ct-play-1")
            return r["sessions"][0] if r["total"] == 1 and r["sessions"][0]["requests"] == 2 else None
        s = wait_for(sess, timeout=15, what="inferred session")
        has(s, "id", "type", "device", "user", "file_id", "item_id", "item", "started_at", "last_at", "ended_at", "active", "source", "state",
            "bytes", "requests", "watched_s", "max_position_s", "position_s", "duration_s")
        assert s["source"] == "inferred" and s["active"] is True and s["user"] == "Carol" and s["file_id"] == f["id"] and s["item_id"] == item
        assert s["bytes"] == f["size"], s
        assert s["item"]["id"] == item and s["item"]["title"]
        if f.get("duration"):
            assert abs(s["watched_s"] - f["duration"]) < 0.2 and abs(s["max_position_s"] - f["duration"]) < 0.2, s
        npl = [x for x in an("/now-playing")["sessions"] if x["session_id"] == s["id"]]
        assert npl and npl[0]["item"]["id"] == item and npl[0]["device"] == "ct-play-1" and npl[0]["user"] == "Carol"
        has(npl[0], "state", "position_s", "duration_s", "source")
        # closes after the idle timeout
        done = wait_for(lambda: (lambda x: x if not x["active"] else None)(an(f"/sessions/{s['id']}")), timeout=25, what="session close")
        assert done["ended_at"] and "ranges" in done and done["ranges"] == [[0, f["size"]]], done
        assert not [x for x in an("/now-playing")["sessions"] if x["session_id"] == s["id"]]
        assert req("GET", "/api/analytics/sessions/pb_nope")[0] == 404
    check("analytics: playback session inferred from /stream (header + query identity, ranges union, idle close)", t_an_inferred_playback)

    def t_an_explicit_playback():
        f = state["mv"]["files"][0]
        H = {"X-MediaDir-Device": "ct-play-2", "X-MediaDir-User": "Dave"}

        def ev(event, pos, **kw):
            return req("POST", "/api/analytics/playback", {"file_id": f["id"], "event": event, "position_s": pos, **kw}, headers=H, expect=200)[1]
        req("GET", f["stream_url"], headers={**H, "Range": "bytes=0-999"}, raw=True, expect=206)  # inferred part
        a = ev("start", 0, duration_s=100)
        has(a, "ok", "session_id", "source", "state", "position_s", "watched_s")
        assert a["source"] == "explicit" and a["state"] == "playing"
        time.sleep(1.2)
        ev("progress", 1.0)
        time.sleep(1.2)
        b = ev("progress", 2.0)
        assert b["session_id"] == a["session_id"]
        c = ev("pause", 2.0)
        assert c["state"] == "paused"
        ev("resume", 2.0)
        time.sleep(0.3)
        ev("progress", 50.0)  # a seek: not counted as watched
        e = ev("stop", 50.0)
        assert e["state"] == "stopped" and e["active"] is False
        s = an(f"/sessions/{a['session_id']}")
        assert s["source"] == "explicit" and s["user"] == "Dave" and s["state"] == "stopped" and s["ended_at"]
        assert 1.8 <= s["watched_s"] <= 3.2, s
        assert s["max_position_s"] == 50.0 and s["duration_s"] == 100.0, s
        assert s["bytes"] == 1000 and s["inferred_watched_s"] >= 0
        # by item_id
        r = req("POST", "/api/analytics/playback", {"item_id": state["mv"]["id"], "event": "start", "position_s": 0}, headers={"X-MediaDir-Device": "ct-play-3"}, expect=200)[1]
        assert r["item_id"] == state["mv"]["id"]
        req("POST", "/api/analytics/playback", {"item_id": state["mv"]["id"], "event": "stop", "position_s": 0}, headers={"X-MediaDir-Device": "ct-play-3"}, expect=200)
        for body, code in [({"file_id": f["id"], "event": "dance", "position_s": 0}, 400), ({"file_id": f["id"], "event": "start"}, 400),
                           ({"event": "start", "position_s": 0}, 400), ({"file_id": "0000000000000000", "event": "start", "position_s": 0}, 404),
                           ({"file_id": f["id"], "event": "start", "position_s": -1}, 400)]:
            assert req("POST", "/api/analytics/playback", body, headers=H)[0] == code, body
    check("analytics: explicit playback events take precedence (exact position/watch time), validation", t_an_explicit_playback)

    def t_an_device_sessions_top():
        r = wait_for(lambda: (lambda x: x if x["total"] and not x["sessions"][0]["active"] else None)(an("/sessions?type=device&device=ct-dev-1")), timeout=25, what="device session close")
        s = r["sessions"][0]
        has(s, "id", "type", "device", "user", "started_at", "last_at", "ended_at", "active", "requests", "bytes")
        assert s["id"].startswith("dv_") and s["requests"] == 6 and s["user"] == "Alice", s
        assert an(f"/sessions/{s['id']}")["id"] == s["id"]
        assert an("/sessions?type=device&active=false&limit=1")["limit"] == 1
        assert req("GET", "/api/analytics/sessions?type=bogus")[0] == 400
        item = state["mv"]["id"]
        t = an("/top?metric=plays&by=item")
        has(t, "metric", "by", "from", "to", "rows")
        row = [x for x in t["rows"] if x["key"] == item][0]
        has(row, "key", "value", "plays", "bytes", "watch_time_s", "item")
        assert row["plays"] >= 3 and row["item"]["id"] == item
        assert an("/top?metric=bytes&by=user")["rows"][0]["value"] >= state["mv"]["files"][0]["size"]
        assert {x["key"] for x in an("/top?metric=watch_time&by=user")["rows"]} >= {"Carol", "Dave"}
        assert any(x["key"] == "ct-play-1" for x in an("/top?metric=plays&by=device")["rows"])
        assert any(x["key"] == state["mv"]["kind"] for x in an("/top?metric=plays&by=kind&limit=3")["rows"])
        assert req("GET", "/api/analytics/top?metric=nope")[0] == 400
        assert req("GET", "/api/analytics/top?by=nope")[0] == 400
    check("analytics: device sessions, top lists", t_an_device_sessions_top)

    def t_an_overview_library():
        o = an("/overview")
        has(o, "ts", "library", "traffic", "playback", "active", "devices", "users")
        for w in ("last_24h", "last_7d", "last_30d"):
            has(o["traffic"][w], "requests", "bytes", "by_group")
        assert o["traffic"]["last_24h"]["by_group"]["stream"]["bytes"] > 0
        has(o["active"], "playback_sessions", "device_sessions")
        lib = o["library"]
        has(lib, "items", "files", "assets", "people", "relations", "field_defs", "ingest", "disks")
        assert lib["items"]["total"] > 0 and lib["files"]["count"] > 0 and lib["files"]["by_location"]["store"]["count"] > 0
        has(lib["ingest"], "queue", "problems", "pending_hashes")
        for k in ("data", "media", "assets"):
            has(lib["disks"][k], "path", "total_bytes", "free_bytes")
            assert lib["disks"][k]["total_bytes"] >= lib["disks"][k]["free_bytes"] > 0

        def snap():
            r = an("/library?bucket=hour")
            return r if r["series"] and r["series"][-1]["items"]["total"] > 0 else None
        r = wait_for(snap, timeout=15, what="snapshot with items")
        has(r, "from", "to", "bucket", "series")
        p = r["series"][-1]
        has(p, "t", "ts", "items", "files", "assets", "disks")
        assert p["t"] % 3600 == 0 and p["files"]["by_container"] and p["files"]["by_mime"]
        assert len(r["series"]) == len({x["t"] for x in r["series"]})  # one point per bucket
        assert req("GET", "/api/analytics/library?bucket=x")[0] == 400
        assert req("GET", "/api/analytics/overview?x=1")[0] == 200
    check("analytics: overview + library snapshot series shapes", t_an_overview_library)

    def t_no_slots():
        mv = state["movie"]
        for path in (f"/image/item/{mv['id']}/thumb", f"/image/item/{mv['id']}/poster", "/image/person/abcdabcdabcdabcd"):
            assert req("GET", path, raw=True)[0] == 404, path
        assert req("PUT", "/api/people/abcdabcdabcdabcd/image", b"x")[0] in (404, 405)
        st, h, _ = req("GET", "/api/fields", headers={"Accept-Encoding": "gzip"}, raw=True, expect=200)
        assert h.get("Content-Encoding") == "gzip"
    check("no image slots / person image endpoints; gzip JSON", t_no_slots)

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
        assert req("PUT", "/api/fields/fps", {"applies_to": ["bogus"]})[0] == 400
        fps = req("PUT", "/api/fields/fps", {"applies_to": ["movie"]}, expect=200)[1]
        assert fps["applies_to"] == ["movie"] and fps["builtin"], fps
        assert [f for f in get("/api/fields") if f["key"] == "fps"][0]["applies_to"] == ["movie"]
        mvd = get(f"/api/items/{mv['id']}")
        assert not [b for b in mvd["display"] if b["key"] == "fps"], "excluded kind not presented"
        fps = req("DELETE", "/api/fields/fps", expect=200)[1]
        assert fps["builtin"] and fps["applies_to"] == ["movie", "episode", "music_video", "video"], fps
        assert fps["display"]["color"] == "#FF4D6D", fps
        req("DELETE", f"/api/fields/{key}", expect=200)
        d = get(f"/api/items/{mv['id']}")
        assert key not in d["fields"] and key not in [f["key"] for f in get("/api/fields")]
    check("custom field create -> set -> display -> delete", t_custom_field)

    def t_people_credits():
        s, p = req("POST", "/api/people", {"name": "Contract Person", "bio": "Bio text", "birth_date": "1990-01-02"})
        assert s == 201, (s, p)
        has(p, "id", "name", "sort_name", "credit_count", "bio", "birth_date", "fields", "display", "credits")
        assert p["sort_name"] == "Person, Contract" and p["credit_count"] == 0 and "image" not in p
        assert req("POST", "/api/people", {"name": "contract  person"})[0] == 409
        pid = p["id"]
        p = req("PATCH", f"/api/people/{pid}", {"bio": "New bio", "sort_name": "CP"}, expect=200)[1]
        assert p["bio"] == "New bio" and p["sort_name"] == "CP"
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
    check("people CRUD, credits, person fields", t_people_credits)

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
        assert os.stat(blob).st_nlink == 1 and stat.S_IMODE(os.stat(blob).st_mode) & 0o222, "stored files are not made read-only"
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
        assert os.path.isfile(blob) and not os.path.lexists(orig)
        r = req("DELETE", f"/api/items/{mountain['id']}", expect=200)[1]
        assert r["ok"] is True and r["files"] == 1, r
        assert not os.path.exists(blob) and not os.path.lexists(orig)
        assert req("GET", f"/api/items/{mountain['id']}")[0] == 404
        assert req("GET", f["stream_url"])[0] == 404
        # nothing is left in the ingest dir, so nothing can be ingested again
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
        assert req("POST", "/api/relations", {"type": "x", "members": [{"item_id": a["id"]}, {"item_id": a["id"]}]})[0] == 400
        assert req("POST", "/api/relations", {"type": "x", "members": [{"item_id": "0000000000000000"}]})[0] == 404
        assert get("/api/relations")["total"] == before, "failed creates must not leave relations behind"
        # create (type is normalized)
        rel = req("POST", "/api/relations", {
            "type": "  Soundtrack Of ", "name": "My Pack", "props": {"color": "red"},  # props is ignored now
            "members": [{"item_id": a["id"], "role": "from"}, {"item_id": b["id"], "ord": 5, "role": " to "}]}, expect=201)[1]
        has(rel, "id", "type", "name", "member_count", "created_at", "updated_at", "members")
        assert rel["type"] == "soundtrack_of" and rel["name"] == "My Pack"
        assert "props" not in rel
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
        assert rel2["name"] == ""
        assert {r["id"] for r in get(f"/api/items/{a['id']}/relations")} == {rid, rel2["id"]}
        # relations never show up as items
        assert not any(i["id"] in (rid, rel2["id"]) for i in items(top="false")["items"])
        # patch
        p = req("PATCH", f"/api/relations/{rid}", {"name": "Renamed", "type": "Album"}, expect=200)[1]
        assert p["name"] == "Renamed" and p["type"] == "album" and p["member_count"] == 2
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

    # ---------------------------------------------------------------- image assets
    def t_assets():
        pool = [i for i in items(library_id=state["movies"]["id"])["items"]]
        item = pool[0]
        pid = req("POST", "/api/people", {"name": "Asset Person"}, expect=201)[1]["id"]
        jpg = make_image(work, "jpg", 800, 600)
        png = make_image(work, "png", 400, 200)
        # upload + dedupe
        s, a = req("POST", "/api/assets", jpg, headers={"Content-Type": "image/jpeg"})
        assert s == 201, (s, a)
        has(a, "hash", "mime", "width", "height", "size")
        assert len(a["hash"]) == 64 and a["mime"] == "image/jpeg" and (a["width"], a["height"]) == (800, 600) and a["size"] == len(jpg)
        s, a2 = req("POST", "/api/assets", jpg)
        assert s == 200 and a2 == a, "same bytes -> existing asset"
        assert req("POST", "/api/assets", b"definitely not an image")[0] == 400
        assert req("POST", "/api/assets", b"")[0] == 400
        pa = req("POST", "/api/assets", png, expect=201)[1]
        assert pa["mime"] == "image/png" and (pa["width"], pa["height"]) == (400, 200)
        # stored content-addressed in ASSET_DIR (separate from the media store), original untouched
        adir = status()["asset_dir"]
        rel = os.path.join(adir, a["hash"][:2], a["hash"][2:4], a["hash"] + ".jpg")
        assert os.path.isfile(rel) and open(rel, "rb").read() == jpg and not adir.startswith(state["media_dir"])
        # original: no params, ?original=1, unknown params
        for q in ("", "?original=1", "?v=abc", "?original=1&w=100&fmt=png"):
            st, h, body = req("GET", f"/asset/{a['hash']}{q}", raw=True, expect=200)
            assert body == jpg and h["Content-Type"] == "image/jpeg", q
            assert "immutable" in h["Cache-Control"] and h["ETag"] == '"%s"' % a["hash"], h
        st, h, _ = req("GET", f"/asset/{a['hash']}", headers={"If-None-Match": '"%s"' % a["hash"]}, raw=True)
        assert st == 304
        assert req("GET", "/asset/" + "0" * 64, raw=True)[0] == 404
        assert req("GET", "/asset/nothex", raw=True)[0] == 404
        # transforms: w, h, fmt, fit; cache miss then hit
        st, h, body = req("GET", f"/asset/{a['hash']}?w=400&fmt=webp", raw=True, expect=200)
        assert h["Content-Type"] == "image/webp" and body[:4] == b"RIFF" and body[8:12] == b"WEBP", body[:12]
        assert (h["X-Asset-Width"], h["X-Asset-Height"]) == ("400", "300") and h["X-Asset-Cache"] == "miss"
        assert probe_size(work, body, "webp") == (400, 300)
        st, h2, body2 = req("GET", f"/asset/{a['hash']}?fmt=webp&w=400", raw=True, expect=200)
        assert h2["X-Asset-Cache"] == "hit" and body2 == body and h2["ETag"] == h["ETag"]
        assert req("GET", f"/asset/{a['hash']}?w=400", raw=True, expect=200)[2] == body, "webp is the default format"
        assert req("GET", f"/asset/{a['hash']}?w=400&fmt=webp", headers={"If-None-Match": h["ETag"]}, raw=True)[0] == 304
        st, h, body = req("GET", f"/asset/{a['hash']}?h=150&fmt=jpg&q=70", raw=True, expect=200)
        assert h["Content-Type"] == "image/jpeg" and body[:3] == b"\xff\xd8\xff" and probe_size(work, body, "jpg") == (200, 150)
        st, h, body = req("GET", f"/asset/{a['hash']}?w=200&h=200&fmt=png", raw=True, expect=200)
        assert h["Content-Type"] == "image/png" and body[:4] == b"\x89PNG" and probe_size(work, body, "png") == (200, 200), "cover crops to the box"
        st, h, body = req("GET", f"/asset/{a['hash']}?w=200&h=200&fit=contain&fmt=png", raw=True, expect=200)
        assert probe_size(work, body, "png") == (200, 150), "contain fits inside the box"
        # no upscaling; no size restriction otherwise
        st, h, body = req("GET", f"/asset/{a['hash']}?w=5000&fmt=png", raw=True, expect=200)
        assert (h["X-Asset-Width"], h["X-Asset-Height"]) == ("800", "600") and probe_size(work, body, "png") == (800, 600)
        st, h3, body3 = req("GET", f"/asset/{a['hash']}?w=9999&fmt=png", raw=True, expect=200)
        assert h3["X-Asset-Cache"] == "hit" and body3 == body, "clamped sizes share a cache entry"
        st, h, body = req("GET", f"/asset/{a['hash']}?w=123&h=77", raw=True, expect=200)
        assert (h["X-Asset-Width"], h["X-Asset-Height"]) == ("123", "77"), "arbitrary sizes are served"
        # default width: transform params without w/h (ASSET_DEFAULT_WIDTH=1280 > 800 original -> original size)
        st, h, body = req("GET", f"/asset/{a['hash']}?fmt=jpg", raw=True, expect=200)
        assert (h["X-Asset-Width"], h["X-Asset-Height"]) == ("800", "600")
        for q in ("w=0", "fit=zoom", "fmt=bmp", "q=0", "q=101", "w=abc"):
            assert req("GET", f"/asset/{a['hash']}?{q}", raw=True)[0] == 400, q

        # image field defs: nothing seeded, created like any custom field
        assert not any(f["type"] == "image" for f in get("/api/fields")), "no image fields are seeded"
        d = req("POST", "/api/fields", {"key": "ct_poster", "label": "Poster", "type": "image", "applies_to": []}, expect=201)[1]
        assert d["type"] == "image" and d["builtin"] is False
        req("POST", "/api/fields", {"key": "ct_photos", "label": "Photos", "type": "image", "applies_to": ["person"]}, expect=201)
        # set on an item: single ref
        det = req("PUT", f"/api/items/{item['id']}/fields/ct_poster", {"value": {"hash": a["hash"]}}, expect=200)[1]
        assert det["fields"]["ct_poster"] == {"hash": a["hash"]}
        assert not any(b["key"] == "ct_poster" for b in det["display"]), "hidden by default"
        # ...but every summary carries the raw image values regardless of placement
        assert det["images"] == {"ct_poster": {"hash": a["hash"]}}, det.get("images")
        lst = [i for i in items(top="false", limit=500)["items"] if i["id"] == item["id"]]
        assert lst and lst[0]["images"] == {"ct_poster": {"hash": a["hash"]}}, "list summary"
        other = next(i for i in items(top="false", limit=500)["items"] if i["id"] != item["id"])
        assert "images" not in other, "items without image values omit the map"
        sr = get("/api/search?q=" + urllib.parse.quote(item["title"]))
        assert any(i["id"] == item["id"] and i["images"]["ct_poster"] == {"hash": a["hash"]} for i in sr["items"]), "search summary"
        # ordered list, on a person too (uppercase hashes are normalized, extra keys dropped)
        pv = [{"hash": pa["hash"].upper(), "x": 1}, {"hash": a["hash"]}]
        p = req("PUT", f"/api/people/{pid}/fields/ct_photos", {"value": pv}, expect=200)[1]
        assert p["fields"]["ct_photos"] == [{"hash": pa["hash"]}, {"hash": a["hash"]}], p["fields"]
        assert "image" not in p and p["images"] == {"ct_photos": [{"hash": pa["hash"]}, {"hash": a["hash"]}]}, p.get("images")
        pl = [x for x in get("/api/people?q=Asset")["people"] if x["id"] == pid]
        assert pl and pl[0]["images"] == p["images"], "person summary in list"
        # invalid values
        for bad in ({"hash": "0" * 64}, [{"hash": "0" * 64}], {"hash": "zz"}, "deadbeef", a["hash"], [], 5, {"x": 1}, [a["hash"]]):
            assert req("PUT", f"/api/items/{item['id']}/fields/ct_poster", {"value": bad})[0] == 400, bad
            assert req("PUT", f"/api/people/{pid}/fields/ct_photos", {"value": bad})[0] == 400, bad
        assert get(f"/api/items/{item['id']}")["fields"]["ct_poster"] == {"hash": a["hash"]}, "failed set keeps the value"
        # clear with null
        det = req("PUT", f"/api/items/{item['id']}/fields/ct_poster", {"value": None}, expect=200)[1]
        assert "ct_poster" not in det["fields"]
        req("PUT", f"/api/items/{item['id']}/fields/ct_poster", {"value": {"hash": a["hash"]}}, expect=200)
        state.update(asset=a, asset_png=pa, asset_item=item["id"], asset_person=pid)
    check("assets: upload+dedupe, original, transforms, cache, no upscale, image fields, validation", t_assets)

    def t_asset_gc():
        a, pa = state["asset"], state["asset_png"]
        orphan_bytes = make_image(work, "jpg", 64, 64)
        orphan = req("POST", "/api/assets", orphan_bytes, expect=201)[1]
        req("GET", f"/asset/{orphan['hash']}?w=32", raw=True, expect=200)  # also creates a cache entry
        # inside the grace period nothing unreferenced is collected
        time.sleep(1.5)
        assert req("GET", f"/asset/{orphan['hash']}", raw=True)[0] == 200, "freshly uploaded assets survive"
        # after the grace period the orphan goes, attached assets stay
        wait_for(lambda: req("GET", f"/asset/{orphan['hash']}", raw=True)[0] == 404, timeout=40, what="orphan asset collected")
        assert status()["last_asset_gc"], "gc task ran"
        assert req("GET", f"/asset/{a['hash']}", raw=True)[0] == 200, "referenced asset kept"
        assert req("GET", f"/asset/{pa['hash']}", raw=True)[0] == 200, "referenced by a person kept"
        adir = status()["asset_dir"]
        assert not os.path.exists(os.path.join(adir, orphan["hash"][:2], orphan["hash"][2:4], orphan["hash"] + ".jpg"))
        # re-uploading the same bytes brings it back (the file was really removed)
        again = req("POST", "/api/assets", orphan_bytes, expect=201)[1]
        assert again["hash"] == orphan["hash"]
        # clearing the last reference lets the asset go after the grace period
        req("DELETE", f"/api/people/{state['asset_person']}/fields/ct_photos", expect=200)
        req("PUT", f"/api/items/{state['asset_item']}/fields/ct_poster", {"value": None}, expect=200)
        wait_for(lambda: req("GET", f"/asset/{pa['hash']}", raw=True)[0] == 404, timeout=40, what="released asset collected")
        # deleting the field definition drops the values too
        req("DELETE", "/api/fields/ct_photos", expect=200)
        req("DELETE", "/api/fields/ct_poster", expect=200)
        req("DELETE", f"/api/people/{state['asset_person']}", expect=200)
    check("assets: GC respects the grace period, keeps referenced assets, collects released ones", t_asset_gc)

    def t_asset_verify():
        b_bytes = make_image(work, "jpg", 32, 32)
        b = req("POST", "/api/assets", b_bytes, expect=201)[1]
        adir = status()["asset_dir"]
        path = os.path.join(adir, b["hash"][:2], b["hash"][2:4], b["hash"] + ".jpg")
        fid = "asset:" + b["hash"]
        # keep it referenced so GC leaves it alone while verify reports the damage
        req("POST", "/api/fields", {"key": "ct_vimg", "label": "V", "type": "image"}, expect=201)
        item = items(top="false", limit=1)["items"][0]
        req("PUT", f"/api/items/{item['id']}/fields/ct_vimg", {"value": {"hash": b["hash"]}}, expect=200)
        os.chmod(path, 0o644)
        os.unlink(path)
        probs = wait_for(lambda: [p for p in status()["problems"] if p["file_id"] == fid], what="missing asset reported")
        assert probs[0]["problem"] == "missing" and probs[0]["hash"] == b["hash"], probs
        # re-upload restores the file and the problem clears
        req("POST", "/api/assets", b_bytes, expect=200)
        wait_for(lambda: not [p for p in status()["problems"] if p["file_id"] == fid], what="asset problem cleared")
        req("DELETE", "/api/fields/ct_vimg", expect=200)
    check("assets: integrity verify reports missing files, re-upload heals", t_asset_verify)

    def t_tasks():
        s = status()
        has(s, "last_asset_gc", "asset_dir")
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

    def t_symlink():
        ing = state["ingest_dir"]
        assert "external_root" not in status() and "pending_hashes" in status()
        ext = os.path.join(work, "ext")  # a different tree than INGEST_DIR/MEDIA_DIR: where the media "lives"
        os.makedirs(os.path.join(ext, "Movies"), exist_ok=True)
        target = os.path.join(ext, "Movies", "Ref Film (2012).mkv")
        make_video(target, 320, 180, 777)
        os.chmod(target, 0o444)
        st0 = os.stat(target)
        link = os.path.join(ing, "Ref Film (2012).mkv")
        os.symlink(target, link)
        link_ino = os.lstat(link).st_ino
        # a relative link in a sub folder, a dangling one, and a hardlink to a file kept elsewhere
        rtarget = os.path.join(ext, "Movies", "Relative Film (2014).mkv")
        make_video(rtarget, 320, 180, 778)
        os.makedirs(os.path.join(ing, "rel"))
        rlink = os.path.join(ing, "rel", "Relative Film (2014).mkv")
        os.symlink(os.path.relpath(rtarget, os.path.dirname(rlink)), rlink)
        dlink = os.path.join(ing, "Dangling Film (2010).mkv")
        os.symlink(os.path.join(ext, "nope.mkv"), dlink)
        keep = os.path.join(ext, "Movies", "Hard Film (2015).mkv")
        make_video(keep, 320, 180, 779)
        hlink = os.path.join(ing, "Hard Film (2015).mkv")
        os.link(keep, hlink)
        keep_ino = os.stat(keep).st_ino
        before = len(get("/api/ingest?limit=1000"))
        wait_idle(min_records=before + 4)
        recs = {os.path.basename(r["source"]): r for r in get("/api/ingest?limit=1000") if r["source"].startswith(ing)}
        r = recs["Ref Film (2012).mkv"]
        assert r["state"] == "done" and r["placement"] == "symlink", r
        d = recs["Dangling Film (2010).mkv"]
        assert d["state"] == "failed" and "dangling" in d["error"] and d["item_id"] is None, d
        assert os.path.islink(dlink), "a failed entry stays in the ingest dir"
        assert not items(q="Dangling Film")["items"]
        # the link itself moved into the store; the ingest dir lost it, the target is untouched
        assert not os.path.lexists(link)
        item = get(f"/api/items/{r['item_id']}")
        assert item["kind"] == "movie" and item["title"] == "Ref Film" and len(item["files"]) == 1
        f = item["files"][0]
        assert f["location"] == "external" and f["hash_state"] in ("pending", "full"), f
        assert f["original_name"] == "Ref Film (2012).mkv" and f["video"]["height"] == 180
        if f["hash_state"] == "pending":
            fp = f["hash"][3:]
            e = os.path.join(state["media_dir"], "fp", fp[:2], fp + ".mkv")
            assert os.path.islink(e) and os.lstat(e).st_ino == link_ino and os.readlink(e) == target, e
        # stream + Range + frame work through the store symlink
        raw = open(target, "rb").read()
        st, h, body = req("GET", f["stream_url"], headers={"Range": "bytes=10-109"}, raw=True)
        assert st == 206 and body == raw[10:110] and h["Content-Range"] == f"bytes 10-109/{len(raw)}", (st, dict(h))
        st, h, body = req("GET", f["stream_url"], raw=True, expect=200)
        assert body == raw
        st, h, body = req("GET", f"/frame/{f['id']}?t=0.5", raw=True, expect=200)
        assert body[:2] == b"\xff\xd8", "jpeg frame"
        # lazy hash becomes the full hash, the store symlink is renamed to the hash path; identity unchanged
        def full():
            g = get(f"/api/items/{item['id']}")["files"][0]
            return g if g["hash_state"] == "full" else None
        g = wait_for(full, timeout=40, what="lazy full hash")
        assert g["id"] == f["id"] and len(g["hash"]) == 64 and not g["hash"].startswith("fp:"), g
        e = blob_path(state["media_dir"], g)
        assert os.path.islink(e) and os.readlink(e) == target, e
        assert os.stat(e).st_ino == st0.st_ino
        assert status()["pending_hashes"] == 0
        assert req("GET", g["stream_url"], headers={"Range": "bytes=0-9"}, raw=True)[0] == 206
        # relative link: re-created with an absolute target in the store, original gone
        ri = wait_for(lambda: [x for x in items(q="Relative Film")["items"]], timeout=40, what="relative link ingested")[0]
        rf = get(f"/api/items/{ri['id']}")["files"][0]
        assert rf["location"] == "external" and not os.path.lexists(rlink)
        wait_for(lambda: get(f"/api/items/{ri['id']}")["files"][0]["hash_state"] == "full", timeout=40, what="relative lazy hash")
        rf = get(f"/api/items/{ri['id']}")["files"][0]
        rl = os.readlink(blob_path(state["media_dir"], rf))
        assert os.path.isabs(rl) and os.path.samefile(rl, rtarget)
        assert req("GET", rf["stream_url"], headers={"Range": "bytes=0-9"}, raw=True)[0] == 206
        assert not os.path.isdir(os.path.join(ing, "rel")), "emptied sub folder pruned"
        # hardlink: the dropped link moved, the other link and the inode are intact
        hi = wait_for(lambda: [x for x in items(q="Hard Film")["items"]], timeout=40, what="hardlink ingested")[0]
        hf = get(f"/api/items/{hi['id']}")["files"][0]
        assert hf["location"] == "store" and not os.path.lexists(hlink)
        hb = blob_path(state["media_dir"], hf)
        assert os.stat(hb).st_ino == keep_ino == os.stat(keep).st_ino and os.stat(keep).st_nlink == 2
        # sidecars are never written for symlinked entries
        time.sleep(3)
        assert not os.path.exists(os.path.join(os.path.dirname(blob_path(state["media_dir"], g)), g["hash"] + ".json"))
        assert sorted(os.listdir(os.path.join(ext, "Movies"))) == sorted(["Ref Film (2012).mkv", "Relative Film (2014).mkv", "Hard Film (2015).mkv"])
        # a changed target is flagged, re-fingerprinted (hash pending again) and hashed again
        os.chmod(target, 0o644)
        os.utime(target, (st0.st_atime, st0.st_mtime + 100))
        os.chmod(target, 0o444)
        probs = wait_for(lambda: [p for p in status()["problems"] if p["file_id"] == f["id"]], timeout=30, what="changed file reported")
        assert probs[0]["problem"] == "changed" and probs[0]["item_id"] == item["id"], probs
        wait_for(full, timeout=40, what="hash after change")
        # delete: the store symlink goes, the target never
        r2 = req("DELETE", f"/api/items/{item['id']}", expect=200)[1]
        assert r2["ok"] is True and r2["files"] == 1, r2
        assert req("GET", f"/api/items/{item['id']}")[0] == 404
        assert not os.path.lexists(e) and os.path.isfile(target)
        st1 = os.stat(target)
        assert (st1.st_size, st1.st_mode, st1.st_ino) == (st0.st_size, st0.st_mode, st0.st_ino)
        assert st1.st_mtime == st0.st_mtime + 100
        assert open(target, "rb").read() == raw
        # deleting the hardlinked file removes only the store link
        req("DELETE", f"/api/items/{hi['id']}", expect=200)
        assert not os.path.lexists(hb) and os.stat(keep).st_ino == keep_ino and os.stat(keep).st_nlink == 1
        # a removed target is reported as missing (never deleted from our side)
        gone = os.path.join(ext, "Movies", "Gone Film (2013).mkv")
        make_video(gone, 160, 90, 333)
        glink = os.path.join(ing, "Gone Film (2013).mkv")
        os.symlink(gone, glink)
        gi = wait_for(lambda: [x for x in items(q="Gone Film")["items"]], timeout=40, what="gone film ingested")[0]
        gf = get(f"/api/items/{gi['id']}")["files"][0]
        assert gf["location"] == "external"
        os.unlink(gone)
        probs = wait_for(lambda: [p for p in status()["problems"] if p["file_id"] == gf["id"]], timeout=30, what="missing target reported")
        assert probs[0]["problem"] == "missing", probs
        assert req("GET", gf["stream_url"], raw=True)[0] == 404
        req("DELETE", f"/api/items/{gi['id']}", expect=200)
        wait_for(lambda: not [p for p in status()["problems"] if p["file_id"] == gf["id"]], what="problem cleared")
        req("DELETE", f"/api/items/{ri['id']}", expect=200)
        assert os.path.isfile(rtarget)
        # only the dangling link remains in the ingest dir
        assert os.listdir(ing).count("Dangling Film (2010).mkv") == 1
    check("rename placement: symlink (abs/relative/dangling), hardlink, Range, frame, lazy hash path, verify, delete keeps target", t_symlink)

    shutil.rmtree(work, ignore_errors=True)
    print(f"\n{PASSES} passed, {len(FAILS)} failed")


if __name__ == "__main__":
    main()
    sys.exit(1 if FAILS else 0)
