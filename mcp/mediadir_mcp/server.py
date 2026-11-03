"""MediaDir MCP server: wraps the MediaDir v3 HTTP API (docs/API.md) as MCP tools.

Deliberately has no tools for deleting items or media files.
"""
from __future__ import annotations

import functools
import json
import os
import re
from typing import Any, Optional
from urllib.parse import quote, urlencode

import httpx
from mcp.server.fastmcp import FastMCP
from mcp.server.fastmcp.exceptions import ToolError

BASE_URL = os.environ.get("MEDIADIR_URL", "http://localhost:8096").rstrip("/")
TIMEOUT = float(os.environ.get("MEDIADIR_TIMEOUT", "30"))

mcp = FastMCP(
    "mediadir",
    instructions=(
        "Read and edit metadata in a MediaDir media server. Ids are opaque 16-char hex strings; "
        "find them with list_libraries/list_items/search. Editing a core field (title, year, ...) "
        "locks it against re-ingest overwrites; use delete_item_field to unlock. "
        "Files and items cannot be deleted through this server."
    ),
)

_client: Optional[httpx.Client] = None


def tool():
    """Register a tool whose result is returned as ONE compact-JSON text block."""
    def deco(fn):
        @functools.wraps(fn)
        def wrapper(*a, **kw) -> str:
            return json.dumps(fn(*a, **kw), separators=(",", ":"), ensure_ascii=False)
        return mcp.tool()(wrapper)
    return deco


def _http() -> httpx.Client:
    global _client
    if _client is None:
        _client = httpx.Client(base_url=BASE_URL, timeout=TIMEOUT)
    return _client


def _call(method: str, path: str, *, params: dict | None = None, json: Any = None,
          content: bytes | None = None, headers: dict | None = None) -> Any:
    params = {k: v for k, v in (params or {}).items() if v is not None}
    try:
        r = _http().request(method, path, params=params, json=json, content=content, headers=headers)
    except httpx.HTTPError as e:
        raise ToolError(f"cannot reach MediaDir at {BASE_URL}: {e}") from e
    if r.status_code >= 400:
        try:
            msg = r.json().get("error") or r.text
        except Exception:
            msg = r.text
        raise ToolError(f"MediaDir API {method} {path} -> {r.status_code}: {msg}")
    if not r.content:
        return {"ok": True, "status": r.status_code}
    try:
        return r.json()
    except ValueError:
        raise ToolError(f"non-JSON response from {path}")


def _enc(s: str) -> str:
    return quote(str(s), safe="")


def _slim_summary(i: dict) -> dict:
    keep = ("id", "kind", "title", "subtitle", "year", "index", "parent_index", "parent_id",
            "library_id", "duration", "child_count")
    return {k: i[k] for k in keep if i.get(k) is not None}


def _slim_file(f: dict) -> dict:
    out = {k: f.get(k) for k in ("id", "path", "label", "container", "size", "duration", "bitrate")}
    v = f.get("video")
    if v:
        out["video"] = {k: v.get(k) for k in ("codec", "width", "height", "fps", "hdr")}
    out["audio"] = [{"codec": a.get("codec"), "channels": a.get("channels"), "language": a.get("language")}
                    for a in f.get("audio") or []]
    out["subtitles"] = len(f.get("subtitles") or [])
    return out


def _is_ref(v: Any) -> bool:
    return isinstance(v, dict) and isinstance(v.get("hash"), str) and set(v) == {"hash"}


def _compact_fields(fields: Any) -> Any:
    """Image field values ({"hash":h} or a list of those) become "img:<hash>" / ["img:<hash>", ...]."""
    if not isinstance(fields, dict):
        return fields
    out = {}
    for k, v in fields.items():
        if _is_ref(v):
            v = "img:" + v["hash"]
        elif isinstance(v, list) and v and all(_is_ref(x) for x in v):
            v = ["img:" + x["hash"] for x in v]
        out[k] = v
    return out


def _slim_item(d: dict, full: bool) -> dict:
    if full:
        return d
    out = {k: v for k, v in d.items() if k not in ("display", "children", "files", "card", "extras")}
    if "fields" in out:
        out["fields"] = _compact_fields(out["fields"])
    out["files"] = [_slim_file(f) for f in d.get("files") or []]
    if d.get("children") is not None:
        out["children"] = [_slim_summary(c) for c in d["children"]]
    return out


