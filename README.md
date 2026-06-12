# cifs-client-stream

`cifs-client-stream` is a focused Rust fork of `re-gmbh/cifs-client`.

The project is being developed as a **read-only SMB1-first streaming backend** for the future Apple TV media player **Apex**. It is not a general-purpose Samba replacement and not a write-capable SMB client. Its main purpose is stable media browsing and sequential playback from legacy local storage such as **AirPort Extreme + USB HDD**, Time Capsule-like setups, and older SMB1/NAS devices.

## Status

Current state:

- SMB1 connection, negotiation, authentication, mount and unmount.
- NTLM/NTLMv2 authentication.
- Read-only file open/read/close.
- Directory listing through `TRANS2_FIND_FIRST2` / `TRANS2_FIND_NEXT2`.
- Timeout-aware operations.
- Error classification for network, timeout, protocol, server, auth, config and internal failures.
- Media-aware browsing:
  - folders, audio and video entries;
  - natural sorting;
  - hidden/system entry filtering;
  - common system folder/file filtering;
  - subtitle and non-media filtering;
  - movie-folder detection;
  - conservative collection handling;
  - explicit extra flags such as `--short`, `--deleted`, `--trailer`, `--featurette`.
- Playback-oriented streaming through `SmbMediaStream`.
- Real SMB smoke tests against AirPort Extreme.

The public playback path is now:

```text
Cifs
  ├─ mount / list / media browsing
  └─ SmbMediaStream
       ├─ fill_initial_buffer
       ├─ read_block
       ├─ maybe_prefill
       ├─ seek
       └─ stats
```

The old experimental read-ahead stream has been removed. The selected API path is `SmbMediaStream`.

## Design goals

- Read-only access by default.
- No server-side writes for metadata, thumbnails, watch history or cache.
- Safe Rust first.
- Predictable memory usage.
- Sequential streaming optimized for media playback.
- Clear behavior on old SMB1 servers.
- Strong compatibility with AirPort Extreme-style setups.
- Future room for SMB2/SMB3 as separate backends, without breaking the SMB1-first layer.

## Non-goals for the current backend

This backend currently does **not** provide:

- server-side writes;
- file deletion or modification;
- whole-file download APIs for large media;
- SMB notify / directory change watching;
- SMB2/SMB3;
- AV1 decoding;
- Dolby/DTS licensing logic;
- TMDb metadata cache;
- Apple TV UI;
- TestFlight distribution;
- a finished media player.

Apex metadata, artwork, thumbnails, history and library cache should live in the app/library layer, not inside the SMB share.

## Configuration model

The preferred test configuration is now explicit host + share:

```text
SMB_HOST   = server IP or hostname
SMB_SHARE  = SMB share / disk name
SMB_USER   = username
SMB_PASSWORD = password
SMB_DOMAIN = optional workgroup/domain
```

Example:

```sh
SMB_HOST='10.0.1.1' \
SMB_SHARE='HARD' \
SMB_USER='user' \
SMB_PASSWORD='password' \
cargo run --bin smb_smoke
```

`SMB_URI` is optional and can still be used as a shortcut:

```sh
SMB_URI='smb://10.0.1.1/HARD' \
SMB_USER='user' \
SMB_PASSWORD='password' \
cargo run --bin smb_smoke
```

A server-root URI is also supported, but a share name must still be provided:

```sh
SMB_URI='smb://10.0.1.1/' \
SMB_SHARE='HARD' \
SMB_USER='user' \
SMB_PASSWORD='password' \
cargo run --bin smb_smoke
```

SMB share discovery is intentionally not required for the MVP. Some legacy SMB1 servers, including AirPort-style setups, may not provide reliable share enumeration. Apex should expose the share/disk name as a user-editable field.

## Smoke tests

### 1. Root listing

```sh
SMB_HOST='10.0.1.1' \
SMB_SHARE='HARD' \
SMB_USER='user' \
SMB_PASSWORD='password' \
SMB_PRINT_ENTRIES=1 \
SMB_TIMEOUT_MS=15000 \
cargo run --bin smb_smoke
```

### 2. Nested folder listing

```sh
SMB_HOST='10.0.1.1' \
SMB_SHARE='HARD' \
SMB_USER='user' \
SMB_PASSWORD='password' \
SMB_LIST_PATH='/Movies' \
SMB_PRINT_ENTRIES=1 \
SMB_TIMEOUT_MS=15000 \
cargo run --bin smb_smoke
```

### 3. Movie-folder scan

```sh
SMB_HOST='10.0.1.1' \
SMB_SHARE='HARD' \
SMB_USER='user' \
SMB_PASSWORD='password' \
SMB_LIST_PATH='/Movies' \
SMB_PRINT_ENTRIES=1 \
SMB_SCAN_FOLDER_SUMMARIES=1 \
SMB_TIMEOUT_MS=15000 \
cargo run --bin smb_smoke
```

This checks whether folders can be safely classified as:

```text
folder
movie-folder
playable-file
```

The detection is conservative. A folder with multiple unflagged primary videos remains a normal folder. A folder with one primary video and flagged extras can become a movie-folder.

### 4. Sequential read smoke

```sh
SMB_HOST='10.0.1.1' \
SMB_SHARE='HARD' \
SMB_USER='user' \
SMB_PASSWORD='password' \
SMB_READ_PATH='/path/to/movie.mkv' \
SMB_READ_BYTES=262144 \
SMB_READ_BLOCKS=256 \
SMB_TIMEOUT_MS=15000 \
cargo run --bin smb_smoke
```

