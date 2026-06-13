# cifs-client-stream

`cifs-client-stream` is a focused source-available Rust fork of the original
MIT-licensed `cifs-client` project.

This fork is being developed as a **read-only SMB1-first media streaming
backend** for legacy local storage and the future Apple TV media player
**Apex**.

It is not a general-purpose Samba replacement, not a write-capable SMB client,
and not a finished media player. Its current purpose is stable media browsing
and sequential playback from legacy SMB1 storage such as **AirPort Extreme +
USB HDD**, Time Capsule-like setups, and older SMB1/NAS devices.

## License and usage notice

This repository is **source-available, not OSI-approved open source**.

Commercial use, commercial distribution, integration into a paid product or
service, or hosted commercial use requires separate written permission from
Vladislav Kalinkin.

Original upstream portions derived from `cifs-client` remain available under
their original MIT license. The original upstream MIT license is preserved in:

```text
LICENSES/MIT-ORIGINAL.txt
```

Project-specific modifications, documentation, smoke-test tooling, media
classification logic, streaming changes, and other original work added by
Vladislav Kalinkin are licensed under the project license in:

```text
LICENSE
```

See also:

```text
NOTICE
```

## Current status

Current backend capabilities:

- SMB1 connection, negotiation, authentication, mount and unmount.
- NTLM / NTLMv2 authentication.
- Read-only file open/read/close.
- Directory listing through `TRANS2_FIND_FIRST2` / `TRANS2_FIND_NEXT2`.
- Timeout-aware operations.
- Error classification for network, timeout, protocol, server, auth, config
  and internal failures.
- Media-aware browsing: folders, audio/video entries, natural sorting, hidden
  and system filtering, conservative movie-folder detection, explicit extras.
- Playback-oriented streaming through `SmbMediaStream`.
- Pipelined SMB1 reads by default.
- Media stream seek support.
- Sequential, stress, long-read, listing, movie-folder and seek smoke tests.
- Copyable smoke-test report summaries.
- Optional smoke summary saving through `SMB_REPORT_PATH`.
- Real smoke tests against AirPort Extreme + USB HDD.

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

## Default SMB1 streaming behavior

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

For AirPort Extreme and similar legacy SMB1 servers, large reads above roughly
64 KiB should not be expected to work. Throughput is improved through
**pipelined 64 KiB reads**, not through Large ReadX.

## Current AirPort baseline

Measured AirPort Extreme + USB HDD behavior in release smoke tests:

```text
short 64 MiB reads: often 7-8+ MiB/s
long 512 MiB / 1 GiB sustained reads: around 5.5-6 MiB/s
aggressive 120 Mbps 4K stress-read: passed as a read test, but above realistic playback bandwidth
effective SMB1 chunk size: 65534
pipeline depth: 8
```

A 120 Mbps video sample is useful as a stress test, but should not be treated
as a guaranteed real-time AirPort playback target.

## Design goals

- Read-only access by default.
- No server-side writes for metadata, thumbnails, watch history or cache.
- Safe Rust first.
- Predictable memory usage.
- Sequential streaming optimized for playback, not bulk copying.
- Strong compatibility with AirPort Extreme-style setups.
- Future room for SMB2/SMB3 as separate backend(s), without breaking SMB1.
- Privacy-friendly testing.

## Non-goals for the current backend

This backend currently does **not** provide server-side writes, file deletion,
whole-file download APIs for large media, SMB notify, SMB2/SMB3, AV1 decoding,
Dolby/DTS licensing logic, TMDb metadata cache, Apple TV UI, TestFlight
distribution, or a finished media player.

## Configuration model

```text
SMB_HOST      server IP or hostname
SMB_SHARE     SMB share / disk name
SMB_USER      username
SMB_PASSWORD  password
SMB_DOMAIN    optional workgroup/domain
```

Example:

```sh
SMB_HOST='10.0.1.1' SMB_SHARE='HARD' SMB_USER='user' SMB_PASSWORD='REDACTED' cargo run --bin smb_smoke
```

`SMB_URI` is optional and can still be used as a shortcut.

## Smoke tests

### Root listing

```sh
SMB_HOST='10.0.1.1' SMB_SHARE='HARD' SMB_USER='user' SMB_PASSWORD='REDACTED' SMB_PRINT_ENTRIES=1 SMB_TIMEOUT_MS=15000 cargo run --release --bin smb_smoke
```

### Sequential read with report file

```sh
SMB_HOST='10.0.1.1' SMB_SHARE='HARD' SMB_USER='user' SMB_PASSWORD='REDACTED' SMB_READ_PATH='/path/to/movie.mkv' SMB_READ_BYTES=262144 SMB_READ_BLOCKS=256 SMB_REPORT_PATH='smoke-report-airport.txt' SMB_TIMEOUT_MS=15000 cargo run --release --bin smb_smoke
```

### Seek smoke

```sh
SMB_HOST='10.0.1.1' SMB_SHARE='HARD' SMB_USER='user' SMB_PASSWORD='REDACTED' SMB_READ_PATH='/path/to/movie.mkv' SMB_READ_BYTES=262144 SMB_SEEK_TEST=1 SMB_REPORT_PATH='smoke-report-airport-seek.txt' SMB_TIMEOUT_MS=15000 cargo run --release --bin smb_smoke
```

The seek smoke test checks reads from the start, quarter, half, near-end and
backward positions of a media stream.

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
SMB_CHUNK_SIZE                   configured SMB read chunk size
SMB_WORKER_INITIAL_BUFFER_BYTES  startup buffer, default 1 MiB
SMB_WORKER_PREFILL_HIGH          foreground prefill simulation
SMB_WORKER_PREFILL_TARGET_BYTES  high-watermark / prefill target
SMB_LOW_WATERMARK_BYTES          low watermark override
SMB_HIGH_WATERMARK_BYTES         high watermark override
SMB_PIPELINE_DEPTH               in-flight SMB reads, default 8
SMB_PRINT_ENTRIES                print directory entries
SMB_PRINT_BLOCKS                 print per-block read diagnostics
SMB_SCAN_FOLDER_SUMMARIES        scan child folders for movie-folder detection
SMB_SEEK_TEST                    run seek/read validation instead of sequential read
SMB_REPORT_PATH                  optional path to save the final smoke summary
SMB_TIMEOUT_MS                   operation timeout
```

## Public testing status

This is not a public Apex beta and not a finished Apple TV media player.

Official technical smoke test reports are accepted through GitHub Issues. A
GitHub account is required to submit an issue or comment. Do not include real
passwords, public IP addresses, personal details, private media names, or
anything you do not want to share publicly.

## Documentation

```text
docs/SMOKE_TESTING.md
docs/TEST_REPORT_TEMPLATE.txt
docs/ARCHITECTURE.md
docs/ROADMAP.md
```

## Local checks

```sh
cargo fmt
cargo check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

## Security notes

SMB1 should be used only on a trusted local network. Do not expose SMB1 to the
public internet.

## License

Source-available, non-commercial. See:

```text
LICENSE
NOTICE
LICENSES/MIT-ORIGINAL.txt
```