def _slim_person(d: dict) -> dict:
    out = {k: v for k, v in d.items() if k not in ("display", "credits")}
    if "fields" in out:
        out["fields"] = _compact_fields(out["fields"])
    out["credits"] = [{"credit_id": c.get("id"), "role": c.get("role"), "character": c.get("character"),
                       "item": _slim_summary(c["item"]) if c.get("item") else None}
                      for c in d.get("credits") or []]
    return out


# ---------------------------------------------------------------- discovery

@tool()
def list_libraries() -> Any:
    """List the virtual libraries (one per item kind with items): id, name, kind, item_count."""
    return _call("GET", "/api/libraries")


@tool()
def list_items(library_id: Optional[str] = None, kind: Optional[str] = None,
               parent_id: Optional[str] = None, person_id: Optional[str] = None,
               q: Optional[str] = None, sort: Optional[str] = None, order: Optional[str] = None,
               limit: int = 50, offset: int = 0) -> Any:
    """List items with filters. Returns {total, items}.

    kind: movie, show, season, episode, artist, album, track, music_video, video.
    parent_id lists children of a container (e.g. episodes of a season). With none of
    parent_id/kind/person_id/q given, only top-level items of the library are returned.
    sort: title | added | year | index; order: asc | desc. limit max 1000.
    """
    r = _call("GET", "/api/items", params=dict(library_id=library_id, kind=kind, parent_id=parent_id,
                                               person_id=person_id, q=q, sort=sort, order=order,
                                               limit=limit, offset=offset))
    return {"total": r["total"], "offset": offset, "items": [_slim_summary(i) for i in r["items"]]}


@tool()
def search(q: str) -> Any:
    """Search items and people by text. Returns {items: [...], people: [...]}."""
    r = _call("GET", "/api/search", params={"q": q})
    return {"items": [_slim_summary(i) for i in r.get("items", [])],
            "people": [{k: p.get(k) for k in ("id", "name", "credit_count")} for p in r.get("people", [])]}


@tool()
def get_item(item_id: str, full: bool = False) -> Any:
    """Get full item detail: core metadata, locked fields, custom fields, credits, relations,
    breadcrumbs, children and a summary of media files (codec, resolution, size).
    Set full=true for the raw API response (includes display blocks and full file probe info)."""
    return _slim_item(_call("GET", f"/api/items/{_enc(item_id)}"), full)


# ---------------------------------------------------------------- item metadata

@tool()
def update_item(item_id: str, title: Optional[str] = None, sort_title: Optional[str] = None,
                subtitle: Optional[str] = None, year: Optional[int] = None,
                overview: Optional[str] = None, genres: Optional[list[str]] = None,
                index: Optional[int] = None, parent_index: Optional[int] = None,
                kind: Optional[str] = None, clear: Optional[list[str]] = None) -> Any:
    """Edit core metadata. Only the arguments you pass are changed; edited fields become locked.

    index = episode/track/season number, parent_index = season/disc number.
    kind can only switch between movie, music_video, video, track on top-level items.
    clear: list of fields to set to null (sort_title, subtitle, year, overview, genres, index, parent_index).
    """
    body: dict[str, Any] = {k: v for k, v in dict(title=title, sort_title=sort_title, subtitle=subtitle,
                                                  year=year, overview=overview, genres=genres,
                                                  index=index, parent_index=parent_index,
                                                  kind=kind).items() if v is not None}
    for k in clear or []:
        body[k] = None
    if not body:
        raise ToolError("nothing to update: pass at least one field")
    return _slim_item(_call("PATCH", f"/api/items/{_enc(item_id)}", json=body), False)


@tool()
def unlock_field(item_id: str, field: str) -> Any:
    """Unlock a core field (title, year, ...) so ingest may overwrite it again. This also drops
    the user-set value, reverting to what the ingest pipeline derived."""
    return _slim_item(_call("DELETE", f"/api/items/{_enc(item_id)}/fields/{_enc(field)}"), False)