### 5. Prefill simulation

```sh
SMB_HOST='10.0.1.1' \
SMB_SHARE='HARD' \
SMB_USER='user' \
SMB_PASSWORD='password' \
SMB_READ_PATH='/path/to/movie.mkv' \
SMB_READ_BYTES=262144 \
SMB_READ_BLOCKS=256 \
SMB_WORKER_PREFILL_HIGH=1 \
SMB_WORKER_PREFILL_TARGET_BYTES=2097152 \
SMB_TIMEOUT_MS=15000 \
cargo run --bin smb_smoke
```

## Useful smoke variables

```text
SMB_HOST                         server IP or hostname
SMB_SHARE                        SMB share / disk name
SMB_VOLUME_NAME                  alias for SMB_SHARE
SMB_DISK_NAME                    alias for SMB_SHARE
SMB_URI                          optional shortcut URI
SMB_USER                         username
SMB_PASSWORD                     password
SMB_DOMAIN                       optional workgroup/domain
SMB_LIST_PATH                    directory path inside the share
SMB_READ_PATH                    file path inside the share
SMB_READ_BYTES                   read block size, default 256 KiB
SMB_READ_BLOCKS                  number of blocks to read
SMB_READ_AHEAD_BYTES             stream capacity ceiling, default 8 MiB
SMB_CHUNK_SIZE                   SMB read chunk size, default 65534
SMB_WORKER_INITIAL_BUFFER_BYTES  startup buffer, default 1 MiB
SMB_WORKER_PREFILL_HIGH          foreground prefill simulation
SMB_WORKER_PREFILL_TARGET_BYTES  high-watermark/prefill target
SMB_LOW_WATERMARK_BYTES          low watermark override
SMB_HIGH_WATERMARK_BYTES         high watermark override
SMB_PRINT_ENTRIES                print directory entries
SMB_PRINT_BLOCKS                 print per-block read diagnostics
SMB_SCAN_FOLDER_SUMMARIES        scan child folders for movie-folder detection
SMB_TIMEOUT_MS                   operation timeout
```

## Current streaming profile

The current default media stream profile is:

```text
initial buffer: 1 MiB
low watermark: 1 MiB
prefill target / high watermark: 2 MiB
read block: 256 KiB
stream capacity ceiling: 8 MiB
SMB chunk size: 65534 bytes
```

This profile was selected from real tests against AirPort Extreme + USB HDD. On this class of hardware, moderate frequent reads are usually better than rare very large foreground prefill reads.

Cold HDD wake-up can make the first buffer slow. That is expected behavior for sleeping USB HDDs and should be handled in Apex UI as startup buffering / disk wake-up, not as a stream failure.

## Media classification

### Media entries

The backend classifies entries as:

```rust
MediaKind::Folder
MediaKind::Audio
MediaKind::Video
```

Common system entries are ignored, including examples such as:

```text
.DS_Store
.Trashes
.Spotlight-V100
.fseventsd
@eaDir
$RECYCLE.BIN
System Volume Information
Temporary Items
Network Trash Folder
TheVolumeSettingsFolder
```

Subtitles are filtered out of the main media entry list for now. They may be handled later by a subtitle-aware layer.

### Movie folders

A folder becomes a `movie-folder` only when the child scan finds exactly one primary video.

Examples:

```text
Movie.mkv
Trailer --trailer.mkv
```

This can become a movie-folder.

```text
Movie 1.mkv
Movie 2.mkv
```

This remains a normal folder/collection.

This conservative behavior prevents collections, franchises and TV seasons from being collapsed into a single movie.

### Explicit extra flags

The backend recognizes optional filename markers:

```text
--extra
--bonus
--deleted
--deleted-scene
--deleted-scenes
--short
--trailer
--teaser
--featurette
--behind
--behind-the-scenes
--interview
--sample
```

Example:

```text
Ice Age.mkv
Ice.Age - Gone Nutty --short.mkv
```

This allows the folder to be treated as one main movie plus one extra.

Without the explicit flag, a second unmarked video is treated conservatively as another primary video.

## Local checks

Run before committing:

```sh
cargo fmt
cargo check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

`cargo fmt --check` is not necessary in this workflow because formatting is applied directly.

## Public testing status

This is not a public Apex beta and not a finished Apple TV media player.

The current suitable public-facing test is a technical backend smoke test:

```text
connect
mount share
list root folder
list nested folder
scan movie folders
read blocks from a large video file
report timings/errors
```

Good early testers:

- AirPort Extreme + USB HDD users;
- Time Capsule users;
- old SMB1/NAS users;
- users with non-English filenames;
- users with Blu-ray-style folders and extras;
- users with large 4K WEB-DL files.

## Planned next steps

Near-term backend work:

1. Improve smoke output for public test reports.
2. Add an interactive CLI wizard for non-developer testers.
3. Add a Windows-friendly CLI build.
4. Add `docs/SMOKE_TESTING.md`.
5. Add `docs/TEST_REPORT_TEMPLATE.md`.
6. Add longer real-world stability tests.
7. Add a future background refill model.
8. Design the Apex-facing library/cache layer.

Future Apex layers:

```text
cifs-client-stream    SMB/media backend
apex-library          local SQLite index, metadata matching, artwork cache
Apex tvOS app         UI, playback, settings, local-first media experience
```

## Security notes

SMB1 should be used only on a trusted local network.

Do not expose SMB1 to the public internet. This project exists to support legacy local media storage, not to recommend SMB1 for modern network deployments.

## License

MIT.
