# MediaDir v3 — managed storage & ingest

MediaDir owns its media. There are no library folders, no scanners and no compatibility with other
tools. Media enters through one of two doors and lives in a private, content-addressed store.

## Configuration
| env | meaning | default |
|---|---|---|
| `INGEST_DIR` | The only directory MediaDir watches. Pure drop zone: drop media files anywhere inside it (any depth, any structure); ingested entries are *moved out*, so it ends up empty. **Must be on the same filesystem as `MEDIA_DIR`** (checked at startup; the server refuses to start otherwise). | `./ingest` |
| `MEDIA_DIR` | MediaDir's private store. Not for humans; nothing watches it. Same filesystem as `INGEST_DIR`. | `./store` |
| `ASSET_DIR` | Content-addressed image assets (see [Image assets](#image-assets)). Separate from `MEDIA_DIR`; must not contain or be inside `INGEST_DIR`/`MEDIA_DIR`. | `./assets` |
| `ASSET_DEFAULT_WIDTH` | Output width of an asset transform request that gives neither `w` nor `h`. | `1280` |
| `ASSET_GC_INTERVAL` | Seconds between unreferenced-asset garbage collection runs (`0` = off). | `3600` |
| `ASSET_GC_GRACE_SECS` | An unreferenced asset is only collected once it is older than this (age = last upload of those bytes). | `86400` |
| `DATA_DIR` | Database, `analytics.db`, asset transform cache (`asset-cache/`), frame cache, backups. | `./data` |
| `INGEST_POLL_SECS` | Poll interval in addition to inotify (network mounts don't deliver inotify). `0` = inotify only. | `60` |
| `INGEST_SETTLE_SECS` | A file must be unchanged (size + mtime) this long before it's ingested, so partial downloads/copies are never taken. | `10` |
| `SIDECAR_INTERVAL` | Seconds between sidecar manifest sync runs. | `3600` |
| `VERIFY_INTERVAL` | Seconds between integrity check runs. | `86400` |
| `LAZY_HASH_MB_S` | Read-rate cap (MiB/s) of the background full hash of symlinked files (`0` = unlimited). | `64` |
| `ANALYTICS_SNAPSHOT_SECS` | Seconds between [analytics](#analytics-storage) library snapshots (`0` = only the one taken at startup). | `3600` |
| `ANALYTICS_RAW_DAYS` | Raw request-log rows are deleted after this many days; hourly aggregates and snapshots are kept forever. | `30` |
| `ANALYTICS_PLAYBACK_IDLE_SECS` | A playback session closes after this long without `/stream` requests or playback events. | `300` |
| `ANALYTICS_DEVICE_IDLE_SECS` | A device session closes after this long without any request. | `1800` |
| `BACKUP_INTERVAL` / `BACKUP_KEEP` | Database backup cadence (seconds) and how many to keep. | `21600` / `14` |

## Two ways in
1. **Ingest directory.** Watched recursively (inotify via `notify`, plus polling). Every regular file,
   hardlink or symlink whose name has a known video/audio extension and which ffprobe can open is ingested.
   Everything else (nfo, images, subtitles, text, partial `*.part`/`*.crdownload`/`*.tmp`, hidden files) is ignored.
2. **Upload.** `POST /api/ingest/upload` with the raw file as the request body and the original file
   name in `X-Filename` (URL-encoded). Streamed to `MEDIA_DIR/.staging/` and then goes through the
   exact same pipeline. Returns the ingest record.

## Pipeline (per file)
1. **Settle** — wait until size+mtime are stable for `INGEST_SETTLE_SECS`.
2. **Hash** — BLAKE3 of the full content (streamed). The hash is the file's identity.
3. **Place** into the store at `MEDIA_DIR/<h[0..2]>/<h[2..4]>/<hash>.<ext>` with **one `rename(2)` of the
   directory entry** out of `INGEST_DIR` (same filesystem, so no bytes are copied and the inode is kept):
   - a regular file is renamed into the store;
   - a hardlink: that link is renamed into the store (other links elsewhere stay; they and the store entry share the inode);
   - a symlink: the **symlink itself** is renamed into the store, never its target (see [Symlinked media](#symlinked-media)).
   Uploads are staged in `MEDIA_DIR/.staging/` and renamed the same way. MediaDir never chmods stored files (there is
   no read-only protection); store directories are 0700. After the move, folders in `INGEST_DIR` that became empty
   are removed (`INGEST_DIR` itself stays).
   If the hash is already known, the file is a duplicate: nothing is placed, it is recorded as `duplicate` and the entry
   stays in `INGEST_DIR`. The same holds for `ignored` and `failed` entries.
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
`placement` is `move` (regular file/hardlink) or `symlink`. Entries that stay in the ingest dir (duplicates,
ignored, failed) are tracked by `(device, inode, size, mtime)` in `seen_files`, so they are never processed twice
(failures are not retried endlessly; `POST /api/ingest/scan` forgets failures once and retries them), across restarts too.

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

## Symlinked media
For big libraries that already live on another disk (e.g. `/mnt/sdb1/Movies`), drop a **symlink** in `INGEST_DIR`
instead of the file. "External" simply means *the store entry is a symlink*:

```sh
ln -s /mnt/sdb1/Movies/Some.Film.2012.mkv "$INGEST_DIR/"      # the link's name is the file name used for naming
```

- **The link moves, the target never does.** The symlink itself is renamed into the store (`rename(2)`, like any
  entry) and `INGEST_DIR` loses it. Metadata for probing, fingerprinting and hashing comes from the target, but
  MediaDir never copies, moves, writes or chmods the target (nor chmods through the link); read-only mounts work.
  A link with a **relative** target is re-created in the store with the absolute, canonical target (so it still
  resolves from there) and the ingest entry is removed. Then the usual pipeline: settle → identify → ffprobe → name → item.
- **Failures** (`state: failed`, nothing catalogued, entry left in `INGEST_DIR`, not retried until you fix it and
  `POST /api/ingest/scan`, `POST /api/ingest/{id}/retry` or `POST /api/ingest/retry-failed`): a dangling link,
  a target that is not a regular file.
- **Duplicates by fingerprint.** A link is a duplicate when its `fp:` matches a row's hash *or* its `fingerprint`
  column (kept after the lazy hash replaced `hash`); file ids derive from the hash at insert time, so they never
  change and a colliding id is replaced by a fresh one. When the lazy hash finds an existing file with the same
  content, the symlinked row is dropped and all its ingest records become `duplicate` of the surviving file.
- **Leftovers.** At startup, finished records whose symlink is still in `INGEST_DIR` are reconciled: file row and
  store symlink present → leftover link removed (target untouched); file row gone → record becomes `duplicate`
  (content catalogued) or `failed` (retryable).
- **Row.** `media_files` has `location` (`store` | `external`; `external` = the store entry is a symlink),
  `hash_state` (`full` | `pending`), `fingerprint` and `src_mtime` (added by an idempotent startup migration).
  `store_path` is relative to `MEDIA_DIR` like any other file.
- **Lazy hashing.** Hashing a multi-GB file over a slow disk at ingest would block cataloguing, so a symlink gets a
  **fingerprint** = BLAKE3 over a domain tag, the file size, the first 4 MiB and the last 4 MiB (files <= 8 MiB are
  read whole). Until the full hash is known the unique `hash` column holds `fp:<64 hex>` (prefixed so it can never
  equal a real BLAKE3) with `hash_state = pending`, and the store symlink lives at `fp/<fp[0..2]>/<fp>.<ext>`; the
  fingerprint is the file's identity and dedup key (the same target linked twice is a `duplicate`). A background
  task, **one file at a time and rate-limited** (`LAZY_HASH_MB_S`), computes the full BLAKE3, replaces `hash` with
  it (`hash_state = full`) and **renames the store symlink to its final hash-based path**
  (`<h[0..2]>/<h[2..4]>/<hash>.<ext>`, updating `store_path`); file/item ids never change. If the full hash equals an
  already catalogued file the symlinked row is a duplicate: it is dropped (and its item too, if it had no other
  files; database rows only), its store symlink is removed (never the target), and its ingest records become
  `duplicate` of the existing file. The fingerprint is not a content guarantee (a change in the middle with
  identical size and edges is not seen) — that is why the full hash follows. Once `full`, the file joins the normal
  full-rehash rotation.
- **Serving.** `/stream` (HTTP Range), ffprobe and `/frame` read through the store symlink.
- **Deleting** (`DELETE /api/items/{id}` / `/api/files/{id}`) removes the store entry: for a symlink only the link,
  never the target.
- **Integrity.** Every verify run checks the target's existence, size and mtime. Missing is reported as `missing`; a
  changed size/mtime is reported as `changed` (once), the file is re-fingerprinted and its hash goes back to `pending`
  (size updated; it is not re-probed). Nothing is ever deleted.
- **Sidecars.** Not written for symlinked entries (nothing is written next to or about them; the database and its
  backups are the source of truth for them).
- **Status.** `GET /api/ingest/status` includes `pending_hashes`.
- **Migration from `EXTERNAL_ROOT`.** Earlier versions kept the symlink in `INGEST_DIR` and stored `store_path`
  relative to an `EXTERNAL_ROOT`. At startup, every `external` row whose store entry is not a symlink yet is converted:
  a store symlink is created (absolute target = `EXTERNAL_ROOT` env var if still set + old `store_path`, otherwise
  derived from the leftover ingest symlink recorded in the ingest records), `store_path` is set to the store-relative
  path, and the leftover ingest symlink is removed. It is idempotent, logged (`migrate: …`), and never touches targets.
  Rows that cannot be resolved (neither the env var nor the ingest symlink available) are reported and retried on the
  next start. `EXTERNAL_ROOT` is read only for this and can be dropped afterwards. Hashed regular files that earlier
  versions left in `INGEST_DIR` as hardlink originals are not migrated: they stay where they are.

## Analytics storage
Observability data lives in its own SQLite file, `DATA_DIR/analytics.db` (WAL), so the main database and
its backups stay lean. The API is documented in [API.md](API.md#analytics). Tables:
`snapshots` (JSON library snapshot per timestamp), `requests` (raw log), `requests_hourly` (aggregates by
hour × route group × route template × status class × device × user), `devices`, `device_users`,
`playback_sessions`, `device_sessions`, `meta`.

- **Request log.** A middleware wraps every response body and counts the bytes actually sent (Range
  partials, client aborts and gzip'd API bodies included). Events go through a bounded in-memory queue to
  one writer thread that batch-inserts about once a second, so request latency is unaffected; if the queue
  overflows, events are dropped and counted (`dropped_events` in the overview). `/api/analytics/*` itself is
  not logged. Unrouted paths are logged as route `(unmatched)`.
- **Retention.** Each request also increments its hourly aggregate row when it is written. Raw rows older than
  `ANALYTICS_RAW_DAYS` are deleted hourly (and at startup); aggregates, snapshots, devices and sessions are kept
  forever. Percentiles exist only for raw data.
- **Sessions** live in memory in the writer and are persisted every flush; sessions still open at shutdown are
  closed (at their last activity) on the next start.
- **Backups.** The backup task also writes `DATA_DIR/backups/analytics-<unix>.db` (`VACUUM INTO`), pruned with
  the same `BACKUP_KEEP`. Losing analytics.db loses only history.
- **Identity.** No auth: `X-MediaDir-Device` / `X-MediaDir-User` headers (or `?device=` / `?user=`), else
  `anon:<client ip>`. The IP is the first `X-Forwarded-For` / `X-Real-IP` value if present, else the socket peer.

## Deleting
`DELETE /api/items/{id}` removes the item (and its children) and removes each of their store entries: a regular
file is deleted, a hardlinked file loses only that link (other links keep the data), a [symlink](#symlinked-media)
loses only the link (the target is never touched). `DELETE /api/files/{id}` removes one version.

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
| POST | `/api/ingest/{id}/retry` · `/api/ingest/retry-failed` | retry failed record(s) whose entry is still in `INGEST_DIR` |
| POST | `/api/ingest/upload` | upload (see above) |
| DELETE | `/api/items/{id}` | delete item + unreferenced blobs |
| DELETE | `/api/files/{id}` | delete one file/version |
| POST/PATCH/DELETE | `/api/libraries…` | **removed** (405) |
| POST | `/api/assets` | upload an image asset (see above) |
| GET | `/asset/{hash}` | original or transformed image (see above) |
