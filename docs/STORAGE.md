# MediaDir v3 — managed storage & ingest

MediaDir owns its media. There are no library folders, no scanners and no compatibility with other
tools. Media enters through one of two doors and lives in a private, content-addressed store.

## Configuration
| env | meaning | default |
|---|---|---|
| `INGEST_DIR` | The only directory MediaDir watches. Drop media files anywhere inside it (any depth, any structure). | `./ingest` |
| `MEDIA_DIR` | MediaDir's private store. Not for humans; nothing watches it. | `./store` |
| `ASSET_DIR` | Content-addressed image assets (see [Image assets](#image-assets)). Separate from `MEDIA_DIR`; must not contain or be inside `INGEST_DIR`/`MEDIA_DIR`. | `./assets` |
| `ASSET_DEFAULT_WIDTH` | Output width of an asset transform request that gives neither `w` nor `h`. | `1280` |
| `ASSET_GC_INTERVAL` | Seconds between unreferenced-asset garbage collection runs (`0` = off). | `3600` |
| `ASSET_GC_GRACE_SECS` | An unreferenced asset is only collected once it is older than this (age = last upload of those bytes). | `86400` |
| `DATA_DIR` | Database, asset transform cache (`asset-cache/`), frame cache, backups. | `./data` |
| `INGEST_POLL_SECS` | Poll interval in addition to inotify (network mounts don't deliver inotify). `0` = inotify only. | `60` |
| `INGEST_SETTLE_SECS` | A file must be unchanged (size + mtime) this long before it's ingested, so partial downloads/copies are never taken. | `10` |
| `SIDECAR_INTERVAL` | Seconds between sidecar manifest sync runs. | `3600` |
| `VERIFY_INTERVAL` | Seconds between integrity check runs. | `86400` |
| `BACKUP_INTERVAL` / `BACKUP_KEEP` | Database backup cadence (seconds) and how many to keep. | `21600` / `14` |

## Two ways in
1. **Ingest directory.** Watched recursively (inotify via `notify`, plus polling). Every regular file
   whose extension is a known video/audio extension and which ffprobe can open is ingested.
   Everything else (nfo, images, subtitles, text, partial `*.part`/`*.crdownload`/`*.tmp`, hidden files) is ignored.
2. **Upload.** `POST /api/ingest/upload` with the raw file as the request body and the original file
   name in `X-Filename` (URL-encoded). Streamed to `MEDIA_DIR/.staging/` and then goes through the
   exact same pipeline. Returns the ingest record.

## Pipeline (per file)
1. **Settle** — wait until size+mtime are stable for `INGEST_SETTLE_SECS`.
2. **Hash** — BLAKE3 of the full content (streamed). The hash is the file's identity.
3. **Place** into the store at `MEDIA_DIR/<h[0..2]>/<h[2..4]>/<hash>.<ext>`:
   reflink (FICLONE) → hardlink → move. Never copy, never symlink. Ingest-dir files that were
   linked stay where they are; moved ones disappear from the ingest dir (only when linking failed).
   Stored files are made read-only (0444), the store tree is 0700.
   If the hash is already in the store, the file is a duplicate: nothing is placed, it is recorded as `duplicate`.
4. **Probe** — ffprobe for technical streams only (codecs, resolution, fps, duration, tracks).
5. **Name → item** — the **file name is the only metadata source** (no folders, no nfo, no tags, no artwork files):
   - `…S01E02…` / `1x02` / multi-episode → `show` › `season` › `episode` (show title from the part before the pattern)
   - `Title (Year)` → `movie`
   - audio-only file → `track`; `Artist - Title` → artist/title, grouped under `artist` (no album unless later edited)
   - `Artist - Title` video → `music_video`; YouTube-style `Title ｜ … [id]` names are cleaned
   - anything else → `video` titled from the cleaned file name
   Grouping is by normalized name: a second file mapping to the same movie (title+year) or the same
   episode becomes another **MediaFile (version)** of that item, labelled by resolution/codec.
   Kind and every field remain editable afterwards; future smart/AI detection plugs in here.
6. Item appears in the API. **No thumbnail or other image is generated at ingest**; images are
   `image` field values attached later by clients ([Image assets](#image-assets)).

Ingest records (`GET /api/ingest`): `{ id, source, size, hash, state, error, item_id, ingested_at }` with
`state ∈ waiting | hashing | placing | probing | done | duplicate | failed | ignored`.
The ingest dir is tracked by `(device, inode, size, mtime)` so linked originals that remain in the
ingest dir are never ingested twice, across restarts too.

## Image assets
Images are not special slots. They are values of ordinary fields of type `image` (create the field
definitions you want, e.g. `poster`, `backdrop`, `photo`, with `POST /api/fields`). The server never
chooses, ranks or falls back between images; the client decides which one is shown where.

- **Storage.** `ASSET_DIR/<h[0..2]>/<h[2..4]>/<hash>.<ext>` where `hash` is the BLAKE3 hex of the
  bytes (same scheme as media files) and `ext` is `jpg|png|webp|gif`. Files are 0444, dirs 0700.
  Originals are never modified or re-encoded; they are assumed to be the highest resolution available.
  The `assets` table holds `hash (PK), mime, width, height, size, created_at` (+ `verified_at` for the
  integrity rotation). Width/height are the displayed size (EXIF orientation applied).
- **Upload.** `POST /api/assets`, raw body (JPEG, PNG, WebP or GIF; max 100 MB, `413` above; `400` for
  anything else or an undecodable image). Response `{ "hash", "mime", "width", "height", "size" }`:
  `201` for a new asset, `200` for bytes that already exist (deduplicated by hash; `created_at` is
  refreshed, which restarts the GC grace period).
- **Field values.** An `image` field value is one ref `{"hash": "<64 hex>"}` or an ordered, non-empty
  list `[{"hash": …}, …]`. `PUT /api/items/{id}/fields/{key}` and `PUT /api/people/{id}/fields/{key}`
  require every hash to exist in `assets` (`400` otherwise); hashes are lower-cased, extra keys dropped,
  the single/list shape is preserved. `null` clears the field.
- **Delivery** (`GET /asset/{hash}`):
  - no query params, or `original=1`, or only unknown params (e.g. a cache-buster `v=`) → the original bytes,
    original `Content-Type`.
  - any of `w`, `h`, `fit`, `fmt`, `q` → a transform (below).
- **Transform params.** `w`, `h`: target pixels (positive integers). `fit=cover|contain` (default `cover`;
  only matters when both `w` and `h` are given: `cover` scales to fill the `w`×`h` box and center-crops,
  `contain` scales to fit inside it, no padding). `fmt=webp|jpg|png` (default `webp`; `jpg` has no alpha).
  `q=1..100` (default 80; ignored for png). With only `w` or only `h` the aspect ratio is kept. With
  neither, `w = ASSET_DEFAULT_WIDTH`. **Sizes are not restricted** (no allowlist) but the output is clamped to the
  original: never upscaled (for `cover`, the `w`×`h` box itself is shrunk, keeping its aspect, until it fits
  inside the original). Bad params → `400`; unknown hash → `404`.
- **Cache.** Variants are rendered with ffmpeg on first request and stored in
  `DATA_DIR/asset-cache/<h[0..2]>/<h[2..4]>/`, keyed by hash + the *normalized* plan (final size, crop vs scale,
  format, q), so requests that clamp to the same result share one entry. Responses carry
  `Cache-Control: public, max-age=31536000, immutable` and an `ETag` (`"<hash>"` for originals,
  `"<hash>-<WxH>_<cover|scale>_<fmt>_q<q>"` for variants; `If-None-Match` → `304`). Extra headers:
  `X-Asset-Cache: original|hit|miss`, and for variants `X-Asset-Width`, `X-Asset-Height`.
  The cache is not size-capped; it is removed per asset when the asset is collected.
- **Garbage collection.** The `asset gc` periodic task deletes assets (file, cache entries, row) that no
  item or person `image` field value references **and** whose `created_at` is older than `ASSET_GC_GRACE_SECS`,
  so a freshly uploaded asset survives until it has been attached. Deleting an item/person, clearing a field or
  deleting a field definition releases its assets.
- **Durability.** The integrity task checks every asset file exists with the right size and re-hashes a rotating
  slice; problems appear in `GET /api/ingest/status` `problems` with `file_id: "asset:<hash>"` and an empty
  `item_id`. The `assets` table is part of the database backups; back up `ASSET_DIR` together with `MEDIA_DIR`.
  No sidecars are written for assets (the file name is the hash, the mime is the extension).

## Deleting
`DELETE /api/items/{id}` removes the item (and its children) and every stored file that is no longer
referenced. Files left in the ingest dir are untouched and are **not** re-ingested (their inode is remembered).
`DELETE /api/files/{id}` removes one version.

## Libraries
Libraries are virtual: one per item kind that has items (`movies`, `shows`, `music`, `music_videos`,
`videos`), with stable ids. `GET /api/libraries` keeps its existing shape (`path` = null).
Creating/deleting libraries is gone (405).

## Durability
- **Database** is the source of truth. Online SQLite backups (`VACUUM INTO`) every `BACKUP_INTERVAL`
  into `DATA_DIR/backups/`, keeping `BACKUP_KEEP`.
- **Sidecars** — a periodic task (not realtime) writes `<hash>.json` next to each stored file:
  `{ hash, size, original_name, ingested_at, item: { id, kind, title, year, index, parent titles… } }`.
  Only for files whose item changed since the last run. Enough to rebuild titles/structure without the DB.
- **Integrity** — a periodic task checks every stored file exists with the right size and re-hashes a
  rotating slice (full re-hash over ~a week). Problems are reported at `GET /api/ingest/status`
  (`missing`, `corrupt`, last run times) and logged.

## API changes (additive except libraries CRUD)
| Method | Path | |
|---|---|---|
| GET | `/api/ingest` | recent ingest records, `?state=` filter, newest first |
| GET 
| POST | `/api/ingest/scan` | force a poll of the ingest dir |
| POST | `/api/ingest/upload` | upload (see above) |
| DELETE | `/api/items/{id}` | delete item + unreferenced blobs |
| DELETE | `/api/files/{id}` | delete one file/version |
| POST/PATCH/DELETE | `/api/libraries…` | **removed** (405) |
| POST | `/api/assets` | upload an image asset (see above) |
| GET | `/asset/{hash}` | original or transformed image (see above) |
