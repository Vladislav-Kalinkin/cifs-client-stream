# cifs-client-stream

`cifs-client-stream` is a focused Rust fork of `re-gmbh/cifs-client`.

This fork is being developed as a **read-only SMB1-first streaming backend** for the future Apple TV media player **Apex**. It is not a full Samba replacement, not a write-capable NAS client, and not a finished media player. Its current goal is stable media browsing and sequential playback from legacy SMB1 storage, especially **AirPort Extreme + USB HDD** and similar old NAS/router setups.

## Current status

Implemented and tested:

- SMB1 connection, negotiation, authentication, mount and unmount.
- NTLM / NTLMv2 authentication.
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
  - conservative movie-folder detection;
  - explicit extra markers such as `--short`, `--deleted`, `--trailer`, `--featurette`.
- Playback-oriented streaming through `SmbMediaStream`.
- Pipelined SMB1 reads by default.
- Real smoke tests against AirPort Extreme.

The selected public playback path is:

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

The old experimental `ReadAhead` stream has been removed. Low-level streaming internals are private. The intended public path is `Cifs::open_media_stream*()` plus `SmbMediaStream`.

## Default SMB1 streaming behavior

The current default media stream profile is:

```text
initial buffer: 1 MiB
low watermark: 1 MiB
prefill target / high watermark: 2 MiB
read block: 256 KiB
stream capacity ceiling: 8 MiB
configured SMB chunk size: 65534 bytes
effective SMB1 chunk size: 65534 bytes
pipeline depth: 8
```

For SMB1/AirPort, large reads above roughly 64 KiB are not expected to work. The backend therefore clamps the effective SMB1 chunk size to `SMB_LEGACY_READ_MAX = 65534` and improves throughput through **pipelined 64 KiB reads**, not Large ReadX.

`SMB_CHUNK_SIZE` may be set to a larger value for smoke experiments, but the normal SMB1 path reports both configured and effective chunk sizes. For AirPort, the effective size remains 65534.

## Design goals

- Read-only access by default.
- No server-side metadata/artwork/history writes.
- Safe Rust first.
- Predictable memory usage.
- Sequential streaming optimized for real playback, not bulk copying.
- Strong compatibility with legacy SMB1 devices.
- AirPort Extreme remains the main hard legacy target.
- Future SMB2/SMB3 support should be added as separate backend(s), not by breaking this SMB1 path.

## Non-goals

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

Apex metadata, artwork, thumbnails, playback history and library cache should live above this backend, not inside the SMB share.

## Configuration model

The preferred test configuration is explicit host + share:

```text
SMB_HOST      server IP or hostname
SMB_SHARE     SMB share / disk name
SMB_USER      username
SMB_PASSWORD  password
SMB_DOMAIN    optional workgroup/domain
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

A server-root URI is supported, but a share name must still be supplied:

```sh
SMB_URI='smb://10.0.1.1/' \
SMB_SHARE='HARD' \
SMB_USER='user' \
SMB_PASSWORD='password' \
cargo run --bin smb_smoke
```

SMB share discovery is intentionally not required for the MVP. Some legacy SMB1 servers do not provide reliable share enumeration. Apex should expose the share/disk name as a user-editable field.

## Quick smoke test

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

Expected output should include:

```text
configured_chunk_size=65534
effective_chunk_size=65534
pipeline_depth=8
initial worker buffer: ...
read ... MiB/s
refill blocks: ...
cached blocks: ...
block latency: p95 ..., p99 ...
total including initial buffer: ...
```

## Local checks

Run before committing:

```sh
cargo fmt
cargo check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

## Security notes

SMB1 should be used only on a trusted local network. Do not expose SMB1 to the public internet.

This project exists to support legacy local media storage, not to recommend SMB1 for modern network deployments.

## License

MIT.