@tool()
def set_item_field(item_id: str, key: str, value: Any) -> Any:
    """Set a custom field (or editable builtin such as rating, studio, tagline, content_rating) on an item.

    The field must be defined first (list_field_defs / create_field_def). Value is validated against the
    field type: text/url/date -> string, number/rating/duration -> number, bool -> boolean,
    tags -> list of strings, person -> person id or list of person ids, image -> {"hash": h} or a list of
    those (prefer set_image_field, which uploads for you). null clears it.
    """
    return _slim_item(_call("PUT", f"/api/items/{_enc(item_id)}/fields/{_enc(key)}", json={"value": value}), False)


@tool()
def delete_item_field(item_id: str, key: str) -> Any:
    """Remove a user-set field value from an item (on a core field this also unlocks it)."""
    return _slim_item(_call("DELETE", f"/api/items/{_enc(item_id)}/fields/{_enc(key)}"), False)


# ---------------------------------------------------------------- field definitions

@tool()
def list_field_defs() -> Any:
    """List field definitions (builtin + custom): key, label, type, applies_to, derived, display hints."""
    return _call("GET", "/api/fields")


@tool()
def create_field_def(key: str, label: str, type: str, applies_to: Optional[list[str]] = None,
                     display: Optional[dict] = None) -> Any:
    """Create a custom field definition.

    type: text | number | date | url | tags | rating | bool | duration | person | image.
    applies_to: item kinds (plus "person"); empty/omitted = all.
    display (optional): {placement: card_badge|card_meta|header|info|section|hidden,
      widget: badge|chip|text|rating|link|tags|people|markdown|stat|image (default for image fields, placement hidden), order, format ("{value}"), color "#RRGGBB", icon}.
    """
    body = {"key": key, "label": label, "type": type, "applies_to": applies_to or []}
    if display:
        body["display"] = display
    return _call("POST", "/api/fields", json=body)


@tool()
def update_field_def(key: str, label: Optional[str] = None, applies_to: Optional[list[str]] = None,
                     display: Optional[dict] = None) -> Any:
    """Update a field definition's label, applies_to or display (builtins: label/display only)."""
    body = {k: v for k, v in dict(label=label, applies_to=applies_to, display=display).items() if v is not None}
    return _call("PUT", f"/api/fields/{_enc(key)}", json=body)


@tool()
def delete_field_def(key: str) -> Any:
    """Delete a custom field definition and every value stored under it. Builtins cannot be deleted."""
    return _call("DELETE", f"/api/fields/{_enc(key)}")


# ---------------------------------------------------------------- people & credits

@tool()
def search_people(q: Optional[str] = None, role: Optional[str] = None, limit: int = 50, offset: int = 0) -> Any:
    """List/search people by name and optionally by credit role (actor, director, artist, ...). Returns {total, people}."""
    r = _call("GET", "/api/people", params=dict(q=q, role=role, limit=limit, offset=offset))
    return {"total": r["total"], "people": [{k: p.get(k) for k in ("id", "name", "sort_name", "credit_count")}
                                            for p in r["people"]]}


@tool()
def get_person(person_id: str) -> Any:
    """Get a person: name, sort_name, bio, birth_date, custom fields and their credits (with credit ids)."""
    return _slim_person(_call("GET", f"/api/people/{_enc(person_id)}"))


@tool()
def create_person(name: str, sort_name: Optional[str] = None, bio: Optional[str] = None,
                  birth_date: Optional[str] = None) -> Any:
    """Create a person (409 if the normalized name exists). birth_date is YYYY-MM-DD."""
    body = {k: v for k, v in dict(name=name, sort_name=sort_name, bio=bio, birth_date=birth_date).items() if v is not None}
    return _slim_person(_call("POST", "/api/people", json=body))


