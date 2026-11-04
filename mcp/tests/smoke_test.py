#!/usr/bin/env python3
"""Smoke test: spawns a DISPOSABLE MediaDir server (temp dirs, port 8199 by default), seeds a
generated clip via upload, then drives the MCP server over stdio with the MCP client SDK.

usage: .venv/bin/python tests/smoke_test.py [path/to/mediadir-server]
Never touches port 8096.
"""
import asyncio, json, os, shutil, subprocess, sys, tempfile, time, urllib.parse, urllib.request

import httpx
from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
_cands = [os.path.join(ROOT, "server", "target", d, "mediadir-server") for d in ("release", "debug")]
_cands = [c for c in _cands if os.path.exists(c)]
BIN = os.path.abspath(sys.argv[1]) if len(sys.argv) > 1 else max(_cands, key=os.path.getmtime)
PORT = int(os.environ.get("SMOKE_PORT", "8199"))
assert PORT != 8096
URL = f"http://127.0.0.1:{PORT}"
passes = fails = 0


def ok(name, cond, extra=""):
    global passes, fails
    if cond:
        passes += 1
        print(f"  ok   {name}")
    else:
        fails += 1
        print(f"  FAIL {name} {extra}")


async def main():
    tmp = tempfile.mkdtemp(prefix="mediadir-mcp-smoke-")
    srv = None
    try:
        env = dict(os.environ, INGEST_DIR=f"{tmp}/ingest", MEDIA_DIR=f"{tmp}/store", DATA_DIR=f"{tmp}/data",
                   BIND=f"127.0.0.1:{PORT}", INGEST_SETTLE_SECS="1", INGEST_POLL_SECS="2",
                   ASSET_DIR=f"{tmp}/assets")
        srv = subprocess.Popen([BIN], env=env, cwd=tmp, stdout=open(f"{tmp}/server.log", "w"), stderr=subprocess.STDOUT)
        for _ in range(60):
            try:
                if httpx.get(URL + "/api/health", timeout=1).json().get("ok"):
                    break
            except Exception:
                pass
            time.sleep(0.5)
        else:
            raise RuntimeError("disposable server did not start")
        clip = f"{tmp}/clip.mkv"
        subprocess.run(["ffmpeg", "-v", "error", "-f", "lavfi", "-i", "testsrc=size=320x180:rate=15:duration=2",
                        "-f", "lavfi", "-i", "sine=duration=2", "-c:v", "libx264", "-c:a", "aac", "-shortest", clip], check=True)
        with open(clip, "rb") as f:
            r = httpx.post(URL + "/api/ingest/upload", content=f.read(),
                           headers={"X-Filename": urllib.parse.quote("Smoke Film (2020).mkv")}, timeout=60)
        assert r.status_code == 201, r.text

        params = StdioServerParameters(command=sys.executable, args=["-m", "mediadir_mcp"],
                                       env=dict(os.environ, MEDIADIR_URL=URL))
        async with stdio_client(params) as (rd, wr), ClientSession(rd, wr) as s:
            await s.initialize()

            async def call(_tool, **args):
                res = await s.call_tool(_tool, args)
                if res.isError:
                    return None, res.content[0].text
                return json.loads(res.content[0].text), None

            tools = {t.name for t in (await s.list_tools()).tools}
            ok("tool list", {"list_libraries", "update_item", "create_relation", "ingest_status", "upload_asset", "asset_url", "get_asset_info", "set_image_field"} <= tools, tools)
            ok("set_person_image removed", "set_person_image" not in tools)
            ok("no destructive item/file tools", not any(("delete_item" == t or "delete_file" in t or "delete_media" in t) for t in tools))

            libs, e = await call("list_libraries")
            ok("list_libraries", libs and libs[0]["kind"] == "movies", (libs, e))
            res, e = await call("list_items", library_id=libs[0]["id"])
            ok("list_items", res and res["total"] == 1, (res, e))
            iid = res["items"][0]["id"]
            res, e = await call("search", q="Smoke")
            ok("search", res and res["items"], (res, e))
            it, e = await call("get_item", item_id=iid)
            ok("get_item", it and it["title"] and it["files"] and it["files"][0]["video"]["width"] == 320, (it, e))

            it, e = await call("update_item", item_id=iid, title="Smoke Film Edited", year=1999, genres=["Test"], overview="hi")
            ok("update_item", it and it["title"] == "Smoke Film Edited" and it["year"] == 1999 and "title" in it["locked"], (it, e))
            it, e = await call("unlock_field", item_id=iid, field="overview")
            ok("unlock_field", it and "overview" not in it["locked"], (it, e))
            _, e = await call("update_item", item_id=iid)
            ok("update_item with no args is a tool error", e is not None)
            _, e = await call("get_item", item_id="nope")
            ok("API 404 surfaces as tool error", e is not None and "404" in e, e)

            d, e = await call("create_field_def", key="mood", label="Mood", type="text", applies_to=["movie"])
            ok("create_field_def", d and d["key"] == "mood", (d, e))
            defs, e = await call("list_field_defs")
            ok("list_field_defs", any(x["key"] == "mood" for x in defs))
            it, e = await call("set_item_field", item_id=iid, key="mood", value="cheerful")
            ok("set_item_field", it and it["fields"].get("mood") == "cheerful", (it, e))
            d, e = await call("update_field_def", key="mood", label="Mood 2")
            ok("update_field_def", d and d["label"] == "Mood 2", (d, e))
            it, e = await call("delete_item_field", item_id=iid, key="mood")
            ok("delete_item_field", it and "mood" not in it["fields"], (it, e))
            _, e = await call("delete_field_def", key="mood")
            ok("delete_field_def", e is None, e)


            # ---- images
            subprocess.run(["ffmpeg", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=200x300", "-frames:v", "1", f"{tmp}/a.png"], check=True)
            subprocess.run(["ffmpeg", "-v", "error", "-y", "-f", "lavfi", "-i", "smptebars=size=200x300", "-frames:v", "1", f"{tmp}/b.jpg"], check=True)
            shutil.copy(f"{tmp}/a.png", f"{tmp}/a_copy.png")
            a1, e = await call("upload_asset", path_or_url=f"{tmp}/a.png")
            ok("upload_asset", a1 and len(a1["hash"]) == 64 and a1["width"] == 200 and a1["height"] == 300, (a1, e))
            a2, e = await call("upload_asset", path_or_url=f"{tmp}/a_copy.png")
            ok("upload dedupe (same hash)", a2 and a2["hash"] == a1["hash"], (a2, e))
            a3, e = await call("upload_asset", path_or_url=URL + f"/asset/{a1['hash']}")
            ok("upload_asset from URL dedupes", a3 and a3["hash"] == a1["hash"], (a3, e))
            _, e = await call("upload_asset", path_or_url=f"{tmp}/clip.mkv")
            ok("upload non-image is tool error", e is not None)
            _, e = await call("upload_asset", path_or_url=f"{tmp}/missing.png")
            ok("upload missing file is tool error", e is not None)
            u, e = await call("asset_url", hash=a1["hash"], w=100, fmt="jpg")
            ok("asset_url", u and u["url"] == f"{URL}/asset/{a1['hash']}?w=100&fmt=jpg", (u, e))
            r = httpx.get(u["url"])
            ok("asset_url serves resized jpeg", r.status_code == 200 and r.headers["content-type"] == "image/jpeg")
            inf, e = await call("get_asset_info", hash=a1["hash"])
            ok("get_asset_info", inf and inf["mime"] == "image/png" and inf["size"] == a1["size"], (inf, e))
            _, e = await call("get_asset_info", hash="0" * 64)
            ok("get_asset_info unknown hash errors", e is not None)

            d, e = await call("create_field_def", key="poster", label="Poster", type="image", applies_to=["movie", "person"])
            ok("create_field_def image", d and d["type"] == "image", (d, e))
            it, e = await call("set_image_field", owner="item", id=iid, key="poster", sources=[f"{tmp}/a.png"])
            ok("set_image_field item single ref", it and it["fields"]["poster"] == "img:" + a1["hash"], (it, e))
            raw = httpx.get(f"{URL}/api/items/{iid}").json()["fields"]["poster"]
            ok("stored as single ref", raw == {"hash": a1["hash"]}, raw)
            it, e = await call("set_image_field", owner="item", id=iid, key="poster", sources=[f"{tmp}/b.jpg"], append=True)
            ok("append gives list", it and isinstance(it["fields"]["poster"], list) and len(it["fields"]["poster"]) == 2
               and it["fields"]["poster"][0] == "img:" + a1["hash"], (it, e))
            it, e = await call("set_image_field", owner="item", id=iid, key="poster", sources=[a1["hash"]])
            ok("single source on list field stays list (hash source)", it and it["fields"]["poster"] == ["img:" + a1["hash"]], (it, e))
            it, e = await call("get_item", item_id=iid, full=True)
            ok("get_item full keeps raw refs", it and it["fields"]["poster"] == [{"hash": a1["hash"]}], (it, e))
            it, e = await call("get_item", item_id=iid)
            ok("get_item slim compacts images", it and it["fields"]["poster"] == ["img:" + a1["hash"]], (it, e))
            _, e = await call("set_image_field", owner="item", id=iid, key="poster", sources=["f" * 64])
            ok("unknown hash rejected", e is not None and "400" in e, e)
            pp, e = await call("create_person", name="Photo Person")
            pp_id = pp["id"]
            pp, e = await call("set_image_field", owner="person", id=pp_id, key="poster", sources=[f"{tmp}/b.jpg"])
            ok("set_image_field person", pp and pp["fields"]["poster"].startswith("img:"), (pp, e))
            pp, e = await call("get_person", person_id=pp_id)
            ok("get_person compacts image", pp and pp["fields"]["poster"].startswith("img:"), (pp, e))
            _, e = await call("delete_person", person_id=pp_id)
            ok("cleanup image person", e is None, e)

            p, e = await call("create_person", name="Jane Tester", bio="b")
            ok("create_person", p and p["name"] == "Jane Tester", (p, e))
            pid = p["id"]
            p, e = await call("update_person", person_id=pid, bio="new bio", birth_date="1990-01-02")
            ok("update_person", p and p["bio"] == "new bio", (p, e))
            it, e = await call("add_credit", item_id=iid, person_id=pid, role="actor", character="Hero")
            ok("add_credit", it and any(c["person"]["id"] == pid for c in it["credits"]), (it, e))
            cid = next(c["id"] for c in it["credits"] if c["person"]["id"] == pid)
            it, e = await call("update_credit", item_id=iid, credit_id=cid, role="director")
            ok("update_credit", it and any(c["role"] == "director" for c in it["credits"]), (it, e))
            res, e = await call("search_people", q="Jane")
            ok("search_people", res and res["total"] == 1, (res, e))
            p, e = await call("get_person", person_id=pid)
            ok("get_person shows credit", p and len(p["credits"]) == 1, (p, e))
            it, e = await call("add_credit", item_id=iid, person_name="Bob Newname", role="writer")
            ok("add_credit by name", it and any(c["person"]["name"] == "Bob Newname" for c in it["credits"]), (it, e))
            cid = next(c["id"] for c in it["credits"] if c["person"]["id"] == pid)
            it, e = await call("remove_credit", item_id=iid, credit_id=cid)
            ok("remove_credit", it and not any(c["person"]["id"] == pid for c in it["credits"]), (it, e))
            _, e = await call("delete_person", person_id=pid)
            ok("delete_person", e is None, e)

            rel, e = await call("create_relation", type="Collection", name="Smoke Set", members=[{"item_id": iid, "role": "main"}])
            ok("create_relation", rel and rel["type"] == "collection" and rel["member_count"] == 1, (rel, e))
            rid = rel["id"]
            rel, e = await call("update_relation", relation_id=rid, name="Renamed")
            ok("update_relation (no props)", rel and rel["name"] == "Renamed" and "props" not in rel, (rel, e))
            rel, e = await call("get_relation", relation_id=rid)
            ok("get_relation", rel and rel["members"][0]["item_id"] == iid, (rel, e))
            ir, e = await call("get_item_relations", item_id=iid)
            ok("get_item_relations", ir and ir[0]["id"] == rid, (ir, e))
            rt, e = await call("list_relation_types")
            ok("list_relation_types", rt and rt[0]["type"] == "collection", (rt, e))
            lr, e = await call("list_relations", type="collection")
            ok("list_relations", lr and lr["total"] == 1, (lr, e))
            # second item via upload for member ops
            subprocess.run(["ffmpeg", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=160x90:rate=10:duration=1",
                            "-c:v", "libx264", f"{tmp}/c2.mkv"], check=True)
            with open(f"{tmp}/c2.mkv", "rb") as f:
                httpx.post(URL + "/api/ingest/upload", content=f.read(),
                           headers={"X-Filename": urllib.parse.quote("Second Smoke (2021).mkv")}, timeout=60)
            res, _ = await call("list_items", kind="movie")
            iid2 = next(i["id"] for i in res["items"] if i["id"] != iid)
            rel, e = await call("add_relation_member", relation_id=rid, item_id=iid2, role="extra")
            ok("add_relation_member", rel and len(rel["members"]) == 2, (rel, e))
            rel, e = await call("replace_relation_members", relation_id=rid, members=[{"item_id": iid2, "ord": 0}, {"item_id": iid, "ord": 1}])
            ok("replace_relation_members", rel and rel["members"][0]["item_id"] == iid2, (rel, e))
            rel, e = await call("remove_relation_member", relation_id=rid, item_id=iid2)
            ok("remove_relation_member", rel and len(rel["members"]) == 1, (rel, e))
            _, e = await call("delete_relation", relation_id=rid)
            ok("delete_relation", e is None, e)
            _, e = await call("get_relation", relation_id=rid)
            ok("relation gone", e is not None)

            st, e = await call("ingest_status", records=True)
            ok("ingest_status", st and "status" in st and len(st["records"]) >= 1, (st, e))

            # ---- jobs and schedules
            ok("job tools listed", {"list_jobs", "get_job", "create_job", "cancel_job", "pause_job", "resume_job",
                                    "retry_job", "delete_job", "job_stats", "wait_for_job", "list_schedules",
                                    "get_schedule", "update_schedule", "run_schedule"} <= tools)
            sch, e = await call("list_schedules")
            ok("list_schedules", sch and any(x["id"] == "verify" for x in sch), (sch, e))
            sc, e = await call("update_schedule", id="verify", interval_secs=7200)
            ok("update_schedule interval", sc and sc["interval_secs"] == 7200, (sc, e))
            sc, e = await call("get_schedule", id="verify")
            ok("get_schedule reflects update", sc and sc["interval_secs"] == 7200, (sc, e))
            _, e = await call("update_schedule", id="verify", cron="not a cron")
            ok("bad cron is tool error", e is not None and "400" in e, e)
            _, e = await call("update_schedule", id="verify")
            ok("update_schedule no args is tool error", e is not None)
            job, e = await call("run_schedule", id="verify")
            ok("run_schedule", job and job["type"] == "verify" and job["schedule_id"] == "verify", (job, e))
            job, e = await call("wait_for_job", id=job["id"], timeout_s=30)
            ok("scheduled job reaches done", job and job["state"] == "done" and not job.get("timed_out"), (job, e))
            job, e = await call("create_job", type="verify")
            ok("create_job verify", job and job["type"] == "verify", (job, e))
            job, e = await call("wait_for_job", id=job["id"], timeout_s=30)
            ok("created verify job done", job and job["state"] == "done", (job, e))
            g, e = await call("get_job", id=job["id"])
            ok("get_job has logs", g and "logs" in g, (g, e))
            lj, e = await call("list_jobs", state="done", type="verify")
            ok("list_jobs filter", lj and lj["total"] >= 2 and all(j["type"] == "verify" for j in lj["jobs"]), (lj, e))
            future = int(time.time()) + 3600
            qj, e = await call("create_job", type="backup", run_at=future)
            ok("create deferred job", qj and qj["state"] == "queued", (qj, e))
            pj, e = await call("pause_job", id=qj["id"])
            ok("pause_job", pj and pj["state"] == "paused", (pj, e))
            rj, e = await call("resume_job", id=qj["id"])
            ok("resume_job", rj and rj["state"] == "queued", (rj, e))
            cj, e = await call("cancel_job", id=qj["id"])
            ok("cancel_job", cj and cj["state"] == "cancelled", (cj, e))
            _, e = await call("cancel_job", id=qj["id"])
            ok("cancel finished job is 409 tool error", e is not None and "409" in e, e)
            rj, e = await call("retry_job", id=qj["id"])
            ok("retry_job", rj and rj["state"] == "queued", (rj, e))
            await call("cancel_job", id=qj["id"])
            _, e = await call("delete_job", id=qj["id"])
            ok("delete_job", e is None, e)
            _, e = await call("get_job", id=qj["id"])
            ok("deleted job 404", e is not None and "404" in e, e)
            stt, e = await call("job_stats")
            ok("job_stats", stt and "by_state" in stt and "workers" in stt, (stt, e))
            _, e = await call("create_job", type="bogus")
            ok("create_job bad type is tool error", e is not None and "400" in e, e)
    finally:
        if srv:
            srv.terminate()
            try:
                srv.wait(10)
            except Exception:
                srv.kill()
        shutil.rmtree(tmp, ignore_errors=True)
    print(f"{passes} passed, {fails} failed")
    return fails


sys.exit(1 if asyncio.run(main()) else 0)
