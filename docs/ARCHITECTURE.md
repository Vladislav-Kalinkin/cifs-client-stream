# cifs-client-stream architecture

`cifs-client-stream` is a focused read-only SMB1 media backend.

It should stay small, conservative and backend-oriented. It is not the Apex UI,
not a metadata engine, not a transcoder and not a general-purpose Samba
replacement.

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

```text
Cifs::open_timeout
Cifs::mount
Cifs::open_dir_reader_timeout
DirectoryReader::next_media_entries_timeout
media_presentations / media_presentations_with_summaries
Cifs::open_media_stream_with_options
SmbMediaStream::fill_initial_buffer_timeout
SmbMediaStream::read_block_timeout
SmbMediaStream::seek
Cifs::close_media_stream
Cifs::umount
```

## Seek model

A seek operation clears the local streaming buffer, sets playback and source
positions to the requested offset, and continues future reads from that new
source position. The smoke tool validates this through `SMB_SEEK_TEST=1`.

## Pipelined SMB reads

The backend supports pipelined source reads. Current default:

```text
pipeline_depth = 8
effective SMB1 chunk size = 65534
```

`Cifs` keeps pending SMB replies by multiplex id (`mid`) so out-of-order replies
can be matched safely.

## Chunk size

For normal SMB1:

```text
effective_chunk_size = min(configured_chunk_size, SMB_LEGACY_READ_MAX)
SMB_LEGACY_READ_MAX = 65534
```

Large ReadX for non-AirPort SMB1 servers is a possible future experiment, but
not the default path.

## Future Apex layering

```text
cifs-client-stream    SMB1 media backend
apex-remote-core      common abstraction for remote backends
apex-smb2             future SMB2/3 backend
apex-http/webdav      future HTTP(S)/WebDAV backend
apex-library          local SQLite index, metadata, artwork cache
Apex tvOS app         UI, Keychain, AVPlayer/ResourceLoader/bridge
```