@tool()
def update_person(person_id: str, name: Optional[str] = None, sort_name: Optional[str] = None,
                  bio: Optional[str] = None, birth_date: Optional[str] = None,
                  clear: Optional[list[str]] = None) -> Any:
    """Edit a person. Only passed arguments change. clear: list of sort_name/bio/birth_date to null out.
    Person ids do not change when renamed."""
    body: dict[str, Any] = {k: v for k, v in dict(name=name, sort_name=sort_name, bio=bio,
                                                  birth_date=birth_date).items() if v is not None}
    for k in clear or []:
        body[k] = None
    if not body:
        raise ToolError("nothing to update: pass at least one field")
    return _slim_person(_call("PATCH", f"/api/people/{_enc(person_id)}", json=body))


@tool()
def delete_person(person_id: str) -> Any:
    """Delete a person and all their credits (items are untouched)."""
    return _call("DELETE", f"/api/people/{_enc(person_id)}")


@tool()
def set_person_field(person_id: str, key: str, value: Any) -> Any:
    """Set a custom field on a person (definition must exist and apply to "person"). null clears it."""
    return _slim_person(_call("PUT", f"/api/people/{_enc(person_id)}/fields/{_enc(key)}", json={"value": value}))


@tool()
def delete_person_field(person_id: str, key: str) -> Any:
    """Remove a custom field value from a person."""
    return _slim_person(_call("DELETE", f"/api/people/{_enc(person_id)}/fields/{_enc(key)}"))


@tool()
def add_credit(item_id: str, role: str, person_id: Optional[str] = None, person_name: Optional[str] = None,
               character: Optional[str] = None, order: Optional[int] = None) -> Any:
    """Add a credit (person in a role) to an item. Give person_id, or person_name to find/create by name.

    role (lowercase, open set): actor, director, writer, producer, artist, album_artist, featured,
    singer, composer, lyricist, dancer, guest, creator. Returns the updated item (see credits[].id).
    """
    body = {k: v for k, v in dict(role=role, person_id=person_id, person_name=person_name,
                                  character=character, order=order).items() if v is not None}
    return _slim_item(_call("POST", f"/api/items/{_enc(item_id)}/credits", json=body), False)


@tool()
def remove_credit(item_id: str, credit_id: str) -> Any:
    """Remove a credit from an item by credit id (from get_item credits[].id)."""
    return _slim_item(_call("DELETE", f"/api/items/{_enc(item_id)}/credits/{_enc(credit_id)}"), False)


@tool()
def update_credit(item_id: str, credit_id: str, role: Optional[str] = None,
                  character: Optional[str] = None, order: Optional[int] = None) -> Any:
    """Change a credit's role/character/order. The API has no credit PATCH, so this reads the credit,
    removes it and re-adds it for the same person with the changed values (the credit id changes)."""
    item = _call("GET", f"/api/items/{_enc(item_id)}")
    cur = next((c for c in item.get("credits", []) if c["id"] == credit_id), None)
    if cur is None:
        raise ToolError(f"credit {credit_id} not found on item {item_id}")
    _call("DELETE", f"/api/items/{_enc(item_id)}/credits/{_enc(credit_id)}")
    body = {"person_id": cur["person"]["id"], "role": role or cur["role"],
            "character": character if character is not None else cur.get("character"),
            "order": order if order is not None else cur.get("order")}
    body = {k: v for k, v in body.items() if v is not None}
    return _slim_item(_call("POST", f"/api/items/{_enc(item_id)}/credits", json=body), False)


# ---------------------------------------------------------------- relations

def _members(members: list[dict]) -> list[dict]:
    return [{k: v for k, v in m.items() if k in ("item_id", "ord", "role") and v is not None} for m in members]


def _slim_relation(r: Any) -> Any:
    if not isinstance(r, dict) or "members" not in r:
        return r
    out = {k: v for k, v in r.items() if k != "members"}
    out["members"] = [{"item_id": m["item_id"], "ord": m.get("ord"), "role": m.get("role"),
                       "title": (m.get("item") or {}).get("title"), "kind": (m.get("item") or {}).get("kind")}
                      for m in r["members"]]
    return out


@tool()
def list_relation_types() -> Any:
    """List relation types in use with counts: [{type, count}]."""
    return _call("GET", "/api/relation-types")


@tool()
def list_relations(type: Optional[str] = None, q: Optional[str] = None, limit: int = 50, offset: int = 0) -> Any:
    """List relations (named groups of items such as collections or albums). Returns {total, relations}."""
    return _call("GET", "/api/relations", params=dict(type=type, q=q, limit=limit, offset=offset))


