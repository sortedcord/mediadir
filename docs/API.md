# MediaDir — API contract (v2 shapes, v3 storage)

Shared contract between the Rust server (`server/`) and the Android app (`android/`).
**No transcoding, ever.** The server indexes metadata and serves original bytes (HTTP Range).
How media gets in (ingest dir, uploads, the content-addressed store, file-name mapping,
virtual libraries, deletes, durability) is specified in **[STORAGE.md](STORAGE.md)** (v3).
The response shapes below are unchanged from v2 except where marked *v3*.
All ids are opaque strings (16 hex chars, stable hashes). All image/stream URLs in responses are
**server-relative paths** (e.g. `/stream/ab12…`, `/asset/<hash>`); clients prefix the server base URL.
Unknown JSON fields must be ignored by clients; optional fields may be absent or `null`.

## Concepts

| Concept | Description |
|---|---|
| **Library** | *v3:* virtual, one per item kind that has items (see [STORAGE.md](STORAGE.md#libraries)). |
| **Item** | Any browsable/playable node. Hierarchical via `parent_id`. |
| **MediaFile** | A physical file attached to an item. An item may have several (versions / quality variants). |
| **Person** | Anyone featured in media (actor, artist, director…). Linked to items via **Credits**. |
| **FieldDefinition** | Schema for a metadata field (builtin or custom) **including how the frontend displays it**. |
| **DisplayBlock** | A server-resolved, ready-to-render field value + its display hints. Frontends render these generically. |

### Library kinds → item kinds
| library `kind` | hierarchy |
|---|---|
| `movies` | `movie` |
| `shows` | `show` → `season` → `episode` |
| `music` | `artist` → `track` (`album` exists as a kind but is not created from file names) |
| `music_videos` | `music_video` |
| `videos` | `video` (generic / home videos) |

`playable` item kinds: `movie`, `episode`, `track`, `music_video`, `video`. Containers (`show`, `season`, `artist`, `album`) are not directly playable but `/playback` builds a queue for them.

`aspect` (image shape hint for cards): `poster` (2:3: movie, show, season), `landscape` (16:9: episode, music_video, video), `square` (artist, album, track).

## Objects

### Library
```json
{ "id": "…", "name": "Music Videos", "kind": "music_videos", "path": null, "item_count": 5, "scanning": false }
```
*v3:* `path` is always null, `scanning` always false, ids are stable per kind.

### ItemSummary
```json
{
  "id": "…", "kind": "music_video", "library_id": "…", "parent_id": null,
  "title": "Kusu Kusu", "subtitle": "Nora Fatehi · Satyameva Jayate 2", "year": 2021,
  "index": null, "parent_index": null,
  "duration": 178.4, "playable": true, "child_count": null,
  "aspect": "landscape",
  "added_at": 1793433022,
  "card": [ DisplayBlock, … ],
  "images": { "<field_key>": {"hash": "…"} | [ {"hash": "…"}, … ] }
}
```
`images` (*v3*) = the raw stored values of ALL `image`-type fields of the item, regardless of placement (image
fields default to `hidden`, so they are not in `card`). Omitted when the item has no image values. The server
never chooses which image to show; clients pick a key (and fall back to parents) themselves. Present on every
ItemSummary (lists, children, search, relation members, credits, playback, ...).
`index` = episode/track/season number; `parent_index` = season/disc number.
`card` = display blocks whose placement is `card_badge` or `card_meta`, already sorted by `order`.

### ItemDetail = ItemSummary + 
```json
{
  "overview": "…", "sort_title": "…", "genres": ["Dance"], "locked": ["title"],
  "breadcrumbs": [ { "id": "…", "kind": "show", "title": "…" } ],
  "files": [ MediaFile ],
  "credits": [ Credit ],
  "children": [ ItemSummary ],
  "fields": { "<key>": <raw json value>, … },
  "display": [ DisplayBlock, … ]
}
```
`children` sorted naturally (seasons/episodes/tracks by index, albums by year).
`fields` = raw values of all custom + editable builtin fields set on the item (image fields hold `{"hash"}` refs, see [Images](#images-v3)).
`display` = all non-hidden blocks for the detail page (placements `header`, `info`, `section`, plus card placements).
`locked` = core field names manually edited.

### MediaFile
```json
{
  "id": "…", "path": "Original File Name.mkv", "label": "4320p · AV1", "original": true,
  "hash": "<blake3 hex>", "location": "store", "hash_state": "full", "original_name": "Original File Name.mkv", "added_at": 1793433022,
  "container": "mkv", "mime": "video/x-matroska", "size": 551123344, "duration": 178.4, "bitrate": 24600000,
  "video": { "codec": "av1", "profile": "Main", "width": 7680, "height": 4320, "fps": 29.97, "hdr": null, "bit_depth": 8 },
  "audio": [ { "index": 1, "codec": "opus", "channels": 2, "language": "eng", "title": null } ],
  "subtitles": [ { "index": 2, "codec": "subrip", "language": "eng", "title": null, "forced": false, "external": null } ],
  "stream_url": "/stream/…"
}
```
`video` is null for audio files. Files are ordered by height desc (then oldest first).
*v3:* `path` is the file name the media arrived with (the stored blob's location is private);
`hash` (BLAKE3), `original_name`, `added_at` are new. Every file is `original: true`; subtitles are
the embedded streams only (`external` is always null; there is no `/subtitle` endpoint).
*Symlinked media* (see [STORAGE.md](STORAGE.md#symlinked-media)): `location` is `"store"` (the store entry is a
regular file MediaDir holds) or `"external"` (the store entry is a symlink to media kept elsewhere; streaming/frames
work the same).
`hash_state` is `"full"` (`hash` = BLAKE3 hex) or `"pending"` (only external files: `hash` is `fp:<64 hex>`, a quick
fingerprint, until the background task has the full hash; clients must not treat it as a BLAKE3). Both fields are
always present.

### Person / Credit
```json
// Person (summary)
{ "id": "…", "name": "Nora Fatehi", "sort_name": "Fatehi, Nora", "credit_count": 4,
  "images": { "<field_key>": {"hash": "…"} | [ {"hash": "…"}, … ] } }   // images omitted when none
// PersonDetail = Person + 
{ "bio": "…", "birth_date": "1992-02-06", "fields": {…}, "display": [DisplayBlock],
  "credits": [ { "id": "<credit id>", "role": "actor", "character": null, "item": ItemSummary } ] }
// Credit (on ItemDetail); `person` is a PersonRef `{id, name, images?}` (images as in Person)
{ "id": "…", "role": "actor", "character": "Herself", "order": 0,
  "person": { "id": "…", "name": "…" } }
```
`role` (open set, lowercase): `actor`, `director`, `writer`, `producer`, `artist`, `album_artist`, `featured`, `singer`, `composer`, `lyricist`, `dancer`, `guest`, `creator`.

### FieldDefinition
```json
{
  "key": "fps", "label": "Frame rate", "type": "number", "builtin": true, "derived": true,
  "applies_to": ["movie","episode","music_video","video"],
  "display": { "placement": "card_badge", "widget": "badge", "order": 20,
               "format": "{value}fps", "color": "#FF4D6D", "icon": null }
}
```
- `type`: `text | number | date | url | tags | rating | bool | duration | person | image`
  (`person` values are person ids, or arrays of person ids — rendered as tappable people;
  `image` values are asset refs, see [Images](#images-v3)).
- `derived`: computed by the server (from probing/structure); values not editable, display still configurable.
- `applies_to`: item kinds; `[]` = all kinds.
- `display.placement`: `card_badge | card_meta | header | info | section | hidden`
  - `card_badge` small pill over the card image · `card_meta` small text line under card title
  - `header` chips next to the title on detail · `info` key/value table row · `section` full-width block with heading (long text, tag clouds, people rows)
- `display.widget`: `badge | chip | text | rating | link | tags | people | markdown | stat | image`
- `display.format`: template, `{value}` substituted (for numbers/text). Optional.
- `display.color`: `#RRGGBB` or null (client default). `icon`: Material icon name hint (e.g. `star`, `hd`, `language`), client may ignore unknown.

Builtin field keys (server must provide these definitions; all `builtin:true`):
`resolution` (derived; "8K"/"4K"/"1080p"… card_badge), `fps` (derived; card_badge), `hdr` (derived; card_badge),
`video_codec`, `audio_codec`, `container`, `size`, `bitrate` (derived; card_meta / info),
`duration` (derived; info), `year` (header chip), `genres` (tags, header), `rating` (rating, header),
`content_rating` (chip), `studio` (info), `album` / `track_number` (info, for tracks), `overview` (markdown, section),
`tagline` (text, section), `language` (info).
Custom fields are created by users with any key not in this list.

### DisplayBlock
```json
{ "key": "fps", "label": "Frame rate", "type": "number", "widget": "badge", "placement": "card_badge",
  "order": 20, "color": "#FF4D6D", "icon": null,
  "text": "30fps",                    // formatted, ready to show
  "value": 29.97,                     // raw
  "people": [ { "id": "…", "name": "…" } ]   // only for widget=people / type=person
}
```
Blocks with empty/null values are omitted. A block of an `image` field has `text: ""` and the raw ref(s) in `value`; new image fields default to placement `hidden`, so they only appear in `fields`. Clients render purely from these; they must not hardcode which fields exist.

## Endpoints
All JSON. Errors: non-2xx with `{ "error": "message" }`.

| Method | Path | Notes |
|---|---|---|
| GET | `/api/health` | `{ "ok": true, "version": 3 }` |
| GET | `/api/libraries` | `[Library]` (virtual). `POST/PATCH/DELETE /api/libraries…` → `405`; `/api/scan` and `/api/libraries/{id}/scan` are gone (`404`). See [STORAGE.md](STORAGE.md). |
| GET | `/api/libraries/{id}` | one virtual library |
| GET | `/api/items` | query: `library_id, kind, parent_id, person_id, q, sort=(title\|added\|year\|index), order=(asc\|desc), limit=50, offset=0, top=true`. `top=true` (default when `parent_id` absent) returns only top-level items of the library. → `{ "total": n, "items": [ItemSummary] }` |
| GET | `/api/items/{id}` | ItemDetail |
| PATCH | `/api/items/{id}` | `{title?, sort_title?, subtitle?, year?, overview?, genres?, index?, parent_index?, kind?}` → ItemDetail; edited keys added to `locked`. *v3:* `kind` can change between `movie`, `music_video`, `video`, `track` on top-level items (else `400`). |
| DELETE | `/api/items/{id}` | *v3:* item + children + every stored file they referenced → `{ok, items, files}` (counts). Empty parent containers are removed too. |
| DELETE | `/api/files/{id}` | *v3:* one version; the item goes too when it was the last → `{ok, items, files}` |
| PUT | `/api/items/{id}/fields/{key}` | body `{ "value": <json> }` (custom or editable builtin e.g. rating, studio) → ItemDetail |
| DELETE | `/api/items/{id}/fields/{key}` | → ItemDetail |
| POST | `/api/items/{id}/credits` | `{person_id? , person_name?, role, character?, order?}` (creates person by name if needed) → ItemDetail |
| DELETE | `/api/items/{id}/credits/{credit_id}` | → ItemDetail |
| GET | `/api/items/{id}/playback` | `{ "start_index": n, "queue": [ { "item": ItemSummary, "files": [MediaFile] } ] }`. Playable item → itself plus following siblings (episodes/tracks), start_index 0 points at it; container → all playable descendants in natural order. |
| GET | `/api/people` | `q, role, limit, offset` → `{total, people:[Person]}` |
| POST | `/api/people` | `{name, sort_name?, bio?, birth_date?}` → PersonDetail |
| GET | `/api/people/{id}` | PersonDetail |
| PATCH | `/api/people/{id}` | `{name?, sort_name?, bio?, birth_date?}` |
| PUT | `/api/people/{id}/fields/{key}` / DELETE | like items |
| DELETE | `/api/people/{id}` | removes person + credits |
| GET | `/api/fields` | `[FieldDefinition]` (builtin + custom, sorted by order) |
| POST | `/api/fields` | custom def `{key, label, type, applies_to, display}` |
| DELETE | `/api/fields/{key}` | custom: deletes the field and its values → `{ok:true}`. Builtin: resets the label/display/applies_to override to defaults → the default FieldDefinition |
| PUT | `/api/fields/{key}` | update `label, applies_to, display` (builtins too: the override is stored and `GET /api/fields` returns the effective `label`/`display`/`applies_to`; `[]` = all kinds, same as custom fields). Values already stored on items of a kind that is now excluded are kept but no longer presented; clients should not offer the field for that kind |
| GET | `/api/search` | `q` → `{ "items": [ItemSummary], "people": [Person] }` |
| GET/HEAD | `/stream/{file_id}` | original bytes, Range/206 |
| POST | `/api/assets` | *v3:* upload an image, raw body → `{hash, mime, width, height, size}` (see [Images](#images-v3)) |
| GET | `/asset/{hash}` | *v3:* original or transformed image, `w,h,fit,fmt,q,original` |
| GET | `/api/ingest` · `/api/ingest/{id}` | *v3:* ingest records ([STORAGE.md](STORAGE.md)) |
| GET | `/api/ingest/status` | *v3:* watcher/queue/task status, plus `pending_hashes` (symlinked files still awaiting their full hash). `problems[].problem` is `missing`, `corrupt` or `changed` |
| POST | `/api/ingest/scan` | *v3:* force a poll of the ingest dir, 202 |
| POST | `/api/ingest/{id}/retry` | *v3:* retry one `failed` ingest-dir record: forgets its seen entry and re-queues the entry still in `INGEST_DIR`. `202 {ok, id, retrying}`; `400` if the record is not `failed` or is an upload; `404` if the entry is no longer in `INGEST_DIR` |
| POST | `/api/ingest/retry-failed` | *v3:* same for every failed record whose entry is still in `INGEST_DIR` → `202 {ok, retried, skipped}` (`skipped` = entry gone / upload) |
| POST | `/api/ingest/upload` | *v3:* raw body + `X-Filename` |
| GET | `/api/relations` | *v3:* `type, q, limit=50, offset=0` → `{total, relations:[RelationSummary]}` |
| GET | `/api/relation-types` | `[{type, count}]` for types in use |
| POST | `/api/relations` | `{type, name?, members:[{item_id, ord?, role?}]}` → `201` RelationDetail. `type` is normalized (trim, lowercase, spaces → `_`, `[a-z0-9_]`, else `400`); at least one member, unique, existing items (`404` for unknown items) |
| GET | `/api/relations/{id}` | RelationDetail |
| PATCH | `/api/relations/{id}` | `{name?, type?}` → RelationDetail |
| DELETE | `/api/relations/{id}` | `204`; member items are untouched |
| PUT | `/api/relations/{id}/members` | `{members:[…]}` replaces the member list → RelationDetail; an empty list deletes the relation (`204`) |
| POST | `/api/relations/{id}/members` | `{item_id, ord?, role?}` add or update one member → RelationDetail. A missing `ord`/`role` keeps the current value (new members append); `role: ""` clears it |
| DELETE | `/api/relations/{id}/members/{item_id}` | → RelationDetail, or `204` when that was the last member and the relation was removed |
| GET | `/api/items/{id}/relations` | `[RelationSummary + ord, role]` for every relation the item is in |

## Images (v3)
There are no image slots. The server has no notion of poster/thumb/backdrop and never picks or falls back
between images; **clients decide** which image field is shown where. Images are values of fields of type `image`.
Full details (storage, cache, GC): [STORAGE.md](STORAGE.md#image-assets).

1. **Upload**: `POST /api/assets` with the raw bytes as body (JPEG/PNG/WebP/GIF, max 100 MB).
   ```json
   // 201 (new) or 200 (identical bytes already stored)
   { "hash": "<64-char lowercase hex BLAKE3>", "mime": "image/jpeg", "width": 2000, "height": 3000, "size": 812345 }
   ```
   Errors: `400` not an image, `413` too large.
2. **Define a field** (once, like any custom field; nothing is seeded): `POST /api/fields`
   `{"key":"poster","label":"Poster","type":"image","applies_to":["movie","show"]}` (`applies_to: ["person"]` for people,
   `[]` for all). Default display is `placement: hidden, widget: image`.
3. **Set the value**: `PUT /api/items/{id}/fields/{key}` or `PUT /api/people/{id}/fields/{key}` with
   `{"value": {"hash": "<hex>"}}` (one image) or `{"value": [{"hash": "<hex>"}, {"hash": "<hex>"}]}` (ordered list,
   e.g. a gallery or several candidates). Every hash must be an uploaded asset, else `400`. `{"value": null}` or
   `DELETE …/fields/{key}` clears it. The value comes back unchanged (hashes lower-cased) in `fields.<key>` of
   `ItemDetail` / `PersonDetail`.
4. **Display**: `GET /asset/{hash}` plus optional params; the client builds the URL from the hash.

| `GET /asset/{hash}` query | meaning |
|---|---|
| *(none)*, `original=1` | original bytes, original mime. Unknown params (e.g. `v=`) are ignored. |
| `w`, `h` | target size in pixels; aspect kept when only one is given. Not restricted, but clamped to the original (never upscaled). |
| `fit` | `cover` (default; fill `w`×`h`, center-crop) or `contain` (fit inside `w`×`h`). Only matters when both `w` and `h` are given. |
| `fmt` | `webp` (default), `jpg`, `png` |
| `q` | 1–100, default 80 (ignored for png) |
| any of `w h fit fmt q` without `w`/`h` | width = server `ASSET_DEFAULT_WIDTH` (default 1280), clamped to the original |

Responses: `Cache-Control: public, max-age=31536000, immutable`, `ETag`, `304` on `If-None-Match`; variants also
`X-Asset-Width` / `X-Asset-Height` (actual output size) and `X-Asset-Cache: hit|miss`. HTTP Range is not supported
for assets. Assets nothing references are deleted by the server about 24 h after their last upload, so attach
uploaded images promptly (re-uploading identical bytes is always safe and returns the existing asset).

`ItemSummary.images` / `PersonSummary.images` / `PersonRef.images` are the *new* raw image-field maps (see ItemSummary); the old slot-based `ItemSummary.images` was removed. Also removed in this version: `GET /image/item/{id}/{slot}`, `Person.image`, `GET /image/person/{id}`,
`PUT /api/people/{id}/image`, `Relation.props`, and the thumbnail generated at ingest.

## Relations (v3)
A relation is a named group of items of a free-form `type` (`collection`, `album`, `soundtrack_of`, …). It is not an item: no files, never listed in libraries or search results, and no container items are created for it. An item can belong to any number of relations; a member may have an optional `role` (e.g. `from`/`to` for a directed link) and an `ord` for ordering. Hierarchy (`parent_id`) is unrelated. Deleting an item removes its memberships, and a relation left with no members is deleted automatically (also when its last member is removed through the API).

- **RelationSummary**: `{id, type, name, member_count, created_at, updated_at}`. Relations are pure links: a type, an optional name (`label` is accepted as an alias on input), and ordered members with an optional role. There are no free-form props (a `props` key in a request body is ignored).
- **RelationDetail** = RelationSummary + `members: [{item_id, ord, role, item: ItemSummary}]`, ordered by `ord` then title.
- **ItemDetail** has `relations: [RelationSummary + ord, role]` (this item's position).

## Analytics
Data endpoints only (no UI). Storage, retention and env vars: [STORAGE.md](STORAGE.md#analytics-storage).
All `GET`, JSON, timestamps are unix **seconds** (UTC) in requests and responses. Errors as elsewhere (`{ "error" }`, `400`/`404`).

### Identity
The server has no auth. Clients identify themselves with `X-MediaDir-Device: <stable per-install id>` and
optionally `X-MediaDir-User: <display name>` (each max 128 chars). Media players that cannot set headers may
use `?device=` / `?user=` on the URL instead (the header wins if both are present). Android's ExoPlayer
(`DefaultHttpDataSource.Factory().setDefaultRequestProperties(...)`) can set headers; both forms work.
Missing device -> `anon:<client ip>`. The latest `User-Agent` and client IP are remembered per device.

### What is recorded
- **Library snapshots**: at startup and every `ANALYTICS_SNAPSHOT_SECS`.
- **Requests** (everything except `/api/analytics/*`): `ts`, `group` (`stream|asset|frame|api|other`), `route` (template such
  as `/api/items/{id}`, `/stream/{file_id}`; unrouted = `(unmatched)`), method, status, **response bytes actually sent**
  (Range partials and client aborts counted as streamed), duration in ms (until the body finished or the client went away),
  device, user, `item_id` / `file_id` when the route has one.
- **Playback sessions**, keyed by (device, file): *inferred* from `GET /stream/{file_id}` (200/206): opens on the first
  request, extends while requests continue (an open connection keeps it alive), closes after `ANALYTICS_PLAYBACK_IDLE_SECS`
  without activity. `bytes` = bytes sent; `watched_s` = union of served byte ranges x duration / size; `max_position_s` = furthest
  served byte x duration / size (short reads in the last 10% of the file, container index probes, are ignored for position/watch time).
  *Explicit* events (`POST /api/analytics/playback`) take precedence for the session they touch: `watched_s`, `position_s` and
  `max_position_s` then come from the reported positions (`inferred_watched_s` keeps the byte-based estimate).
- **Device sessions**, keyed by device: open at the first request, close after `ANALYTICS_DEVICE_IDLE_SECS` of inactivity.

### Endpoints
| Method | Path | Notes |
|---|---|---|
| GET | `/api/analytics/overview` | current totals |
| GET | `/api/analytics/library` | `from, to, bucket=hour\|day\|week\|month (default day)`; default range: last 30 days |
| GET | `/api/analytics/traffic` | `from, to, bucket (default hour), group_by=route\|group\|device\|user\|status\|none (default none)`; default range: last 7 days |
| GET | `/api/analytics/sessions` | `type=playback\|device (default playback), active=true\|false, device, user, from, to, limit=50 (max 500), offset` |
| GET | `/api/analytics/sessions/{id}` | one session (`pb_…` playback, `dv_…` device); playback adds `ranges` |
| GET | `/api/analytics/now-playing` | active playback sessions |
| GET | `/api/analytics/top` | `metric=plays\|bytes\|watch_time (default plays), by=item\|user\|device\|kind (default item), from, to, limit=10 (max 200)`; default range: last 30 days |
| GET | `/api/analytics/users` · `/api/analytics/devices` | everything seen so far |
| POST | `/api/analytics/playback` | explicit playback event |

Buckets are UTC; weeks start Monday; months are calendar months. `from`/`to` are `[from, to)`; `from >= to` -> `400`.

**`GET /overview`**
```json
{ "ts": 1700000000,
  "library": { …a snapshot, see below, computed live… },
  "traffic": { "last_24h": { "requests": 10, "bytes": 123, "by_group": { "stream": { "requests": 2, "bytes": 100 }, "api": { … } } },
               "last_7d": { … }, "last_30d": { … } },
  "playback": { "plays_24h": 3, "watch_time_s_24h": 5400.5 },
  "active": { "playback_sessions": 1, "device_sessions": 2 },
  "devices": 4, "users": 3, "last_snapshot_at": 1699999000, "dropped_events": 0 }
```

**Snapshot** (`overview.library`, and each `library.series[]` point, which also has `t` = bucket start and `ts` = snapshot time):
```json
{ "items": { "total": 120, "by_kind": { "movie": 40, "episode": 60 } },
  "files": { "count": 70, "bytes": 123456789,
             "by_container": { "matroska": { "count": 3, "bytes": 99 } }, "by_mime": { "video/mp4": { "count": 2, "bytes": 9 } },
             "by_location": { "store": { "count": 60, "bytes": 1 }, "external": { … } },
             "by_hash_state": { "full": { "count": 69, "bytes": 1 }, "pending": { … } } },
  "assets": { "count": 10, "bytes": 5000 }, "people": 30,
  "relations": { "count": 4, "members": 12 }, "field_defs": 14,
  "ingest": { "queue": 0, "settling": 0, "active": 0, "problems": 0, "pending_hashes": 0, "records_by_state": { "done": 70 } },
  "disks": { "data": { "path": "…", "total_bytes": 1, "free_bytes": 1, "avail_bytes": 1, "used_bytes": 1 }, "media": { … }, "assets": { … } } }
```

**`GET /library`** -> `{ "from", "to", "bucket", "series": [ Snapshot + {t, ts} ] }` ordered by `t`; the last snapshot of each bucket.

**`GET /traffic`**
```json
{ "from": 1, "to": 2, "bucket": "hour", "group_by": "route", "totals": { "requests": 12, "bytes": 3400 },
  "rows": [ { "t": 1700000000, "key": { "group": "stream", "route": "/stream/{file_id}" },
              "requests": 3, "bytes": 3000, "avg_ms": 12.5, "p50_ms": 10, "p95_ms": 30, "source": "raw" } ] }
```
`key` is `null` for `none`, else an object: `route` -> `{group, route}`, `group` -> `{group}`, `device` -> `{device}`, `user` -> `{user}`
(`null` = no user), `status` -> `{status: "2xx"}`. Rows are sorted by `t`, then key. `source` is `raw` (from the request log; percentiles
are nearest-rank over durations), `aggregate` (older than `ANALYTICS_RAW_DAYS`: hour-granular, `p50_ms`/`p95_ms` are `null`) or `mixed`
(`p50_ms`/`p95_ms` `null`).

**Sessions** (`GET /sessions` -> `{ "type", "total", "limit", "offset", "sessions": [ … ] }`, newest first; `/sessions/{id}` -> one object):
```json
{ "id": "pb_0123456789abcdef", "type": "playback", "device": "pixel-7", "user": "Alice",
  "file_id": "…", "item_id": "…", "item_kind": "movie",
  "item": { "id": "…", "kind": "episode", "title": "Pilot", "subtitle": null, "year": null, "index": 1, "parent_index": 1,
            "parent_id": "…", "parents": [ { "id": "…", "kind": "season", "title": "Season 1" }, { "id": "…", "kind": "show", "title": "Show" } ] },
  "started_at": 1, "last_at": 2, "ended_at": null, "active": true,
  "source": "inferred", "state": "playing",
  "bytes": 123, "requests": 2, "watched_s": 12.3, "inferred_watched_s": 12.3, "max_position_s": 12.3, "position_s": 12.3,
  "duration_s": 5400.0, "file_size": 123456 }
```
`item` is `null` if the item was deleted. `source` is `inferred` or `explicit`; `state` is `playing | paused | stopped`. Only
`/sessions/{id}` adds `"ranges": [[start, end], …]` (merged served byte ranges). Device sessions:
`{ id: "dv_…", type: "device", device, user, started_at, last_at, ended_at, active, requests, bytes }`. `from`/`to` select sessions
overlapping `[from, to)`.

**`GET /now-playing`** -> `{ "ts", "total", "sessions": [ { "session_id", "user", "device", "item", "item_id", "file_id", "state", "source",
"position_s", "duration_s", "started_at", "last_at", "watched_s", "bytes" } ] }` (`item` as above). For inferred sessions `position_s`
is the end of the last served range converted to seconds.

**`GET /top`** -> `{ "metric", "by", "from", "to", "rows": [ { "key", "value", "plays", "bytes", "watch_time_s" } ] }` sorted by the chosen
metric (`value` repeats it). `key` is the item id / user name / device id / item kind; `by=item` rows also carry `item_id` and `item` (as above).
Computed from playback sessions started in `[from, to)` (so `bytes` is stream bytes, `plays` counts sessions).

**`GET /users`** -> `{ "total", "users": [ { "name", "first_seen", "last_seen", "requests", "bytes", "devices": [id], "playback_sessions", "watch_time_s" } ] }`

**`GET /devices`** -> `{ "total", "devices": [ { "id", "first_seen", "last_seen", "requests", "bytes", "user_agent", "ip", "last_user", "users": [name],
"playback_sessions", "watch_time_s", "device_sessions", "active" } ] }`, most recently seen first.

**`POST /api/analytics/playback`**
```json
{ "file_id": "…" , "item_id": "…", "event": "start|progress|pause|resume|stop", "position_s": 12.5, "duration_s": 5400 }
```
One of `file_id` / `item_id` is required (`item_id` resolves to the item's best/first file; `404` if unknown); `event` and `position_s` (>= 0)
are required, `duration_s` is optional (defaults to the probed duration). The device/user come from the identity headers/params. Events go to the
active session for (device, file), or open one (any event can open a session; `start` is just the natural first one). While `playing`,
`progress`/`pause`/`stop` add the position advance since the previous event to `watched_s` only when it is plausible continuous playback
(not more than 2x the elapsed wall time + 2 s; larger jumps are seeks). `pause` -> `paused`; `resume`/`progress`/`start` -> `playing`;
`stop` closes the session (trailing buffered `/stream` requests within 15 s still count towards its bytes). A session with no events or
requests for `ANALYTICS_PLAYBACK_IDLE_SECS` is closed too. Send `progress` every ~10-30 s. Response `200`:
`{ "ok": true, "session_id", "source", "state", "active", "file_id", "item_id", "position_s", "watched_s" }`.

## Media ingest (server)
Libraries are no longer scanned. Media enters through the watched ingest directory or the upload
endpoint, is stored content-addressed and mapped to items from its **file name only**; ffprobe is
used for technical stream info. No image is generated at ingest. See **[STORAGE.md](STORAGE.md)**.

## Server notes
Implementation choices where the contract is open; all are additive / backward compatible.

- **Extra fields**: `Library.available` (always true in v3). `Credit.source` = `scan|manual` (`scan` = created by the ingest pipeline, e.g. the `artist` credit of an artist container).
- **Extra endpoints**: `GET /api/libraries/{id}`, `GET /api/fields/{key}`.
- **Status codes**: creates (`POST /api/people`, `/api/fields`) return `201`. Deletes return `200 {"ok":true,…}`. Creating a person whose normalized name already exists → `409`. Duplicate field key → `409`. Unknown field key on `PUT .../fields/{key}` → `404` (define it first); derived field → `400`.
- **`/api/items` defaults**: `top=true` is the default only when none of `parent_id`, `kind`, `person_id`, `q` is given (so `kind=episode&library_id=…` lists all episodes). Default sort is `index` when `parent_id` is given, otherwise `title`. Default order is `asc`, except `sort=added`, which defaults to `desc`. `limit` is capped at 1000. `library_id` takes the virtual library ids.
- **Edits**: `PUT /api/items/{id}/fields/{year|genres|overview}` behaves like PATCH (marks the field locked). `DELETE /api/items/{id}/fields/{key}` removes a user value; on a core field it also unlocks it. User values always override values set by the ingest pipeline for the same key.
- **Credits**: manual credits (POST) and credits created by the pipeline can both be deleted.
- **Person ids** are hashes of the normalized name and do not change when a person is renamed via PATCH.
- **Person display**: `PersonDetail.display` renders custom fields set on the person. `applies_to` may contain `"person"`.
- **Automatic custom fields**: file names with a trailing `[youtubeid]` give the item a `youtube` url field (`https://youtu.be/<id>`, automatic custom definition, placement `info`, order 500).
- **MediaFile.label** is computed from the probe: `"{height}p · {CODEC}"` for video, `"{CODEC} · {kbps} kbps"` for audio.
- **Music video names**: `Title ｜ Album ｜ People [ytid]` gives title = first segment with upload noise removed (`DILBAR 8K Full Song` → `DILBAR`) and subtitle = the remaining segments joined by ` · `. `A - B` is read as `Artist - Title`, except when A is ALL CAPS and B is not (`O SAKI SAKI - Nora Fatehi` → title `O SAKI SAKI`).
- **Config**: see [STORAGE.md](STORAGE.md#configuration) (incl. `ASSET_DIR`, `ASSET_DEFAULT_WIDTH`, `ASSET_GC_*`) plus `BIND` (default `0.0.0.0:8096`), `FRAME_CACHE_MB`, `FRAME_THREADS`. `LIBRARIES`, `SCAN_INTERVAL`, `LEGACY_DATA_DIR` and `ARTIST_INFO_DIR` are gone. A `DATA_DIR` holding a v2 database is refused at startup (v3 needs a fresh one).
- **v2.1 fields kept for shape compatibility** (clients that ignore unknown fields are unaffected):
  - `ItemSummary.index_end`: last episode of a multi-episode file (`S01E04-E05` gives `index` 4, `index_end` 5). Null otherwise.
  - The v2 slot `ItemSummary.images` and `Person.image` are **gone** (see Images; `images` is now a field-key map). `Person`/`PersonRef` no longer have `image`.
  - `ItemDetail.extras` is always `[]`; the `extra` item kind no longer exists.
  - `MediaFile.part` / `MediaFile.edition` and queue entries' `part` are always null (no stacking or editions; versions are just more files of the item, labelled by resolution/codec).

## Still frames (paused high-quality frame) — v2.1
`GET /frame/{file_id}?t=<seconds, float>&max_w=<int, optional>&fmt=<jpg|webp, default jpg>`
- Extracts ONE still frame (no transcoding of media) at timestamp `t` from that MediaFile with frame-accurate seek, returns `image/jpeg` (quality ~92) or `image/webp`.
- Full source resolution unless `max_w` is given (aspect preserved). HDR sources are tone-mapped to SDR.
- Response headers: `X-Frame-Width`, `X-Frame-Height`, `X-Frame-Pts` (actual pts in seconds), `Cache-Control: public, max-age=31536000`.
- Server caches results on disk (`DATA_DIR/frames`, LRU-capped, default 2 GB via env `FRAME_CACHE_MB`), keyed by file id + pts rounded to the millisecond + max_w + fmt.
- Concurrency limited (one extraction at a time, ffmpeg threads capped via env `FRAME_THREADS`, default 4); a newer request for the same file cancels/obsoletes queued older ones. 404 for unknown file, 400 for t out of range.
Clients pick the "best" file for frames as: highest pixel count among the item's files (originals preferred on ties).

### Server notes: still frames
- A request that a newer `/frame` request for the same file makes obsolete gets `409 {"error":"superseded…"}`. This happens whether it was still queued or already running (its ffmpeg is killed).
- Extra header `X-Frame-Cache: hit|miss`. `X-Frame-Pts` is the pts of the first frame at or after `t` (taken from ffmpeg `showinfo` with `-copyts`). `t` is clamped to just below the duration. A file without a video stream gives `400`.
- HDR (PQ/HLG) sources are tone-mapped with zscale + tonemap (hable) when the ffmpeg build has those filters; otherwise a warning is logged at startup and frames are not tone-mapped.
- A cold frame costs one decode from the preceding keyframe to `t`. On 8K AV1 with 4 cores that takes about 1.3–25 s depending on distance from the keyframe (GOPs up to ~5 s). A 1080p variant takes about 0.35–0.45 s, and cache hits take under 15 ms. Clients that want a fast pause frame can request it from the 1080p variant first.
