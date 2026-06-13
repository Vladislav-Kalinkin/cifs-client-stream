# cifs-client-stream roadmap

## Current milestone

The current milestone is a reliable read-only SMB1 backend that is ready to be
used as the foundation for an Apex playback prototype.

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
- Copyable smoke report summaries.
- Optional smoke report file saving.
- Release smoke tests.
- Long 512 MiB / 1 GiB smoke tests.
- Aggressive 4K stress-read smoke test.
- Seek smoke test.
- Basic error scenario checks.

## Near-term

### 1. Freeze SMB1 as preliminary ready

Before moving fully to Apex prototype work:

```text
cargo fmt
cargo check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

Then keep the current SMB1 backend stable unless Apex integration reveals a real
playback problem.

### 2. Playback readiness

Potential future SMB/backend work should be driven by Apex needs:

- Background refill.
- Better playback-style long-run diagnostics.
- More precise buffering metrics.
- Optional adaptive fallback if a server dislikes pipeline depth 8.
- Optional non-AirPort Large ReadX experiment.

## Medium-term

Introduce a higher-level remote media abstraction before adding more protocols:

```text
list
stat / entry metadata
open byte stream
read range
seek
close
```

Then evaluate SMB2/3 and WebDAV as separate backends.

## Long-term

Apex integration:

- Swift/tvOS bridge.
- Keychain password storage.
- Connection manager UI.
- Folder browser.
- Library/cache layer.
- Details page.
- Extras row.
- Continue watching.
- Seek integration.