@tool()
def get_relation(relation_id: str) -> Any:
    """Get a relation with its ordered members (item_id, ord, role, title, kind)."""
    return _slim_relation(_call("GET", f"/api/relations/{_enc(relation_id)}"))


@tool()
def get_item_relations(item_id: str) -> Any:
    """List every relation an item belongs to, with the item's ord and role in each."""
    return _call("GET", f"/api/items/{_enc(item_id)}/relations")


@tool()
def create_relation(type: str, members: list[dict], name: Optional[str] = None) -> Any:
    """Create a relation. type is free-form (collection, album, soundtrack_of, ...; normalized to
    lowercase_snake). members: at least one {item_id, ord?, role?}; unique existing items."""
    body: dict[str, Any] = {"type": type, "members": _members(members)}
    if name is not None:
        body["name"] = name
    return _slim_relation(_call("POST", "/api/relations", json=body))


@tool()
def update_relation(relation_id: str, name: Optional[str] = None, type: Optional[str] = None) -> Any:
    """Change a relation's name or type (relations have no props)."""
    body = {k: v for k, v in dict(name=name, type=type).items() if v is not None}
    if not body:
        raise ToolError("nothing to update")
    return _slim_relation(_call("PATCH", f"/api/relations/{_enc(relation_id)}", json=body))


@tool()
def delete_relation(relation_id: str) -> Any:
    """Delete a relation. Member items are untouched."""
    return _call("DELETE", f"/api/relations/{_enc(relation_id)}")


@tool()
def add_relation_member(relation_id: str, item_id: str, ord: Optional[int] = None, role: Optional[str] = None) -> Any:
    """Add an item to a relation, or update its ord/role if already a member. Missing ord/role keep
    the current value (new members append); role "" clears the role."""
    body = {k: v for k, v in dict(item_id=item_id, ord=ord, role=role).items() if v is not None}
    return _slim_relation(_call("POST", f"/api/relations/{_enc(relation_id)}/members", json=body))


@tool()
def remove_relation_member(relation_id: str, item_id: str) -> Any:
    """Remove an item from a relation. Removing the last member deletes the relation."""
    return _slim_relation(_call("DELETE", f"/api/relations/{_enc(relation_id)}/members/{_enc(item_id)}"))


@tool()
def replace_relation_members(relation_id: str, members: list[dict]) -> Any:
    """Replace the whole member list with [{item_id, ord?, role?}]. An empty list deletes the relation."""
    return _slim_relation(_call("PUT", f"/api/relations/{_enc(relation_id)}/members",
                                json={"members": _members(members)}))


# ---------------------------------------------------------------- image assets

_HASH_RE = re.compile(r"^[0-9a-fA-F]{64}$")


def _upload_bytes(data: bytes) -> dict:
    return _call("POST", "/api/assets", content=data, headers={"Content-Type": "application/octet-stream"})


def _upload_source(source: str) -> dict:
    if source.startswith(("http://", "https://")):
        try:
            resp = httpx.get(source, timeout=TIMEOUT, follow_redirects=True)
            resp.raise_for_status()
        except httpx.HTTPError as e:
            raise ToolError(f"could not download image: {e}") from e
        data = resp.content
    else:
        try:
            with open(os.path.expanduser(source), "rb") as f:
                data = f.read()
        except OSError as e:
            raise ToolError(f"could not read image file: {e}") from e
    return _upload_bytes(data)


def _asset_url(hash: str, w=None, h=None, fit=None, fmt=None, q=None) -> str:
    qs = urlencode({k: v for k, v in dict(w=w, h=h, fit=fit, fmt=fmt, q=q).items() if v is not None})
    return f"{BASE_URL}/asset/{_enc(hash.lower())}" + (f"?{qs}" if qs else "")


@tool()
def upload_asset(path_or_url: str) -> Any:
    """Upload an image (JPEG/PNG/WebP/GIF, max 100 MB) as a content-addressed asset. path_or_url is a
    local file path (on the machine running this MCP server) or an http(s) URL the MCP server downloads.
    Returns {hash, mime, width, height, size}. Identical bytes always give the same hash (deduped).
    Unreferenced assets are garbage-collected ~24 h after upload, so attach it to an image field promptly
    (set_image_field does upload + attach in one step)."""
    return _upload_source(path_or_url)


