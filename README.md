# cifs-client-stream

`cifs-client-stream` is a focused Rust fork of `re-gmbh/cifs-client`, developed as a read-only SMB1-first streaming backend for the future Apple TV media player **Apex**.

The project is intentionally not a full Samba replacement and not a generic write-capable SMB client. Its main goal is stable, predictable media playback from old SMB1/NAS-style storage, especially AirPort Extreme with an external USB HDD.

## Project goals

- Read-only SMB1 access for local media libraries.
- Primary target: Apple TV media playback through Apex.
- Primary server target: AirPort Extreme + external USB HDD.
- Safe Rust first: no `unsafe` unless a future platform bridge absolutely requires it.
- Sequential streaming optimized for real playback, not bulk file copying.
- Small memory footprint and predictable buffering.
- Clear path toward future SMB2/SMB3 backends without breaking the SMB1-first layer.

## Current status

Implemented and tested:

- SMB1 connection, negotiation, mount, unmount.
- NTLM authentication.
- Directory listing through `TRANS2_FIND_FIRST2` / `TRANS2_FIND_NEXT2`.
- Read-only file open/read/close.
- Timeout-aware operations.
- Error classification for network, timeout, protocol, server, auth, config and internal failures.
- Media-aware directory filtering:
  - keeps folders, audio and video;
  - filters subtitles and non-media files;
  - filters hidden/system entries;
  - sorts entries in media-friendly natural order.
- Playback-oriented streaming layer:
  - `SmbMediaStream`;
  - `SmbMediaStreamOptions`;
  - initial buffer;
  - low watermark;
  - prefill target;
  - block reads;
  - seek;
  - stream stats.
- Real SMB smoke test against AirPort Extreme.

The current default media stream profile is:

```text
initial buffer: 1 MiB
low watermark: 1 MiB
prefill target / high watermark: 2 MiB
read block: 256 KiB
read-ahead capacity ceiling: 8 MiB
SMB chunk size: 65534 bytes
```

This profile was selected after real tests on AirPort Extreme + USB HDD. It favors frequent moderate prefill reads instead of rare large reads, because that behaves better on this storage/server combination.

## What this fork intentionally does not do

This fork currently does not support:

- writing files to the SMB share;
- deleting or modifying server-side files;
- whole-file download APIs for large media;
- SMB notify / directory change watching;
- AV1 decoding;
- Dolby/DTS licensing logic;
- metadata writing back to the NAS;
- SMB2/SMB3 yet.

For Apex, the backend should read media safely and predictably. Metadata, thumbnails, playback history and cache should live in the app layer, not be written into the SMB share.

## Smoke test

Set the SMB URI, host, credentials and file path, then run:

```sh
SMB_URI='smb://10.0.1.1/HARD' \
SMB_HOST='10.0.1.1' \
SMB_USER='user' \
SMB_PASSWORD='password' \
SMB_READ_PATH='/path/to/movie.mkv' \
SMB_READ_BYTES=262144 \
SMB_READ_BLOCKS=256 \
SMB_READ_AHEAD_BYTES=8388608 \
SMB_CHUNK_SIZE=65534 \
SMB_WORKER_INITIAL_BUFFER_BYTES=1048576 \
SMB_TIMEOUT_MS=15000 \
cargo run --bin smb_smoke
```

The selected prefill profile can be tested with:

```sh
SMB_URI='smb://10.0.1.1/HARD' \
SMB_HOST='10.0.1.1' \
SMB_USER='user' \
SMB_PASSWORD='password' \
SMB_READ_PATH='/path/to/movie.mkv' \
SMB_READ_BYTES=262144 \
SMB_READ_BLOCKS=256 \
SMB_READ_AHEAD_BYTES=8388608 \
SMB_CHUNK_SIZE=65534 \
SMB_WORKER_INITIAL_BUFFER_BYTES=1048576 \
SMB_WORKER_PREFILL_HIGH=1 \
SMB_WORKER_PREFILL_TARGET_BYTES=2097152 \
SMB_TIMEOUT_MS=15000 \
cargo run --bin smb_smoke
```

Useful smoke variables:

```text
SMB_URI                         SMB URI, for example smb://10.0.1.1/HARD
SMB_HOST                        optional host override
SMB_USER                        username
SMB_PASSWORD                    password
SMB_DOMAIN                      optional domain/workgroup
SMB_READ_PATH                   file path inside the share
SMB_READ_BYTES                  block size requested by the smoke test
SMB_READ_BLOCKS                 number of blocks to read
SMB_READ_AHEAD_BYTES            stream capacity ceiling
SMB_CHUNK_SIZE                  SMB read chunk size
SMB_WORKER_INITIAL_BUFFER_BYTES initial startup buffer
SMB_WORKER_PREFILL_HIGH         enable prefill simulation
SMB_WORKER_PREFILL_TARGET_BYTES prefill/high-watermark target
SMB_PRINT_BLOCKS                print per-block diagnostics
SMB_PRINT_ENTRIES               print directory entries
SMB_TIMEOUT_MS                  operation timeout
```

## How to read smoke output

Important lines:

```text
initial worker buffer: ...
read ...
refill blocks: ...
cached blocks: ...
block latency: ...
prefill events: ...
total including initial buffer: ...
```

Interpretation:

- `initial worker buffer` measures startup buffering. If an external HDD is asleep, this may take several seconds. That is expected for AirPort Extreme + USB HDD.
- `cached blocks` should be high for smooth playback.
- `block latency` reflects how fast the playback-facing read path returns data.
- `prefill events` show network/storage reads done to refill the buffer.
- `total including initial buffer` includes startup delay.

A cold HDD wake-up can make the first buffer slow. This is not necessarily a stream algorithm problem. In Apex UI this should be presented as startup buffering / disk wake-up.

## Local checks

Run before committing:

```sh
cargo fmt
cargo check
cargo clippy --all-targets --all-features
cargo test
```

## Architecture

High-level layering:

```text
Cifs
  ├─ SMB1 session / mount / list / read
  ├─ DirectoryReader
  └─ SmbMediaStream
       └─ StreamingWorker
            └─ StreamingBuffer
```

`StreamingWorker` is the low-level buffering mechanism.

`SmbMediaStream` is the playback-oriented layer intended to evolve toward Apex and eventually FFI/Swift integration.

## Why SMB1 first

AirPort Extreme and many older NAS-like devices still expose SMB1. Apex needs to support this class of local home media setups cleanly.

SMB2/SMB3 should be added later as a separate backend/layer, not by breaking the SMB1 implementation.

## Roadmap

Near-term:

1. Finish cleanup around the selected `SmbMediaStream` path.
2. Remove remaining obsolete diagnostics and old public entry points.
3. Update smoke output after the final cleanup.
4. Add longer real-world stability tests.
5. Design an Apex-facing Rust API that can later be bridged to Swift.

Medium-term:

1. Add a background refill model.
2. Add a session/actor abstraction around SMB access.
3. Prepare FFI-friendly handles for Apex.
4. Integrate with Apple TV/tvOS app code.
5. Add seek/range behavior suitable for AVPlayer.

Long-term:

1. SMB2/SMB3 backend.
2. macOS/iPadOS/iOS clients sharing the same core.
3. Better metadata and artwork integration in the app layer.
4. Optional library index/cache outside the SMB share.

## Apex philosophy

Apex should be:

- free and open-source;
- local-first;
- privacy-friendly;
- no telemetry by default;
- no subscription for accessing the user's own media;
- honest about codec/platform limitations;
- focused on stable playback rather than claiming support for every format.

The backend should stay conservative and robust. Unsupported formats should fail clearly rather than pretending to support everything.
