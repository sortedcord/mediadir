# MediaDir — API contract (v2 shapes, v3 storage)

Shared contract between the Rust server (`server/`) and the Android app (`android/`).
**No transcoding, ever.** The server indexes metadata and serves original bytes (HTTP Range).
How media gets in (ingest dir, uploads, the content-addressed store, file-name mapping,
virtual libraries, deletes, durability) is specified in **[STORAGE.md](STORAGE.md)** (v3).
The response shapes below are unchanged from v2 except where marked *v3*.
All ids are opaque strings (16 hex chars, stable hashes). All image/stream URLs in responses are
**server-relative paths** (e.g. `/image/item/ab12/thumb`); clients prefix the server base URL.
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
  "images": { "thumb": "/image/item/…/thumb", "poster": null, "backdrop": null },
  "added_at": 1793433022,
  "card": [ DisplayBlock, … ]
}
```
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
`fields` = raw values of all custom + editable builtin fields set on the item.
`display` = all non-hidden blocks for the detail page (placements `header`, `info`, `section`, plus card placements).
`locked` = core field names manually edited.

### MediaFile
```json
{
  "id": "…", "path": "Original File Name.mkv", "label": "4320p · AV1", "original": true,
  "hash": "<blake3 hex>", "original_name": "Original File Name.mkv", "added_at": 1793433022,
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

### Person / Credit
```json
// Person (summary)
{ "id": "…", "name": "Nora Fatehi", "sort_name": "Fatehi, Nora", "image": "/image/person/…" | null, "credit_count": 4 }
// PersonDetail = Person + 
{ "bio": "…", "birth_date": "1992-02-06", "fields": {…}, "display": [DisplayBlock],
  "credits": [ { "id": "<credit id>", "role": "actor", "character": null, "item": ItemSummary } ] }
// Credit (on ItemDetail)
{ "id": "…", "role": "actor", "character": "Herself", "order": 0,
  "person": { "id": "…", "name": "…", "image": null } }
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
- `type`: `text | number | date | url | tags | rating | bool | duration | person`
  (`person` values are person ids, or arrays of person ids — rendered as tappable people).
- `derived`: computed by the server (from probing/structure); values not editable, display still configurable.
- `applies_to`: item kinds; `[]` = all kinds.
- `display.placement`: `card_badge | card_meta | header | info | section | hidden`
  - `card_badge` small pill over the card image · `card_meta` small text line under card title
  - `header` chips next to the title on detail · `info` key/value table row · `section` full-width block with heading (long text, tag clouds, people rows)
- `display.widget`: `badge | chip | text | rating | link | tags | people | markdown | stat`
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
  "people": [ { "id": "…", "name": "…", "image": null } ]   // only for widget=people / type=person
}
```
Blocks with empty/null values are omitted. Clients render purely from these; they must not hardcode which fields exist.

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
| PUT | `/api/people/{id}/image` | raw image body (jpeg/png) |
| DELETE | `/api/people/{id}` | removes person + credits |
| GET | `/api/fields` | `[FieldDefinition]` (builtin + custom, sorted by order) |
| POST | `/api/fields` | custom def `{key, label, type, applies_to, display}` |
| PUT | `/api/fields/{key}` | update `label, applies_to, display` (builtins: display/label only) |
| DELETE | `/api/fields/{key}` | custom only (also deletes values) |
| GET | `/api/search` | `q` → `{ "items": [ItemSummary], "people": [Person] }` |
| GET/HEAD | `/stream/{file_id}` | original bytes, Range/206 |
| GET | `/image/item/{id}/{thumb\|poster\|backdrop}` | 404 if none; long cache headers |
| GET | `/api/ingest` · `/api/ingest/{id}` | *v3:* ingest records ([STORAGE.md](STORAGE.md)) |
| GET | `/api/ingest/status` | *v3:* watcher/queue/task status |
| POST | `/api/ingest/scan` | *v3:* force a poll of the ingest dir, 202 |
| POST | `/api/ingest/upload` | *v3:* raw body + `X-Filename` |
| GET | `/image/person/{id}` | 404 if none |

## Media ingest (server)
Libraries are no longer scanned. Media enters through the watched ingest directory or the upload
endpoint, is stored content-addressed and mapped to items from its **file name only**; ffprobe is
used for technical stream info, ffmpeg for one thumbnail frame. See **[STORAGE.md](STORAGE.md)**.

## Server notes
Implementation choices where the contract is open; all are additive / backward compatible.

- **Extra fields**: `Library.available` (always true in v3). `Credit.source` = `scan|manual` (`scan` = created by the ingest pipeline, e.g. the `artist` credit of an artist container).
- **Extra endpoints**: `GET /api/libraries/{id}`, `GET /api/fields/{key}`.
- **Status codes**: creates (`POST /api/people`, `/api/fields`) return `201`. Deletes return `200 {"ok":true,…}`. Creating a person whose normalized name already exists → `409`. Duplicate field key → `409`. Unknown field key on `PUT .../fields/{key}` → `404` (define it first); derived field → `400`.
- **Image URLs** carry a cache-busting query (`/image/item/{id}/thumb?v=1a2b3c4d`), so the long `Cache-Control` never serves stale art. The bare path also works.
- **Images (v3)**: the only artwork is a generated `thumb` (one video frame). Containers (show, season, artist) use their first child's thumb; children without one fall back to their ancestors. `poster`, `backdrop` and the other slots are always null (404). An `images.*` URL is non-null only when that request will succeed.
- **`/api/items` defaults**: `top=true` is the default only when none of `parent_id`, `kind`, `person_id`, `q` is given (so `kind=episode&library_id=…` lists all episodes). Default sort is `index` when `parent_id` is given, otherwise `title`. Default order is `asc`, except `sort=added`, which defaults to `desc`. `limit` is capped at 1000. `library_id` takes the virtual library ids.
- **Edits**: `PUT /api/items/{id}/fields/{year|genres|overview}` behaves like PATCH (marks the field locked). `DELETE /api/items/{id}/fields/{key}` removes a user value; on a core field it also unlocks it. User values always override values set by the ingest pipeline for the same key.
- **Credits**: manual credits (POST) and credits created by the pipeline can both be deleted.
- **Person ids** are hashes of the normalized name and do not change when a person is renamed via PATCH.
- **Person display**: `PersonDetail.display` renders custom fields set on the person. `applies_to` may contain `"person"`.
- **Automatic custom fields**: file names with a trailing `[youtubeid]` give the item a `youtube` url field (`https://youtu.be/<id>`, automatic custom definition, placement `info`, order 500).
- **MediaFile.label** is computed from the probe: `"{height}p · {CODEC}"` for video, `"{CODEC} · {kbps} kbps"` for audio.
- **Music video names**: `Title ｜ Album ｜ People [ytid]` gives title = first segment with upload noise removed (`DILBAR 8K Full Song` → `DILBAR`) and subtitle = the remaining segments joined by ` · `. `A - B` is read as `Artist - Title`, except when A is ALL CAPS and B is not (`O SAKI SAKI - Nora Fatehi` → title `O SAKI SAKI`).
- **Config**: see [STORAGE.md](STORAGE.md#configuration) plus `BIND` (default `0.0.0.0:8096`), `FRAME_CACHE_MB`, `FRAME_THREADS`. `LIBRARIES`, `SCAN_INTERVAL`, `LEGACY_DATA_DIR` and `ARTIST_INFO_DIR` are gone. A `DATA_DIR` holding a v2 database is refused at startup (v3 needs a fresh one).
- **v2.1 fields kept for shape compatibility** (clients that ignore unknown fields are unaffected):
  - `ItemSummary.index_end`: last episode of a multi-episode file (`S01E04-E05` gives `index` 4, `index_end` 5). Null otherwise.
  - `ItemSummary.images` also carries `banner`, `clearlogo`, `clearart`, `landscape`, `discart`, `keyart`, `characterart` — always null in v3.
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