@tool()
def get_asset_info(hash: str) -> Any:
    """Check an asset exists and describe it: {hash, url, mime, size}. Only response headers are read
    (no bytes downloaded). 404 is a tool error. Pixel dimensions are only returned by upload_asset."""
    if not _HASH_RE.match(hash):
        raise ToolError("hash must be 64 hex characters")
    url = _asset_url(hash)
    try:
        with _http().stream("GET", f"/asset/{hash.lower()}", params={"original": 1}) as r:
            if r.status_code >= 400:
                raise ToolError(f"MediaDir API GET /asset/{hash} -> {r.status_code}")
            info = {"hash": hash.lower(), "url": url, "mime": r.headers.get("content-type"),
                    "size": int(r.headers["content-length"]) if "content-length" in r.headers else None}
    except httpx.HTTPError as e:
        raise ToolError(f"cannot reach MediaDir at {BASE_URL}: {e}") from e
    return info


@tool()
def asset_url(hash: str, w: Optional[int] = None, h: Optional[int] = None, fit: Optional[str] = None,
              fmt: Optional[str] = None, q: Optional[int] = None) -> Any:
    """Build the display URL of an asset (no bytes are fetched). w/h: target pixels (aspect kept if one
    given, never upscaled); fit: cover (default) | contain; fmt: webp (default) | jpg | png; q: 1-100.
    With no options the URL serves the original bytes. Returns {url}."""
    if not _HASH_RE.match(hash):
        raise ToolError("hash must be 64 hex characters")
    return {"url": _asset_url(hash, w, h, fit, fmt, q)}


@tool()
def set_image_field(owner: str, id: str, key: str, sources: list[str], append: bool = False) -> Any:
    """Upload images as needed and set an `image` field on an item or person.

    owner: "item" or "person"; id: its id; key: an image field (create_field_def type "image" first).
    sources: local file paths, http(s) URLs, or existing 64-hex asset hashes.
    Value rule: the result is ONE ref {"hash"} when exactly one image ends up in the field AND the
    field's current value is not a list; otherwise it is an ordered list of refs. append=false replaces the
    value with the sources; append=true appends them (duplicates by hash skipped) to the current images.
    To force a list from a single image, set a list first (e.g. via set_item_field) or pass 2+ sources.
    Returns the updated item/person (image values shown as "img:<hash>")."""
    if owner not in ("item", "person"):
        raise ToolError('owner must be "item" or "person"')
    if not sources:
        raise ToolError("sources must not be empty")
    base = f"/api/{'items' if owner == 'item' else 'people'}/{_enc(id)}"
    cur = _call("GET", base).get("fields", {}).get(key)
    hashes: list[str] = []
    if append and cur:
        hashes += [x["hash"] for x in (cur if isinstance(cur, list) else [cur]) if _is_ref(x)]
    for src in sources:
        h = src.lower() if _HASH_RE.match(src) else _upload_source(src)["hash"]
        if h not in hashes:
            hashes.append(h)
    refs = [{"hash": h} for h in hashes]
    value: Any = refs[0] if len(refs) == 1 and not isinstance(cur, list) else refs
    d = _call("PUT", f"{base}/fields/{_enc(key)}", json={"value": value})
    return _slim_item(d, False) if owner == "item" else _slim_person(d)


# ---------------------------------------------------------------- ingest (read-only)

@tool()
def ingest_status(records: bool = False, state: Optional[str] = None, limit: int = 20) -> Any:
    """Ingest watcher/queue/maintenance status and storage problems. With records=true also include
    recent ingest records (optionally filtered by state: waiting, hashing, placing, probing, done,
    duplicate, failed, ignored)."""
    out = {"status": _call("GET", "/api/ingest/status")}
    if records:
        out["records"] = _call("GET", "/api/ingest", params={"state": state, "limit": limit})
    return out


def main() -> None:
    mcp.run(transport="stdio")


if __name__ == "__main__":
    main()
