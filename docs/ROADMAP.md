# cifs-client-stream roadmap

## Current milestone

The current milestone is a reliable read-only SMB1 backend smoke-testable by technical users.

Completed:

- SMB1 connect/auth/mount/list/read.
- DirectoryReader.
- Media entry classification.
- Conservative movie-folder detection.
- Explicit extras markers.
- ReadAhead removal.
- Low-level streaming internals hidden.
- Pipelined SMB1 reads.
- Effective chunk reporting.
- AirPort-tested default pipeline behavior.

## Near-term

### 1. Documentation

- Keep README current.
- Add `docs/SMOKE_TESTING.md`.
- Add `docs/TEST_REPORT_TEMPLATE.txt`.
- Add `docs/ARCHITECTURE.md`.
- Add `docs/ROADMAP.md`.

### 2. Public backend smoke test readiness

- Add friendlier smoke output.
- Add automatic report file generation.
- Add clear password redaction guidance.
- Add Windows-friendly command examples.
- Add a small CLI wizard.

### 3. Stability testing

Test matrix:

```text
AirPort Extreme + USB HDD
Time Capsule
Samba SMB1
Windows legacy SMB share
NAS/router SMB1
non-English filenames
large 4K WEB-DL files
folders with extras
movie collections
TV seasons
```

### 4. Performance

Current known AirPort strategy:

```text
effective chunk size: 65534
pipeline depth: 8
foreground refill target: low watermark
high watermark: reserved for prefill/background refill
```

Potential future performance work:

- Background refill.
- Adaptive pipeline fallback on timeout/errors.
- Better wall-clock source read metrics.
- Non-AirPort Large ReadX experiments.
- Release build comparisons.
- Longer warm/cold disk runs.

## Medium-term

### 1. Backend abstraction

Introduce a higher-level remote media abstraction before adding more protocols:

```text
list
stat / entry metadata
open byte stream
read range
seek
close
```

This should allow SMB1, SMB2/3, WebDAV and HTTP(S) backends to share upper layers.

### 2. SMB2/3

Do not rewrite SMB2/3 from scratch first.

Plan:

1. Evaluate existing Rust SMB2/3 libraries.
2. Build a separate `smb2_smoke`.
3. Test connect/auth/list/read/seek/Unicode paths.
4. Wrap it behind the same remote media abstraction.
5. Keep SMB1 backend stable and separate.

### 3. HTTP/WebDAV

For Apple platforms:

```text
HLS / direct playable URL -> AVFoundation
WebDAV / custom auth / virtual paths -> Rust backend or ResourceLoader bridge
SMB -> Rust backend
```

Avoid mixing transport, container parsing and decoding into one layer.

## Long-term

### 1. Apex integration

- Swift/tvOS bridge.
- Keychain password storage.
- Connection manager UI.
- Folder browser.
- Library/cache layer.
- TMDb matching.
- Poster/backdrop cache.
- Details page.
- Extras row.
- Continue watching.
- Seek integration.

### 2. Playback strategy

Prefer:

```text
AVPlayer where possible
AVAssetResourceLoader or local range bridge where needed
optional remux/probe helper later
```

Avoid making FFmpeg a required early tvOS dependency.
