# MediaDir v3 — managed storage & ingest

MediaDir owns its media. There are no library folders, no scanners and no compatibility with other
tools. Media enters through one of two doors and lives in a private, content-addressed store.

## Configuration
| env | meaning | default |
|---|---|---|
| `INGEST_DIR` | The only directory MediaDir watches. Drop media files anywhere inside it (any depth, any structure). | `./ingest` |
| `MEDIA_DIR` | MediaDir's private store. Not for humans; nothing watches it. | `./store` |
| `DATA_DIR` | Database, thumbnails, frame cache, backups. | `./data` |
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
6. **Thumbnail** — one frame via ffmpeg into `DATA_DIR` (no transcoding of media).
7. Item appears in the API.

Ingest records (`GET /api/ingest`): `{ id, source, size, hash, state, error, item_id, ingested_at }` with
`state ∈ waiting | hashing | placing | probing | done | duplicate | failed | ignored`.
The ingest dir is tracked by `(device, inode, size, mtime)` so linked originals that remain in the
ingest dir are never ingested twice, across restarts too.

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
| GET | `/api/ingest/status` | `{ watching, ingest_dir, media_dir, queue, last_sidecar_sync, last_verify, last_backup, problems: [...] }` |
| POST | `/api/ingest/scan` | force a poll of the ingest dir |
| POST | `/api/ingest/upload` | upload (see above) |
| DELETE | `/api/items/{id}` | delete item + unreferenced blobs |
| DELETE | `/api/files/{id}` | delete one file/version |
| POST/PATCH/DELETE | `/api/libraries…` | **removed** (405) |
