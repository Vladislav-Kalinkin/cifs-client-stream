# Smoke testing cifs-client-stream

This document describes how to run technical backend smoke tests for `cifs-client-stream`.

These tests are **not** a public Apex beta. They validate the SMB1 backend: connect, mount, list directories, classify media entries, scan movie folders, and read blocks from large files.

## Requirements

- Rust toolchain installed.
- Access to an SMB1 share.
- Hostname/IP, share name, username and password.
- A large video file for sequential read tests.
- A terminal.

Useful targets:

```text
AirPort Extreme + USB HDD
Time Capsule
old NAS
Samba server with SMB1 enabled
Windows legacy share
router/NAS with SMB1/CIFS support
```

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
SMB_TIMEOUT_MS                   operation timeout
```

## 1. Root listing

```sh
SMB_HOST='10.0.1.1' \
SMB_SHARE='HARD' \
SMB_USER='user' \
SMB_PASSWORD='password' \
SMB_PRINT_ENTRIES=1 \
SMB_TIMEOUT_MS=15000 \
cargo run --bin smb_smoke
```

Expected:

```text
using SMB share from environment: HARD
connected to \\10.0.1.1\HARD
listed pattern: *
media entries in first batch: ...
```

## 2. Nested folder listing

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

Use a real folder path inside your share.

## 3. Movie-folder scan

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

The output may classify entries as:

```text
folder
movie-folder
playable-file
```

Movie-folder detection is conservative. A folder with several unmarked videos remains a folder/collection. A folder with one main video and flagged extras may become `movie-folder`.

Recognized extra markers include:

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

## 4. Sequential read test

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

For a longer stability test:

```text
SMB_READ_BLOCKS=2048
```

This reads 512 MiB when `SMB_READ_BYTES=262144`.

## 5. Pipeline override tests

Default pipeline depth is 8. To compare behavior:

```text
SMB_PIPELINE_DEPTH=1
SMB_PIPELINE_DEPTH=2
SMB_PIPELINE_DEPTH=4
SMB_PIPELINE_DEPTH=8
```

For AirPort Extreme, current testing indicates:

```text
pipeline_depth=8 is the best current candidate
pipeline_depth=16 is too aggressive
```

## 6. Configured vs effective chunk size

The smoke output prints both:

```text
configured_chunk_size=...
effective_chunk_size=...
```

For AirPort / normal SMB1 mode, effective chunk size is clamped to:

```text
65534 bytes
```

Example:

```sh
SMB_CHUNK_SIZE=131072
```

may still report:

```text
configured_chunk_size=131072
effective_chunk_size=65534
```

That means the backend is protecting the normal SMB1 path and still using legacy-safe 64 KiB reads.

## 7. How to read performance output

Important lines:

```text
initial worker buffer: ...
initial source reads: calls=... avg_size=... avg_latency=... summed_source_time=...
read ... MiB/s
refill blocks: ... p95 ... p99 ...
cached blocks: ...
block latency: p95 ... p99 ...
stream source reads: calls=... avg_size=...
total including initial buffer: ...
```

Meaning:

- `read ... MiB/s`: read phase wall-clock throughput.
- `total including initial buffer`: throughput including startup buffering.
- `refill blocks`: blocks that required source reads.
- `cached blocks`: blocks served from the local buffer.
- `p95` / `p99`: latency tail. Very important for playback.
- `avg_size`: average internal SMB read size.
- `summed_source_time`: sum of individual read latencies. With pipelining this is **not wall-clock time**.

For playback, stable p95/p99 is often more important than a single high average speed.

## 8. What to report

Please include:

- OS and machine running the test.
- SMB server/device model.
- Wired or Wi-Fi.
- HDD/SSD if known.
- Exact command used.
- Full smoke output.
- Whether filenames contain non-English characters.
- Whether the folder contains movie extras.
- Whether the run was cold HDD wake-up or warm disk.
