# cifs-client-stream architecture

`cifs-client-stream` is a focused read-only SMB1 media backend.

It should stay small, conservative and backend-oriented. It is not the Apex UI, not a metadata engine, not a transcoder and not a general-purpose Samba replacement.

## Layer overview

```text
Network transport
  NetBios / TCP

SMB1 protocol
  negotiate
  session setup
  tree connect
  find first / find next
  open file read-only
  read at offset
  close
  tree disconnect

Media backend API
  Cifs
  DirectoryReader
  MediaEntry
  MediaPresentation
  SmbMediaStream

Streaming internals
  FileStream
  StreamingWorker
  StreamingWorkerState
  StreamingBuffer
  pending SMB replies by MID

Smoke tooling
  src/bin/smb_smoke.rs
```

## Public path

The intended public playback path is:

```text
Cifs::open_timeout
Cifs::mount
Cifs::open_dir_reader_timeout
DirectoryReader::next_media_entries_timeout
media_presentations / media_presentations_with_summaries
Cifs::open_media_stream_with_options
SmbMediaStream::fill_initial_buffer_timeout
SmbMediaStream::read_block_timeout
Cifs::close_media_stream
Cifs::umount
```

Low-level stream internals are intentionally private.

## Streaming model

`SmbMediaStream` uses `StreamingWorker`, which owns:

```text
FileStream
StreamingWorkerState
StreamingBuffer
```

The worker tracks:

```text
playback_position
source_position
file_size
buffered bytes
buffered chunk count
low watermark
high watermark
source EOF
```

Reads are performed as `ReadAndX` requests at explicit file offsets. This makes seeking possible by clearing the buffer and setting both playback and source positions to the new byte offset.

## Pipelined SMB reads

The backend supports pipelined source reads. Instead of sending one 64 KiB read and waiting before sending the next one, the worker can send several read requests in flight.

Current default:

```text
pipeline_depth = 8
effective SMB1 chunk size = 65534
```

To support pipelining safely, `Cifs` keeps pending SMB replies by multiplex id (`mid`). If replies arrive out of order, unrelated replies are stored and consumed later by the matching request.

## Chunk size

The stream distinguishes:

```text
configured_chunk_size
effective_chunk_size
```

For normal SMB1:

```text
effective_chunk_size = min(configured_chunk_size, SMB_LEGACY_READ_MAX)
SMB_LEGACY_READ_MAX = 65534
```

Large ReadX for non-AirPort SMB1 servers is a possible future experiment, but not the default path.

## Media classification

The backend maps directory entries to:

```rust
MediaKind::Folder
MediaKind::Audio
MediaKind::Video
```

It filters common system entries, hidden/system attributes, subtitles and non-media files.

Movie-folder detection is conservative:

```text
exactly one primary video -> movie-folder
multiple unmarked videos -> normal folder/collection
marked extras do not count as primary videos
```

## Future Apex layering

Recommended split:

```text
cifs-client-stream    SMB1 media backend
apex-remote-core      common abstraction for remote backends
apex-smb2             future SMB2/3 backend
apex-http/webdav      future HTTP(S)/WebDAV backend
apex-library          local SQLite index, metadata, artwork cache
Apex tvOS app         UI, Keychain, AVPlayer/ResourceLoader/bridge
```

The SMB backend should not own metadata, artwork or playback UI.
