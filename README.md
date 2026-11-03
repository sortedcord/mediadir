# MediaDir

A Rust media server and native Android client. The server manages original media in a BLAKE3 content-addressed store and streams it without transcoding. Metadata comes from file names and can be edited in the app.

## Run the server

Requires Rust, ffmpeg, and ffprobe on PATH. Storage placement uses Unix filesystem APIs (Linux reflinks when supported).

```sh
cd server
DATA_DIR=./data-v3 MEDIA_DIR=./store-v3 INGEST_DIR=./ingest-v3 cargo run --release
```

Defaults: HTTP on `0.0.0.0:8096`, ingest directory `./ingest`, media store `./store`, and application data `./data`, relative to the server's working directory. The command above selects separate v3 directories to preserve older local data. Drop media in the ingest directory, or upload through the app's Settings → Media ingest & storage screen.

The server requires a fresh v3 database; it refuses older databases. Keep old data separately and set `DATA_DIR` to a fresh directory when switching from v2.

See [storage and configuration](docs/STORAGE.md) and the [API contract](docs/API.md).

## Run in production

Build an optimized binary once (and after every update):

```sh
cd server
cargo build --release
```

Pick four directories on persistent storage, ideally on the same filesystem so ingest can reflink instead of copying. `MEDIA_DIR` and `ASSET_DIR` hold the originals; back them up together with `DATA_DIR` (the database, with rolling backups in `DATA_DIR/backups`).

| Variable | Purpose |
|---|---|
| `BIND` | Listen address, default `0.0.0.0:8096` |
| `DATA_DIR` | Database, frame and image caches, backups |
| `MEDIA_DIR` | Content-addressed media store |
| `ASSET_DIR` | Content-addressed image store (must not overlap ingest/media) |
| `INGEST_DIR` | Drop folder watched for new media |

All other knobs are listed in [docs/STORAGE.md](docs/STORAGE.md).

### As a systemd service

`server/mediadir-server.service` is a ready-made unit. Edit `User`, `Group`, `WorkingDirectory`, `ExecStart` and the `Environment=` paths for your machine (or put overrides in `server/service.env`), then:

```sh
sudo cp server/mediadir-server.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now mediadir-server
systemctl status mediadir-server
journalctl -u mediadir-server -f        # logs
curl http://localhost:8096/api/health   # {"ok":true,"version":3}
```

After pulling updates: `cargo build --release && sudo systemctl restart mediadir-server`.

### Without systemd

```sh
cd server
BIND=0.0.0.0:8096 DATA_DIR=/srv/mediadir/data MEDIA_DIR=/srv/mediadir/store \
ASSET_DIR=/srv/mediadir/assets INGEST_DIR=/srv/mediadir/ingest \
nohup ./target/release/mediadir-server >> mediadir.log 2>&1 &
```

The server has no authentication; expose it only on a trusted LAN or behind a VPN / authenticating reverse proxy.

## MCP server for AI agents

`mcp/` holds an MCP server that exposes metadata read/edit tools over the HTTP API. See [mcp/README.md](mcp/README.md).

## Android

Requires JDK 17 and Android SDK 35. Set `sdk.dir` in `android/local.properties` for your machine.

```sh
cd android
./gradlew :app:assembleDebug     # "MediaDir Dev" (separate app id, badged icon)
./gradlew :app:assembleRelease   # "MediaDir"
```

Configure the server URL in Settings. The ingest screen shows recent records, queue status, maintenance times, and storage problems. It supports one streamed upload at a time; keep the screen open until it finishes. Item pages list stored versions and offer confirmed item/version deletion. The item editor allows supported top-level media type changes.

## Verification

```sh
cd server
cargo test
```

The HTTP contract suite changes files and deletes media. Use a dedicated fresh instance with all three directories isolated from real data:

```sh
cd server
INGEST_DIR=/tmp/mediadir-contract/ingest \
MEDIA_DIR=/tmp/mediadir-contract/store \
ASSET_DIR=/tmp/mediadir-contract/assets \
DATA_DIR=/tmp/mediadir-contract/data \
BIND=127.0.0.1:8098 \
INGEST_SETTLE_SECS=1 INGEST_POLL_SECS=2 \
SIDECAR_INTERVAL=2 VERIFY_INTERVAL=3 BACKUP_INTERVAL=3 \
cargo run
```

In another terminal, from the project root:

```sh
python3 server/tests/contract_test.py http://127.0.0.1:8098
```

The suite uses fixtures in `testmedia/`. The directory paths above must be fresh for each run.

Device validation results are recorded in [docs/V3_VALIDATION.md](docs/V3_VALIDATION.md).
