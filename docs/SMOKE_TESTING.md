# Smoke testing cifs-client-stream

This document describes how to run technical backend smoke tests for
`cifs-client-stream`.

These tests are **not** a public Apex beta. They validate the SMB1 backend:
connect, mount, list directories, classify media entries, scan movie folders,
read blocks from large files, and seek within media streams.

## Requirements

- Rust toolchain installed.
- Access to an SMB1 share.
- Hostname/IP, share name, username and password.
- A large video file for sequential read and seek tests.
- A terminal.

## Environment variables

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
SMB_REPORT_PATH                  optional path to save the final smoke report summary
SMB_READ_BYTES                   read block size, default 256 KiB
SMB_READ_BLOCKS                  number of blocks to read
SMB_READ_AHEAD_BYTES             stream capacity ceiling, default 8 MiB
SMB_CHUNK_SIZE                   configured stream chunk size
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
SMB_TIMEOUT_MS                   operation timeout
```

## Root listing

```sh
SMB_HOST='10.0.1.1' SMB_SHARE='HARD' SMB_USER='user' SMB_PASSWORD='password' SMB_PRINT_ENTRIES=1 SMB_TIMEOUT_MS=15000 cargo run --release --bin smb_smoke
```

## Nested folder listing

```sh
SMB_HOST='10.0.1.1' SMB_SHARE='HARD' SMB_USER='user' SMB_PASSWORD='password' SMB_LIST_PATH='/Movies' SMB_PRINT_ENTRIES=1 SMB_TIMEOUT_MS=15000 cargo run --release --bin smb_smoke
```

## Movie-folder scan

```sh
SMB_HOST='10.0.1.1' SMB_SHARE='HARD' SMB_USER='user' SMB_PASSWORD='password' SMB_LIST_PATH='/Movies' SMB_PRINT_ENTRIES=1 SMB_SCAN_FOLDER_SUMMARIES=1 SMB_TIMEOUT_MS=15000 cargo run --release --bin smb_smoke
```

Movie-folder detection is conservative. A folder with several unmarked videos
remains a folder/collection. A folder with one main video and flagged extras
may become `movie-folder`.

## Sequential read test

```sh
SMB_HOST='10.0.1.1' SMB_SHARE='HARD' SMB_USER='user' SMB_PASSWORD='password' SMB_READ_PATH='/path/to/movie.mkv' SMB_READ_BYTES=262144 SMB_READ_BLOCKS=256 SMB_REPORT_PATH='smoke-report-airport.txt' SMB_TIMEOUT_MS=15000 cargo run --release --bin smb_smoke
```

Recommended release checks:

```text
SMB_READ_BLOCKS=256     64 MiB short read
SMB_READ_BLOCKS=2048    512 MiB long read
SMB_READ_BLOCKS=4096    1 GiB long read
```

## Seek test

```sh
SMB_HOST='10.0.1.1' SMB_SHARE='HARD' SMB_USER='user' SMB_PASSWORD='password' SMB_READ_PATH='/path/to/movie.mkv' SMB_READ_BYTES=262144 SMB_SEEK_TEST=1 SMB_REPORT_PATH='smoke-report-airport-seek.txt' SMB_TIMEOUT_MS=15000 cargo run --release --bin smb_smoke
```

The seek test performs read checks at:

```text
start
quarter
half
near_end
back_to_10_percent
```

A successful seek test means:

```text
actual_offset matches requested_offset
read_len is non-zero
near_end does not read beyond EOF
backward seek clears the old buffer and reads from the new position
```

## Configured vs effective chunk size

For AirPort / normal SMB1 mode, effective chunk size is clamped to 65534 bytes.
If `SMB_CHUNK_SIZE=131072` is set but `effective_chunk_size=65534`, the backend
is protecting the normal SMB1 path and still using legacy-safe 64 KiB reads.

## What to report

Please include OS, SMB server/device model, network type, disk type if known,
the exact command used, full smoke output, and whether the run was cold HDD
wake-up or warm disk.
