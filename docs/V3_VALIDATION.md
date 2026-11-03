# V3 validation

## Android device checks — 2026-11-01

Tested the debug APK on the connected Xiaomi M2102J20SI (Android 16) against the Rust v3 server on port 8096. Tests used generated H.264/AAC clips at 640×360 and 320×180. Previous v2 data was preserved; v3 uses `server/data-v3`, `server/store-v3`, and `server/ingest-v3`.

| Flow | Result |
|---|---|
| Existing server URL → Save & test | Connected, API v3 |
| Ingest status and forced poll | Active watcher and queue state displayed; poll accepted |
| Android document-picker upload | Streamed file ingested; completed record shown |
| Upload same content again | Duplicate recorded; no extra version |
| Drop a second encoding into ingest directory | Settled and attached as another version of the movie |
| Edit title and media kind | Saved through the app; API confirmed values and locks |
| Select media kind by label | Passed after making each row selectable |
| Return Home after kind change | Correct virtual library displayed after refresh fix |
| Return to old library after kind change | Item removed from old library after refresh fix |
| Search | Generated item found by title |
| Playback | Test video rendered on device |
| Select lower-resolution version | 180p version selected through player quality dialog |
| Pause with lower-resolution version selected | Server extracted a frame from the 360p version; frame request and cache output confirmed |
| Cancel item deletion | Both versions preserved |
| Delete one version | Other version retained; deleted blob absent from store |
| Delete final version | Item removed; returned Home; both stored blobs absent |
| Delete whole item | Item, virtual library, and stored blob removed |
| Poll after deletion | Remembered ingest original was not re-imported |

The final APK includes the selectable media-type rows and refresh-on-return fixes for Home and library screens. `:app:assembleDebug` passed and the APK was installed successfully. No AndroidRuntime fatal errors were reported for the final app process during these checks.

Generated test media was removed from the phone, ingest directory, and managed store. The phone's automatic rotation setting was restored. The existing server URL was preserved. The backend remains running with the separate v3 directories; previous v2 media has not been imported into them.

## Server checks from the earlier isolated runs

- `cargo test`: 27 passed, 0 failed.
- `server/tests/contract_test.py`: 24 passed, 0 failed. Covered ingest, classification, storage layout and permissions, virtual libraries, HTTP Range, thumbnails, still frames, metadata edits, custom fields, people, search, queues, uploads, duplicate handling, deletion, sidecars, backups, and integrity checks.

See the root README for commands to reproduce the server checks against a fresh disposable instance. The connected-device run exercised the core foreground workflows with the generated clips; it was not an exhaustive codec or device compatibility run.
