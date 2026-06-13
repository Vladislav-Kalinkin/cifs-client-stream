mod error;
#[cfg(feature = "ffi")]
mod ffi;
mod netbios;
mod ntlm;
mod smb;
mod utils;
pub mod win;

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::io::SeekFrom;
use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use lazy_static::lazy_static;
use regex::Regex;

use crate::netbios::NetBios;
use crate::smb::info::{Cmd, Flags2, Info, Status};
use crate::smb::{Capabilities, DirInfo, SMB_LEGACY_READ_MAX, msg, reply, trans2};
use crate::utils::sanitize_path;
use crate::win::{ExtFileAttr, FileAttr, NTStatus};

const DEFAULT_READ_AHEAD_CAPACITY: usize = 8 * 1024 * 1024;
const DEFAULT_STREAM_CHUNK_SIZE: u32 = SMB_LEGACY_READ_MAX;
const DEFAULT_MEDIA_INITIAL_BUFFER_SIZE: usize = 1024 * 1024;
const DEFAULT_MEDIA_PREFILL_TARGET_SIZE: usize = 2 * 1024 * 1024;
const DEFAULT_SMB1_PIPELINE_DEPTH: usize = 8;

const AUDIO_EXTENSIONS: &[&str] = &[
    "aac", "aiff", "alac", "flac", "m4a", "mp3", "ogg", "opus", "wav",
];
const SUBTITLE_EXTENSIONS: &[&str] = &[
    "ass", "idx", "smi", "srt", "ssa", "sub", "sup", "ttml", "vtt",
];
const VIDEO_EXTENSIONS: &[&str] = &[
    "avi", "divx", "m2ts", "m4v", "mkv", "mov", "mp4", "mpeg", "mpg", "mts", "ts", "webm", "wmv",
];

pub use crate::smb::reply::{Handle, Share};
pub use error::{Error, ErrorKind};
pub use netbios::Error as NetbiosError;
pub use ntlm::Auth;
pub use ntlm::Error as NtlmError;
pub use smb::Error as SmbError;

#[derive(Debug)]
struct PendingReply {
    info: Info,
    body: Bytes,
}

#[derive(Debug)]
pub struct Cifs {
    netbios: NetBios,
    auth: Auth,

    max_smb_size: usize,
    use_unicode: bool,
    uid: u16,
    mid: u16,
    io_stats: CifsIoStats,
    pending_replies: HashMap<u16, PendingReply>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CifsIoStats {
    pub read_at_calls: u64,
    pub read_at_bytes: u64,
    pub read_at_elapsed: Duration,
}

impl CifsIoStats {
    pub fn average_read_size(&self) -> u64 {
        self.read_at_bytes
            .checked_div(self.read_at_calls)
            .unwrap_or(0)
    }

    pub fn average_read_latency(&self) -> Duration {
        let Some(calls) = u32::try_from(self.read_at_calls)
            .ok()
            .filter(|calls| *calls > 0)
        else {
            return Duration::ZERO;
        };

        self.read_at_elapsed / calls
    }

    pub fn read_throughput_mib_per_second(&self) -> f64 {
        if self.read_at_elapsed.is_zero() {
            0.0
        } else {
            (self.read_at_bytes as f64 / (1024.0 * 1024.0)) / self.read_at_elapsed.as_secs_f64()
        }
    }

    fn record_read(&mut self, bytes: usize, elapsed: Duration) {
        self.read_at_calls += 1;
        self.read_at_bytes += bytes as u64;
        self.read_at_elapsed += elapsed;
    }
}

#[derive(Debug)]
struct FileStream {
    handle: Handle,
    position: u64,
}

#[derive(Debug)]
pub struct DirectoryReader {
    tid: u16,
    sid: u16,
    pending: Option<Vec<DirInfo>>,
    last_file: Option<String>,
    end: bool,
}

#[derive(Debug)]
struct StreamingWorker {
    stream: FileStream,
    state: StreamingWorkerState,
}

#[derive(Debug)]
pub struct SmbMediaStream {
    worker: StreamingWorker,
    options: SmbMediaStreamOptions,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    Folder,
    Audio,
    Video,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaExtraKind {
    Extra,
    Bonus,
    DeletedScene,
    Short,
    Trailer,
    Teaser,
    Featurette,
    BehindTheScenes,
    Interview,
    Sample,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaEntry {
    pub name: String,
    pub size: u64,
    pub kind: MediaKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaFolderSummary {
    pub main_video: Option<usize>,
    pub primary_videos: Vec<usize>,
    pub extras: Vec<usize>,
    pub audio_tracks: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MediaPresentation {
    Folder {
        index: usize,
    },
    MovieFolder {
        index: usize,
        summary: MediaFolderSummary,
    },
    PlayableFile {
        index: usize,
    },
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct DirectorySortKey {
    file: bool,
    name: NaturalNameKey,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct NaturalNameKey(Vec<NaturalToken>);

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum NaturalToken {
    Number {
        significant_len: usize,
        significant: String,
        raw_len: usize,
    },
    Text(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamOptions {
    pub read_ahead_capacity: usize,
    pub chunk_size: u32,
}

impl Default for StreamOptions {
    fn default() -> Self {
        Self {
            read_ahead_capacity: DEFAULT_READ_AHEAD_CAPACITY,
            chunk_size: DEFAULT_STREAM_CHUNK_SIZE,
        }
    }
}

impl StreamOptions {
    pub fn new(read_ahead_capacity: usize, chunk_size: u32) -> Result<Self, Error> {
        let options = Self {
            read_ahead_capacity,
            chunk_size,
        };
        options.validate()?;
        Ok(options)
    }

    pub fn validate(&self) -> Result<(), Error> {
        if self.chunk_size > 1024 * 1024 {
            return Err(Error::InvalidConfig(
                "stream chunk size must not exceed 1 MiB".to_owned(),
            ));
        }

        if self.read_ahead_capacity == 0 {
            return Err(Error::InvalidConfig(
                "read-ahead capacity must be greater than zero".to_owned(),
            ));
        }
        if self.chunk_size == 0 {
            return Err(Error::InvalidConfig(
                "stream chunk size must be greater than zero".to_owned(),
            ));
        }
        Ok(())
    }

    pub fn effective_chunk_size(&self) -> u32 {
        self.chunk_size.min(SMB_LEGACY_READ_MAX)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamingWorkerOptions {
    pub stream_options: StreamOptions,
    pub low_watermark: usize,
    pub high_watermark: usize,
    pub pipeline_depth: usize,
}

impl Default for StreamingWorkerOptions {
    fn default() -> Self {
        let stream_options = StreamOptions::default();
        Self {
            stream_options,
            low_watermark: stream_options.read_ahead_capacity / 4,
            high_watermark: stream_options.read_ahead_capacity,
            pipeline_depth: DEFAULT_SMB1_PIPELINE_DEPTH,
        }
    }
}

impl StreamingWorkerOptions {
    pub fn new(
        stream_options: StreamOptions,
        low_watermark: usize,
        high_watermark: usize,
    ) -> Result<Self, Error> {
        Self::new_with_pipeline_depth(stream_options, low_watermark, high_watermark, 1)
    }

    pub fn new_with_pipeline_depth(
        stream_options: StreamOptions,
        low_watermark: usize,
        high_watermark: usize,
        pipeline_depth: usize,
    ) -> Result<Self, Error> {
        let options = Self {
            stream_options,
            low_watermark,
            high_watermark,
            pipeline_depth,
        };
        options.validate()?;
        Ok(options)
    }

    pub fn validate(&self) -> Result<(), Error> {
        self.stream_options.validate()?;

        if self.high_watermark == 0 {
            return Err(Error::InvalidConfig(
                "streaming worker high watermark must be greater than zero".to_owned(),
            ));
        }

        if self.low_watermark > self.high_watermark {
            return Err(Error::InvalidConfig(
                "streaming worker low watermark must not exceed high watermark".to_owned(),
            ));
        }

        if self.high_watermark > self.stream_options.read_ahead_capacity {
            return Err(Error::InvalidConfig(
                "streaming worker high watermark must not exceed read-ahead capacity".to_owned(),
            ));
        }

        if self.pipeline_depth == 0 {
            return Err(Error::InvalidConfig(
                "streaming worker pipeline depth must be greater than zero".to_owned(),
            ));
        }

        if self.pipeline_depth > 16 {
            return Err(Error::InvalidConfig(
                "streaming worker pipeline depth must not exceed 16".to_owned(),
            ));
        }

        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SmbMediaStreamOptions {
    pub worker_options: StreamingWorkerOptions,
    pub initial_buffer_size: usize,
}

impl Default for SmbMediaStreamOptions {
    fn default() -> Self {
        let stream_options = StreamOptions::default();
        let initial_buffer_size = DEFAULT_MEDIA_INITIAL_BUFFER_SIZE;
        let worker_options = StreamingWorkerOptions::new_with_pipeline_depth(
            stream_options,
            initial_buffer_size,
            DEFAULT_MEDIA_PREFILL_TARGET_SIZE,
            DEFAULT_SMB1_PIPELINE_DEPTH,
        )
        .expect("default SMB media stream options must be valid");

        Self {
            worker_options,
            initial_buffer_size,
        }
    }
}

impl SmbMediaStreamOptions {
    pub fn new(
        worker_options: StreamingWorkerOptions,
        initial_buffer_size: usize,
    ) -> Result<Self, Error> {
        let options = Self {
            worker_options,
            initial_buffer_size,
        };
        options.validate()?;
        Ok(options)
    }

    pub fn validate(&self) -> Result<(), Error> {
        self.worker_options.validate()?;

        if self.initial_buffer_size > self.worker_options.high_watermark {
            return Err(Error::InvalidConfig(
                "SMB media stream initial buffer must not exceed high watermark".to_owned(),
            ));
        }

        Ok(())
    }
}

#[derive(Debug, Default)]
struct StreamingBuffer {
    chunks: VecDeque<Bytes>,
    buffered: usize,
}

impl StreamingBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn buffered_len(&self) -> usize {
        self.buffered
    }

    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buffered == 0
    }

    pub fn clear(&mut self) {
        self.chunks.clear();
        self.buffered = 0;
    }

    pub fn push(&mut self, chunk: Bytes) {
        if chunk.is_empty() {
            return;
        }

        self.buffered += chunk.len();
        self.chunks.push_back(chunk);
    }

    pub fn pop_bytes(&mut self, max_len: usize) -> Option<Bytes> {
        if max_len == 0 {
            return None;
        }

        let mut chunk = self.chunks.pop_front()?;
        if chunk.len() > max_len {
            let out = chunk.split_to(max_len);
            self.buffered -= out.len();
            self.chunks.push_front(chunk);
            return Some(out);
        }

        self.buffered -= chunk.len();
        Some(chunk)
    }

    pub fn pop_block(&mut self, max_len: usize) -> Option<Bytes> {
        if max_len == 0 {
            return None;
        }

        let first = self.pop_bytes(max_len)?;
        if first.len() == max_len || self.chunks.is_empty() {
            return Some(first);
        }

        let capacity = max_len.min(first.len().saturating_add(self.buffered));
        let mut out = BytesMut::with_capacity(capacity);
        out.extend_from_slice(&first);

        while out.len() < max_len {
            let Some(chunk) = self.pop_bytes(max_len - out.len()) else {
                break;
            };
            out.extend_from_slice(&chunk);
        }

        Some(out.freeze())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StreamingWorkerReadRequest {
    pub offset: u64,
    pub len: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamingWorkerStats {
    pub playback_position: u64,
    pub source_position: u64,
    pub file_size: u64,
    pub buffered: usize,
    pub buffered_chunks: usize,
    pub low_watermark: usize,
    pub high_watermark: usize,
    pub source_eof: bool,
}

impl StreamingWorkerStats {
    pub fn remaining(&self) -> u64 {
        self.file_size.saturating_sub(self.playback_position)
    }

    pub fn prefetched(&self) -> u64 {
        self.source_position.saturating_sub(self.playback_position)
    }

    pub fn should_prefill(&self) -> bool {
        !self.source_eof
            && self.buffered <= self.low_watermark
            && self.buffered < self.high_watermark
    }

    pub fn high_watermark_deficit(&self) -> usize {
        self.high_watermark.saturating_sub(self.buffered)
    }
}

#[derive(Debug)]
struct StreamingWorkerState {
    options: StreamingWorkerOptions,
    buffer: StreamingBuffer,
    playback_position: u64,
    source_position: u64,
    file_size: u64,
    source_eof: bool,
}

impl StreamingWorkerState {
    pub fn new(file_size: u64, options: StreamingWorkerOptions) -> Result<Self, Error> {
        options.validate()?;

        Ok(Self {
            options,
            buffer: StreamingBuffer::new(),
            playback_position: 0,
            source_position: 0,
            file_size,
            source_eof: file_size == 0,
        })
    }

    pub fn source_position(&self) -> u64 {
        self.source_position
    }

    pub fn buffered_len(&self) -> usize {
        self.buffer.buffered_len()
    }

    pub fn is_source_eof(&self) -> bool {
        self.source_eof
    }

    pub fn is_finished(&self) -> bool {
        self.source_eof && self.buffer.is_empty()
    }

    pub fn stats(&self) -> StreamingWorkerStats {
        StreamingWorkerStats {
            playback_position: self.playback_position,
            source_position: self.source_position,
            file_size: self.file_size,
            buffered: self.buffer.buffered_len(),
            buffered_chunks: self.buffer.chunk_count(),
            low_watermark: self.options.low_watermark,
            high_watermark: self.options.high_watermark,
            source_eof: self.source_eof,
        }
    }

    pub fn should_prefill(&self) -> bool {
        self.stats().should_prefill()
    }

    pub fn next_buffered_read_requests(
        &self,
        buffered_goal: usize,
        max_requests: usize,
    ) -> Vec<StreamingWorkerReadRequest> {
        if max_requests == 0 || self.source_eof || self.source_position >= self.file_size {
            return Vec::new();
        }

        let buffered_goal = buffered_goal.min(self.options.high_watermark);
        let mut simulated_buffered = self.buffer.buffered_len();
        let mut simulated_source_position = self.source_position;
        let mut requests = Vec::with_capacity(max_requests);

        while requests.len() < max_requests
            && simulated_buffered < buffered_goal
            && simulated_source_position < self.file_size
        {
            let free = buffered_goal.saturating_sub(simulated_buffered);
            if free == 0 {
                break;
            }

            let count = free.min(self.options.stream_options.effective_chunk_size() as usize);
            let remaining = self.file_size.saturating_sub(simulated_source_position);

            let len = if remaining > usize::MAX as u64 {
                count
            } else {
                count.min(remaining as usize)
            };

            if len == 0 {
                break;
            }

            requests.push(StreamingWorkerReadRequest {
                offset: simulated_source_position,
                len,
            });

            simulated_source_position += len as u64;
            simulated_buffered += len;
        }

        requests
    }

    pub fn push_source_chunk(&mut self, chunk: Bytes) -> Result<(), Error> {
        if chunk.is_empty() {
            if self.source_position < self.file_size {
                return Err(Error::InternalError(
                    "streaming worker source returned empty chunk before EOF".to_owned(),
                ));
            }

            self.source_eof = true;
            return Ok(());
        }

        let remaining = self.file_size.saturating_sub(self.source_position);
        if chunk.len() as u64 > remaining {
            return Err(Error::InternalError(
                "streaming worker source returned more data than remains in file".to_owned(),
            ));
        }

        self.source_position += chunk.len() as u64;
        self.buffer.push(chunk);

        if self.source_position >= self.file_size {
            self.source_eof = true;
        }

        Ok(())
    }

    pub fn pop_read(&mut self, max_len: usize) -> Option<Bytes> {
        let chunk = self.buffer.pop_block(max_len)?;
        self.playback_position += chunk.len() as u64;
        Some(chunk)
    }

    pub fn seek(&mut self, pos: SeekFrom) -> Result<u64, Error> {
        let next = seek_position(self.file_size, self.playback_position, pos)?;

        self.buffer.clear();
        self.playback_position = next;
        self.source_position = next;
        self.source_eof = next >= self.file_size;

        Ok(next)
    }
}

impl StreamingWorker {
    pub fn new(stream: FileStream, options: StreamingWorkerOptions) -> Result<Self, Error> {
        let position = stream.position();
        let mut state = StreamingWorkerState::new(stream.size(), options)?;

        if position != 0 {
            state.seek(SeekFrom::Start(position))?;
        }

        Ok(Self { stream, state })
    }

    pub fn into_stream(self) -> FileStream {
        self.stream
    }

    pub fn stats(&self) -> StreamingWorkerStats {
        self.state.stats()
    }

    pub fn is_finished(&self) -> bool {
        self.state.is_finished()
    }

    pub fn should_prefill(&self) -> bool {
        self.state.should_prefill()
    }

    pub fn read_available(&mut self, max_len: usize) -> Option<Bytes> {
        self.state.pop_read(max_len)
    }

    pub fn seek(&mut self, pos: SeekFrom) -> Result<u64, Error> {
        let next = self.state.seek(pos)?;
        self.stream.seek(SeekFrom::Start(next))?;
        Ok(next)
    }

    pub async fn fill_until_buffered(
        &mut self,
        cifs: &mut Cifs,
        buffered_goal: usize,
    ) -> Result<bool, Error> {
        let mut filled = false;

        while !self.state.is_source_eof() {
            let requests = self
                .state
                .next_buffered_read_requests(buffered_goal, self.state.options.pipeline_depth);

            if requests.is_empty() {
                break;
            }

            let chunks = cifs
                .read_at_pipelined(&self.stream.handle, &requests)
                .await?;

            if chunks.is_empty() {
                break;
            }

            for chunk in chunks {
                self.state.push_source_chunk(chunk)?;
                self.stream
                    .seek(SeekFrom::Start(self.state.source_position()))?;

                filled = true;

                if self.state.is_source_eof() {
                    break;
                }
            }
        }

        Ok(filled)
    }

    pub async fn prefill_to_high_watermark(&mut self, cifs: &mut Cifs) -> Result<bool, Error> {
        self.fill_until_buffered(cifs, self.state.options.high_watermark)
            .await
    }

    pub async fn read_block(
        &mut self,
        cifs: &mut Cifs,
        max_len: usize,
    ) -> Result<Option<Bytes>, Error> {
        if max_len == 0 {
            return Ok(None);
        }

        if self.state.buffered_len() < max_len && !self.state.is_finished() {
            let buffered_goal = max_len.max(self.state.options.low_watermark);
            self.fill_until_buffered(cifs, buffered_goal).await?;
        }

        Ok(self.read_available(max_len))
    }
}

impl SmbMediaStream {
    fn new(stream: FileStream, options: SmbMediaStreamOptions) -> Result<Self, Error> {
        options.validate()?;

        let worker = StreamingWorker::new(stream, options.worker_options)?;

        Ok(Self { worker, options })
    }

    pub fn options(&self) -> SmbMediaStreamOptions {
        self.options
    }

    fn into_stream(self) -> FileStream {
        self.worker.into_stream()
    }

    pub fn stats(&self) -> StreamingWorkerStats {
        self.worker.stats()
    }

    pub fn is_finished(&self) -> bool {
        self.worker.is_finished()
    }

    pub fn should_prefill(&self) -> bool {
        self.worker.should_prefill()
    }

    pub fn seek(&mut self, pos: SeekFrom) -> Result<u64, Error> {
        self.worker.seek(pos)
    }

    pub async fn fill_initial_buffer(&mut self, cifs: &mut Cifs) -> Result<bool, Error> {
        if self.options.initial_buffer_size == 0 {
            return Ok(false);
        }

        self.worker
            .fill_until_buffered(cifs, self.options.initial_buffer_size)
            .await
    }

    pub async fn fill_initial_buffer_timeout(
        &mut self,
        cifs: &mut Cifs,
        timeout: Duration,
    ) -> Result<bool, Error> {
        with_timeout(timeout, self.fill_initial_buffer(cifs)).await
    }

    pub async fn maybe_prefill(&mut self, cifs: &mut Cifs) -> Result<bool, Error> {
        if !self.should_prefill() {
            return Ok(false);
        }

        self.worker.prefill_to_high_watermark(cifs).await
    }

    pub async fn maybe_prefill_timeout(
        &mut self,
        cifs: &mut Cifs,
        timeout: Duration,
    ) -> Result<bool, Error> {
        with_timeout(timeout, self.maybe_prefill(cifs)).await
    }

    pub async fn read_block(
        &mut self,
        cifs: &mut Cifs,
        max_len: usize,
    ) -> Result<Option<Bytes>, Error> {
        self.worker.read_block(cifs, max_len).await
    }

    pub async fn read_block_timeout(
        &mut self,
        cifs: &mut Cifs,
        max_len: usize,
        timeout: Duration,
    ) -> Result<Option<Bytes>, Error> {
        with_timeout(timeout, self.read_block(cifs, max_len)).await
    }
}

impl FileStream {
    pub fn new(handle: Handle) -> Self {
        Self {
            handle,
            position: 0,
        }
    }

    pub fn into_handle(self) -> Handle {
        self.handle
    }

    pub fn position(&self) -> u64 {
        self.position
    }

    pub fn size(&self) -> u64 {
        self.handle.size
    }

    pub fn seek(&mut self, pos: SeekFrom) -> Result<u64, Error> {
        self.position = seek_position(self.size(), self.position, pos)?;
        Ok(self.position)
    }
}

impl DirectoryReader {
    fn new(tid: u16, reply: trans2::subreply::FindFirst2) -> Self {
        let last_file = last_filename(&reply.info);

        Self {
            tid,
            sid: reply.sid,
            pending: Some(reply.info),
            last_file,
            end: reply.end,
        }
    }

    pub fn is_done(&self) -> bool {
        self.end && self.pending.is_none()
    }

    pub async fn next(&mut self, cifs: &mut Cifs) -> Result<Option<Vec<DirInfo>>, Error> {
        if let Some(info) = self.pending.take() {
            return Ok(Some(info));
        }
        if self.end {
            return Ok(None);
        }

        let last_file = self
            .last_file
            .as_deref()
            .ok_or_else(|| Error::InternalError("directory reader lost resume point".to_owned()))?;
        let subcmd = trans2::subcmd::FindNext2::new(self.sid, last_file);
        let reply: trans2::subreply::FindNext2 = cifs.transact2(self.tid, subcmd).await?;

        self.end = reply.end;
        self.last_file = last_filename(&reply.info);
        Ok(Some(reply.info))
    }

    pub async fn next_timeout(
        &mut self,
        cifs: &mut Cifs,
        timeout: Duration,
    ) -> Result<Option<Vec<DirInfo>>, Error> {
        with_timeout(timeout, self.next(cifs)).await
    }

    pub async fn next_media(&mut self, cifs: &mut Cifs) -> Result<Option<Vec<DirInfo>>, Error> {
        let Some(mut entries) = self.next(cifs).await? else {
            return Ok(None);
        };

        retain_media_entries(&mut entries);
        sort_dir_entries(&mut entries);
        Ok(Some(entries))
    }

    pub async fn next_media_timeout(
        &mut self,
        cifs: &mut Cifs,
        timeout: Duration,
    ) -> Result<Option<Vec<DirInfo>>, Error> {
        with_timeout(timeout, self.next_media(cifs)).await
    }

    pub async fn next_media_entries(
        &mut self,
        cifs: &mut Cifs,
    ) -> Result<Option<Vec<MediaEntry>>, Error> {
        Ok(self
            .next_media(cifs)
            .await?
            .map(|entries| entries.into_iter().map(MediaEntry::from).collect()))
    }

    pub async fn next_media_entries_timeout(
        &mut self,
        cifs: &mut Cifs,
        timeout: Duration,
    ) -> Result<Option<Vec<MediaEntry>>, Error> {
        with_timeout(timeout, self.next_media_entries(cifs)).await
    }
}

impl From<DirInfo> for MediaEntry {
    fn from(entry: DirInfo) -> Self {
        let kind = media_kind(&entry);

        Self {
            name: entry.filename,
            size: entry.filesize,
            kind,
        }
    }
}

impl MediaEntry {
    pub fn is_folder(&self) -> bool {
        self.kind == MediaKind::Folder
    }

    pub fn is_audio(&self) -> bool {
        self.kind == MediaKind::Audio
    }

    pub fn is_video(&self) -> bool {
        self.kind == MediaKind::Video
    }
}

impl MediaFolderSummary {
    pub fn from_entries(entries: &[MediaEntry]) -> Self {
        let primary_videos = primary_video_indices(entries);
        let main_video = largest_video_index(entries, &primary_videos);

        let extras = entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| is_likely_extra_video(entry))
            .map(|(index, _)| index)
            .collect();

        let audio_tracks = entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.is_audio())
            .map(|(index, _)| index)
            .collect();

        Self {
            main_video,
            primary_videos,
            extras,
            audio_tracks,
        }
    }

    pub fn can_collapse_to_movie(&self) -> bool {
        self.primary_videos.len() == 1
    }
}

impl MediaPresentation {
    pub fn from_entry(
        index: usize,
        entry: &MediaEntry,
        folder_summary: Option<MediaFolderSummary>,
    ) -> Self {
        match (entry.kind, folder_summary) {
            (MediaKind::Folder, Some(summary)) if summary.can_collapse_to_movie() => {
                Self::MovieFolder { index, summary }
            }
            (MediaKind::Folder, _) => Self::Folder { index },
            (MediaKind::Audio | MediaKind::Video, _) => Self::PlayableFile { index },
        }
    }

    pub fn index(&self) -> usize {
        match self {
            Self::Folder { index }
            | Self::MovieFolder { index, .. }
            | Self::PlayableFile { index } => *index,
        }
    }

    pub fn is_movie_folder(&self) -> bool {
        matches!(self, Self::MovieFolder { .. })
    }

    pub fn is_playable(&self) -> bool {
        matches!(self, Self::MovieFolder { .. } | Self::PlayableFile { .. })
    }
}

pub fn media_presentations(entries: &[MediaEntry]) -> Vec<MediaPresentation> {
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| MediaPresentation::from_entry(index, entry, None))
        .collect()
}

pub fn media_presentations_with_summaries(
    entries: &[MediaEntry],
    folder_summaries: &[(usize, MediaFolderSummary)],
) -> Vec<MediaPresentation> {
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            MediaPresentation::from_entry(index, entry, folder_summary(folder_summaries, index))
        })
        .collect()
}

pub fn sort_dir_entries(entries: &mut [DirInfo]) {
    entries.sort_by_cached_key(directory_sort_key);
}

pub fn is_media_entry(entry: &DirInfo) -> bool {
    if is_hidden_entry(entry) {
        return false;
    }

    if entry.attributes.contains(ExtFileAttr::DIRECTORY) {
        return true;
    }

    let Some(extension) = file_extension(&entry.filename) else {
        return false;
    };

    is_media_extension(extension) && !is_subtitle_extension(extension)
}

fn is_hidden_entry(entry: &DirInfo) -> bool {
    is_ignored_system_name(&entry.filename)
        || entry.filename.starts_with('.')
        || entry
            .attributes
            .intersects(ExtFileAttr::HIDDEN | ExtFileAttr::SYSTEM)
}

fn is_ignored_system_name(name: &str) -> bool {
    let name = name.trim();

    if name.is_empty() {
        return true;
    }

    let lower = name.to_ascii_lowercase();

    matches!(
        lower.as_str(),
        ".ds_store"
            | ".trashes"
            | ".spotlight-v100"
            | ".fseventsd"
            | "@eadir"
            | "$recycle.bin"
            | "system volume information"
            | "temporary items"
            | "network trash folder"
            | "thevolumesettingsfolder"
    )
}

pub fn retain_media_entries(entries: &mut Vec<DirInfo>) {
    entries.retain(is_media_entry);
}

pub fn main_video_index(entries: &[MediaEntry]) -> Option<usize> {
    let primary_videos = primary_video_indices(entries);
    largest_video_index(entries, &primary_videos)
}

pub fn primary_video_indices(entries: &[MediaEntry]) -> Vec<usize> {
    entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.is_video() && !is_likely_extra_video(entry))
        .map(|(index, _)| index)
        .collect()
}

fn largest_video_index(entries: &[MediaEntry], indices: &[usize]) -> Option<usize> {
    indices
        .iter()
        .copied()
        .max_by_key(|index| entries[*index].size)
}

pub fn is_likely_extra_video(entry: &MediaEntry) -> bool {
    entry.is_video() && is_likely_extra_name(&entry.name)
}

impl Cifs {
    pub async fn open(
        host: &str,
        port: Option<u16>,
        maybe_auth: Option<Auth>,
    ) -> Result<Self, Error> {
        let auth = maybe_auth.unwrap_or(Auth {
            user: String::new(),
            password: String::new(),
            domain: host.to_string(),
            workstation: "ANONYMOUS".to_owned(),
        });

        let netbios = match port {
            Some(port) => NetBios::open_raw(host, port).await?,
            None => NetBios::open(host, &auth.workstation).await?,
        };

        let mut cifs = Cifs {
            netbios,
            auth,

            max_smb_size: 1024,
            use_unicode: true,
            uid: 0,
            mid: 0,
            io_stats: CifsIoStats::default(),
            pending_replies: HashMap::new(),
        };

        cifs.setup_connection().await?;

        Ok(cifs)
    }

    pub async fn open_timeout(
        host: &str,
        port: Option<u16>,
        maybe_auth: Option<Auth>,
        timeout: Duration,
    ) -> Result<Self, Error> {
        with_timeout(timeout, Self::open(host, port, maybe_auth)).await
    }

    pub fn io_stats(&self) -> CifsIoStats {
        self.io_stats
    }

    pub fn reset_io_stats(&mut self) {
        self.io_stats = CifsIoStats::default();
    }

    pub async fn mount(&mut self, path: &str) -> Result<Share, Error> {
        self.mount_password(path, "").await
    }

    pub async fn mount_password(&mut self, path: &str, password: &str) -> Result<Share, Error> {
        self.command(msg::TreeConnect::new(
            sanitize_path(path),
            password.to_owned(),
        ))
        .await
    }

    pub async fn umount_ref(&mut self, share: &Share) -> Result<(), Error> {
        let _: reply::TreeDisconnect = self.command(msg::TreeDisconnect::new(share.tid)).await?;
        Ok(())
    }

    pub async fn umount(&mut self, share: Share) -> Result<(), Error> {
        self.umount_ref(&share).await
    }

    async fn openfile(&mut self, share: &Share, path: &str) -> Result<Handle, Error> {
        self.command(msg::Open::file_ro(share.tid, sanitize_path(path)))
            .await
    }

    async fn open_stream(&mut self, share: &Share, path: &str) -> Result<FileStream, Error> {
        Ok(FileStream::new(self.openfile(share, path).await?))
    }

    pub async fn open_media_stream(
        &mut self,
        share: &Share,
        path: &str,
    ) -> Result<SmbMediaStream, Error> {
        self.open_media_stream_with_options(share, path, SmbMediaStreamOptions::default())
            .await
    }

    pub async fn open_media_stream_with_options(
        &mut self,
        share: &Share,
        path: &str,
        options: SmbMediaStreamOptions,
    ) -> Result<SmbMediaStream, Error> {
        SmbMediaStream::new(self.open_stream(share, path).await?, options)
    }

    pub async fn close_media_stream(&mut self, stream: SmbMediaStream) -> Result<(), Error> {
        self.close_stream(stream.into_stream()).await
    }

    pub async fn close_ref(&mut self, file: &Handle) -> Result<(), Error> {
        let _: reply::Close = self.command(msg::Close::handle(file)).await?;
        Ok(())
    }

    pub async fn close(&mut self, file: Handle) -> Result<(), Error> {
        self.close_ref(&file).await
    }

    async fn close_stream(&mut self, stream: FileStream) -> Result<(), Error> {
        self.close(stream.into_handle()).await
    }

    async fn read_at(&mut self, file: &Handle, offset: u64, count: u32) -> Result<Bytes, Error> {
        if count == 0 {
            return Ok(Bytes::new());
        }

        let started = Instant::now();
        let reply: reply::Read = self.command(msg::Read::handle(file, offset, count)).await?;
        let elapsed = started.elapsed();

        self.io_stats.record_read(reply.data.len(), elapsed);

        Ok(reply.data)
    }

    async fn read_at_pipelined(
        &mut self,
        file: &Handle,
        requests: &[StreamingWorkerReadRequest],
    ) -> Result<Vec<Bytes>, Error> {
        if requests.is_empty() {
            return Ok(Vec::new());
        }

        if requests.len() == 1 {
            let request = requests[0];
            let count: u32 = request.len.try_into()?;
            let chunk = self.read_at(file, request.offset, count).await?;
            return Ok(vec![chunk]);
        }

        let mut in_flight = Vec::with_capacity(requests.len());

        for request in requests {
            let count: u32 = request.len.try_into()?;
            let started = Instant::now();
            let mid = self
                .send(msg::Read::handle(file, request.offset, count))
                .await?;

            in_flight.push((mid, started));
        }

        let mut chunks = Vec::with_capacity(in_flight.len());

        for (mid, started) in in_flight {
            let reply: reply::Read = self.recv(mid).await?;
            let elapsed = started.elapsed();

            self.io_stats.record_read(reply.data.len(), elapsed);
            chunks.push(reply.data);
        }

        Ok(chunks)
    }

    /// find_first starts a search for files in the given share for the given pattern.
    ///
    /// It returns a FindFirst2 structure r, with:
    ///
    ///   If r.end is true, r.info holds all the requested DirInfo.
    ///
    ///   If r.end is false, r.info holds a partial result and self.find_next()
    ///   must be used with r.sid as search id.
    ///
    pub async fn find_first(
        &mut self,
        share: &Share,
        pattern: &str,
    ) -> Result<trans2::subreply::FindFirst2, Error> {
        let search_flags = FileAttr::HIDDEN | FileAttr::SYSTEM | FileAttr::DIRECTORY;
        let subcmd = trans2::subcmd::FindFirst2::new(sanitize_path(pattern), search_flags);
        self.transact2(share.tid, subcmd).await
    }

    pub async fn open_dir_reader(
        &mut self,
        share: &Share,
        pattern: &str,
    ) -> Result<DirectoryReader, Error> {
        let reply = self.find_first(share, pattern).await?;
        Ok(DirectoryReader::new(share.tid, reply))
    }

    pub async fn open_dir_reader_timeout(
        &mut self,
        share: &Share,
        pattern: &str,
        timeout: Duration,
    ) -> Result<DirectoryReader, Error> {
        with_timeout(timeout, self.open_dir_reader(share, pattern)).await
    }

    /// find_next continues a search in the given share: sid must be the search
    /// id returned by a previous find_first and lastfile must be the last
    /// filename given by last find_first or find_next.
    ///
    /// It returns a FindNext2 structure r, with:
    ///   r.info holding a vector of additional DirInfo.
    ///   r.end is true if the search is done (otherwise find_next needs to be
    ///   called again).
    ///
    pub async fn find_next(
        &mut self,
        share: &Share,
        sid: u16,
        lastfile: &str,
    ) -> Result<trans2::subreply::FindNext2, Error> {
        let subcmd = trans2::subcmd::FindNext2::new(sid, lastfile);
        self.transact2(share.tid, subcmd).await
    }

    /// list is a high-level command doing a file-search for the given pattern in the
    /// given share. It returns a complete list of DirInfo, representing the search
    /// result. If more control is needed, use the more low-level find_first/find_next
    /// methods.
    pub async fn list(&mut self, share: &Share, pattern: &str) -> Result<Vec<DirInfo>, Error> {
        let reply = self.find_first(share, pattern).await?;
        if reply.end {
            return Ok(reply.info);
        }

        // we are not done: call find_next until we are
        let sid = reply.sid;
        let mut result = reply.info;

        loop {
            let mut reply = self
                .find_next(share, sid, &result.last().unwrap().filename)
                .await?;

            result.append(&mut reply.info);
            if reply.end {
                break;
            }
        }

        Ok(result)
    }

    //
    // private functions
    //
    async fn setup_connection(&mut self) -> Result<(), Error> {
        let server_setup = self.negotiate().await?;

        // update connection options based on what we learned
        self.max_smb_size = server_setup.max_buffer_size as usize;
        self.use_unicode = server_setup.capabilities.contains(Capabilities::UNICODE);

        if server_setup
            .capabilities
            .contains(Capabilities::EXTENDED_SECURITY)
        {
            self.authenticate_ntlmv2().await
        } else {
            self.authenticate_ntlm(server_setup.challenge).await
        }
    }

    async fn authenticate_ntlm(&mut self, challenge: Bytes) -> Result<(), Error> {
        // TODO: can we do this without clone?
        let setup_reply = self
            .session_setup(
                self.auth.user.clone(),
                self.auth.domain.clone(),
                self.auth.ntlmv1_authenticate(challenge.as_ref()),
            )
            .await?;

        self.uid = setup_reply.uid;

        Ok(())
    }

    async fn authenticate_ntlmv2(&mut self) -> Result<(), Error> {
        // initialize ntlm (also called type 1 message)
        let ntlm_init = {
            let mut ntlm_init_msg = ntlm::InitMsg::new(
                ntlm::Flags::UNICODE
                    | ntlm::Flags::OEM
                    | ntlm::Flags::REQUEST_TARGET
                    | ntlm::Flags::NTLM
                    | ntlm::Flags::DOMAIN_SUPPLIED
                    | ntlm::Flags::WORKSTATION_SUPPLIED,
            );

            ntlm_init_msg.set_origin(&self.auth.domain, &self.auth.workstation);
            ntlm_init_msg.set_default_version();
            ntlm_init_msg.to_bytes()?
        };

        let setup_reply = self.session_setup_ntlmv2(ntlm_init).await?;

        // take over uid the server gave us
        self.uid = setup_reply.uid;

        // try to parse security blob into ntlm challenge (also called type 2 message)
        let ntlm_challenge = ntlm::ChallengeMsg::parse(&setup_reply.security_blob)?;

        // calculate and send ntlm response (type 3 message)
        let ntlm_response = ntlm_challenge.response(&self.auth)?;
        let _ = self.session_setup_ntlmv2(ntlm_response).await?;

        Ok(())
    }

    /// sends a message to server and returns mid used to send it.
    async fn send<M: msg::Msg>(&mut self, msg: M) -> Result<u16, Error> {
        let mut frame_out = BytesMut::with_capacity(self.max_smb_size);

        // allocate a multiplex id
        let mid = self.mid;
        self.mid += 1;

        // create and write SMB header
        let mut info = Info::default(M::CMD);
        info.uid = self.uid;
        info.mid = mid;
        info.flags2.set(Flags2::UNICODE, self.use_unicode);
        msg.fix_header(&mut info);
        info.write(&mut frame_out);

        // add message body to frame and send it
        msg.write(&info, &mut frame_out)?;
        self.netbios.send_message(frame_out.freeze()).await?;

        Ok(mid)
    }

    async fn recv_frame(&mut self) -> Result<PendingReply, Error> {
        let mut frame = self.netbios.recv_message().await?;
        let info = Info::parse(&mut frame)?;

        Ok(PendingReply { info, body: frame })
    }

    fn parse_reply<R: reply::Reply>(pending: PendingReply) -> Result<R, Error> {
        let PendingReply { info, body } = pending;

        if info.cmd != R::CMD {
            return Err(Error::UnexpectedReply(R::CMD, info.cmd));
        }

        if let Status::Known(status) = info.status {
            match status {
                NTStatus::SUCCESS => (),
                NTStatus::MORE_PROCESSING if info.cmd == Cmd::SessionSetup => (),

                _ => return Err(Error::ServerError(info.status)),
            }
        } else {
            return Err(Error::ServerError(info.status));
        }

        R::parse(info, body).map_err(|e| e.into())
    }

    async fn recv<R: reply::Reply>(&mut self, mid: u16) -> Result<R, Error> {
        if let Some(pending) = self.pending_replies.remove(&mid) {
            return Self::parse_reply(pending);
        }

        loop {
            let pending = self.recv_frame().await?;
            let pending_mid = pending.info.mid;

            if pending_mid == mid {
                return Self::parse_reply(pending);
            }

            if self.pending_replies.insert(pending_mid, pending).is_some() {
                return Err(Error::InternalError(format!(
                    "duplicate pending SMB reply for mid {pending_mid}"
                )));
            }
        }
    }

    /// Sends a generic message M and expects result generic R. There is no
    /// check that M and R "fit" together (like M::CMD == R::CMD), so this
    /// is clearly not meant to be a public method.
    /// We built safe wrapper around command, with correct message and reply
    /// types.
    async fn command<M, R>(&mut self, msg: M) -> Result<R, Error>
    where
        M: msg::Msg,
        R: reply::Reply,
    {
        let mid = self.send(msg).await?;
        self.recv(mid).await
    }

    async fn transact2<C, R>(&mut self, tid: u16, subcmd: C) -> Result<R, Error>
    where
        C: trans2::SubCmd,
        R: trans2::SubReply,
    {
        // we only send single transaction messages with the given subcommand.
        // (in theory we could fragment the message if the subcommand is too big)
        let mid = self.send(trans2::msg::Transact2::new(tid, subcmd)).await?;

        // collect replies
        let mut ctx = trans2::collector::CollectTrans2::new();
        loop {
            let reply: trans2::reply::Transact2 = self.recv(mid).await?;
            if ctx.add(reply)? {
                break;
            }
        }

        Ok(ctx.get_subreply()?)
    }

    async fn negotiate(&mut self) -> Result<reply::ServerSetup, Error> {
        self.command(msg::Negotiate {}).await
    }

    async fn session_setup(
        &mut self,
        user: String,
        domain: String,
        secret: [u8; 24],
    ) -> Result<reply::SessionSetup, Error> {
        self.command(msg::SessionSetup::with_auth(user, domain, secret))
            .await
    }

    async fn session_setup_ntlmv2(&mut self, blob: Bytes) -> Result<reply::SessionSetup, Error> {
        self.command(msg::SessionSetup::with_blob(blob)).await
    }
}

fn folder_summary(
    folder_summaries: &[(usize, MediaFolderSummary)],
    index: usize,
) -> Option<MediaFolderSummary> {
    folder_summaries
        .iter()
        .find(|(folder_index, _)| *folder_index == index)
        .map(|(_, summary)| summary.clone())
}

fn last_filename(info: &[DirInfo]) -> Option<String> {
    info.last().map(|entry| entry.filename.clone())
}

fn directory_sort_key(entry: &DirInfo) -> DirectorySortKey {
    DirectorySortKey {
        file: !entry.attributes.contains(ExtFileAttr::DIRECTORY),
        name: natural_name_key(&entry.filename),
    }
}

fn file_extension(filename: &str) -> Option<&str> {
    filename.rsplit_once('.').and_then(|(_, extension)| {
        if extension.is_empty() {
            None
        } else {
            Some(extension)
        }
    })
}

fn is_media_extension(extension: &str) -> bool {
    let extension = extension.to_ascii_lowercase();
    AUDIO_EXTENSIONS.contains(&extension.as_str()) || VIDEO_EXTENSIONS.contains(&extension.as_str())
}

fn media_kind(entry: &DirInfo) -> MediaKind {
    if entry.attributes.contains(ExtFileAttr::DIRECTORY) {
        return MediaKind::Folder;
    }

    match file_extension(&entry.filename) {
        Some(extension) if is_audio_extension(extension) => MediaKind::Audio,
        _ => MediaKind::Video,
    }
}

fn is_audio_extension(extension: &str) -> bool {
    let extension = extension.to_ascii_lowercase();
    AUDIO_EXTENSIONS.contains(&extension.as_str())
}

pub fn media_extra_kind(name: &str) -> Option<MediaExtraKind> {
    let normalized = normalized_media_name(name);

    for (flag, kind) in [
        ("--deleted-scenes", MediaExtraKind::DeletedScene),
        ("--deleted-scene", MediaExtraKind::DeletedScene),
        ("--deleted", MediaExtraKind::DeletedScene),
        ("--behind-the-scenes", MediaExtraKind::BehindTheScenes),
        ("--behind", MediaExtraKind::BehindTheScenes),
        ("--featurette", MediaExtraKind::Featurette),
        ("--interview", MediaExtraKind::Interview),
        ("--trailer", MediaExtraKind::Trailer),
        ("--teaser", MediaExtraKind::Teaser),
        ("--sample", MediaExtraKind::Sample),
        ("--short", MediaExtraKind::Short),
        ("--bonus", MediaExtraKind::Bonus),
        ("--extra", MediaExtraKind::Extra),
    ] {
        if normalized.contains(flag) {
            return Some(kind);
        }
    }

    for (marker, kind) in [
        ("behind", MediaExtraKind::BehindTheScenes),
        ("deleted", MediaExtraKind::DeletedScene),
        ("extras", MediaExtraKind::Extra),
        ("extra", MediaExtraKind::Extra),
        ("featurette", MediaExtraKind::Featurette),
        ("interview", MediaExtraKind::Interview),
        ("sample", MediaExtraKind::Sample),
        ("teaser", MediaExtraKind::Teaser),
        ("trailer", MediaExtraKind::Trailer),
    ] {
        if normalized.contains(marker) {
            return Some(kind);
        }
    }

    None
}

fn is_likely_extra_name(name: &str) -> bool {
    media_extra_kind(name).is_some()
}

fn normalized_media_name(name: &str) -> String {
    name.to_ascii_lowercase()
}

fn is_subtitle_extension(extension: &str) -> bool {
    let extension = extension.to_ascii_lowercase();
    SUBTITLE_EXTENSIONS.contains(&extension.as_str())
}

fn natural_name_key(name: &str) -> NaturalNameKey {
    let mut tokens = Vec::new();
    let mut chars = name.chars().peekable();

    while let Some(next) = chars.peek().copied() {
        if next.is_ascii_digit() {
            let mut raw = String::new();
            while let Some(c) = chars.peek().copied() {
                if !c.is_ascii_digit() {
                    break;
                }
                raw.push(c);
                chars.next();
            }

            let significant = raw.trim_start_matches('0');
            tokens.push(NaturalToken::Number {
                significant_len: significant.len().max(1),
                significant: if significant.is_empty() {
                    "0".to_owned()
                } else {
                    significant.to_owned()
                },
                raw_len: raw.len(),
            });
        } else {
            let mut text = String::new();
            while let Some(c) = chars.peek().copied() {
                if c.is_ascii_digit() {
                    break;
                }
                text.push(c);
                chars.next();
            }
            tokens.push(NaturalToken::Text(text.to_lowercase()));
        }
    }

    NaturalNameKey(tokens)
}

fn seek_position(size: u64, current: u64, pos: SeekFrom) -> Result<u64, Error> {
    let next = match pos {
        SeekFrom::Start(offset) => i128::from(offset),
        SeekFrom::End(offset) => i128::from(size) + i128::from(offset),
        SeekFrom::Current(offset) => i128::from(current) + i128::from(offset),
    };

    if next < 0 || next > i128::from(u64::MAX) {
        return Err(Error::InternalError(
            "seek position is out of range".to_owned(),
        ));
    }

    Ok(next as u64)
}

async fn with_timeout<T>(
    timeout: Duration,
    future: impl Future<Output = Result<T, Error>>,
) -> Result<T, Error> {
    tokio::time::timeout(timeout, future)
        .await
        .map_err(|_| Error::Timeout("operation timed out".to_owned()))?
}

/// Struct for holding the result of resolve_smb_uri
pub struct CifsConfig<'a> {
    pub domain: Option<&'a str>,
    pub user: Option<&'a str>,
    pub password: Option<&'a str>,
    pub hostname: &'a str,
    pub port: Option<u16>,
    pub share: Option<&'a str>,
    pub path: Option<&'a str>,
}

///
/// Helper function that decodes an SMB URI and returns a CifsConfig
///
pub fn resolve_smb_uri<'a>(uri: &'a str) -> Result<CifsConfig<'a>, Error> {
    lazy_static! {
        static ref URI_REGEX: Regex =
            Regex::new(r"^smb://((?P<domain>\w+);)?((?P<user>[\w\.\+_-]+)(:(?P<passwd>[^@]*))?@)?(?P<host>\w[\w\.-]*)(:(?P<port>\d+))?(/(?P<share>[\w\._-]+)(/(?P<path>.*))?)?/?$")
                .expect("can't compile URI regex");
    }

    let uri_match = URI_REGEX.captures(uri).ok_or(Error::InvalidUri)?;

    let config = CifsConfig {
        domain: uri_match.name("domain").map(|m| m.as_str()),

        user: uri_match.name("user").map(|m| m.as_str()),

        password: uri_match.name("passwd").map(|m| m.as_str()),

        hostname: uri_match.name("host").ok_or(Error::InvalidUri)?.as_str(),

        port: uri_match
            .name("port")
            .map(|m| m.as_str().parse::<u16>())
            .transpose()
            .map_err(|_| Error::InvalidUri)?,

        share: uri_match.name("share").map(|m| m.as_str()),

        path: uri_match.name("path").map(|m| m.as_str()),
    };

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_SMB1_PIPELINE_DEPTH, MediaEntry, MediaExtraKind, MediaFolderSummary, MediaKind,
        MediaPresentation, SMB_LEGACY_READ_MAX, SmbMediaStream, SmbMediaStreamOptions,
        StreamOptions, StreamingBuffer, StreamingWorker, StreamingWorkerOptions,
        StreamingWorkerReadRequest, StreamingWorkerState, StreamingWorkerStats,
        is_likely_extra_video, is_media_entry, main_video_index, media_extra_kind,
        media_presentations, resolve_smb_uri, retain_media_entries, seek_position,
        sort_dir_entries,
    };
    use bytes::Bytes;
    use chrono::Local;
    use std::io::SeekFrom;

    #[test]
    fn stream_options_have_streaming_defaults() {
        let options = StreamOptions::default();

        assert_eq!(
            options.read_ahead_capacity,
            super::DEFAULT_READ_AHEAD_CAPACITY
        );
        assert_eq!(options.chunk_size, SMB_LEGACY_READ_MAX);
        assert!(options.validate().is_ok());
    }

    #[test]
    fn stream_options_reject_zero_values() {
        assert!(StreamOptions::new(0, 1).is_err());
        assert!(StreamOptions::new(1, 0).is_err());
    }

    #[test]
    fn streaming_worker_options_have_safe_defaults() {
        let options = StreamingWorkerOptions::default();

        assert_eq!(options.stream_options, StreamOptions::default());
        assert_eq!(
            options.low_watermark,
            options.stream_options.read_ahead_capacity / 4
        );
        assert_eq!(
            options.high_watermark,
            options.stream_options.read_ahead_capacity
        );
        assert_eq!(options.pipeline_depth, DEFAULT_SMB1_PIPELINE_DEPTH);
        assert!(options.validate().is_ok());
    }

    #[test]
    fn streaming_worker_options_reject_invalid_watermarks() {
        let stream_options = StreamOptions::default();

        assert!(
            StreamingWorkerOptions::new(
                stream_options,
                stream_options.read_ahead_capacity + 1,
                stream_options.read_ahead_capacity,
            )
            .is_err()
        );

        assert!(
            StreamingWorkerOptions::new(stream_options, 0, stream_options.read_ahead_capacity + 1,)
                .is_err()
        );
    }

    #[test]
    fn streaming_worker_options_reject_zero_sizes() {
        let stream_options = StreamOptions::default();

        assert!(StreamingWorkerOptions::new(stream_options, 0, 0).is_err());
    }

    #[test]
    fn seek_position_supports_common_origins() {
        assert_eq!(seek_position(100, 10, SeekFrom::Start(7)).unwrap(), 7);
        assert_eq!(seek_position(100, 10, SeekFrom::Current(5)).unwrap(), 15);
        assert_eq!(seek_position(100, 10, SeekFrom::End(-20)).unwrap(), 80);
    }

    #[test]
    fn seek_position_rejects_negative_offsets() {
        assert!(seek_position(100, 10, SeekFrom::Current(-11)).is_err());
        assert!(seek_position(100, 10, SeekFrom::End(-101)).is_err());
    }

    #[test]
    fn sort_dir_entries_uses_media_friendly_order() {
        let mut entries = vec![
            fake_dir_entry("Episode 10.mkv", false),
            fake_dir_entry("season 2", true),
            fake_dir_entry("Episode 2.mkv", false),
            fake_dir_entry("Season 1", true),
            fake_dir_entry("episode 01.mkv", false),
        ];

        sort_dir_entries(&mut entries);

        assert_eq!(
            filenames(&entries),
            vec![
                "Season 1",
                "season 2",
                "episode 01.mkv",
                "Episode 2.mkv",
                "Episode 10.mkv",
            ]
        );
    }

    #[test]
    fn media_filter_keeps_folders_audio_and_video_only() {
        let mut entries = vec![
            fake_dir_entry("Movies", true),
            fake_dir_entry(".Trash", true),
            fake_dir_entry("Episode 01.mkv", false),
            fake_dir_entry("Theme.FLAC", false),
            fake_dir_entry("Commentary.opus", false),
            fake_dir_entry("Episode 01.srt", false),
            fake_dir_entry("cover.jpg", false),
            fake_dir_entry(".DS_Store", false),
            fake_dir_entry("notes.txt", false),
        ];

        retain_media_entries(&mut entries);

        assert_eq!(
            filenames(&entries),
            vec!["Movies", "Episode 01.mkv", "Theme.FLAC", "Commentary.opus"]
        );
    }

    #[test]
    fn media_filter_rejects_hidden_system_entries() {
        let hidden = fake_dir_entry_with_attrs("movie.mkv", crate::win::ExtFileAttr::HIDDEN);
        let system = fake_dir_entry_with_attrs("song.mp3", crate::win::ExtFileAttr::SYSTEM);

        assert!(!is_media_entry(&hidden));
        assert!(!is_media_entry(&system));
    }

    #[test]
    fn media_entry_maps_dir_info_to_ui_shape() {
        assert_eq!(
            MediaEntry::from(fake_dir_entry("Series", true)),
            MediaEntry {
                name: "Series".to_owned(),
                size: 0,
                kind: MediaKind::Folder,
            }
        );
        assert_eq!(
            MediaEntry::from(fake_dir_entry("Commentary.opus", false)).kind,
            MediaKind::Audio
        );
        assert_eq!(
            MediaEntry::from(fake_dir_entry("Episode.mkv", false)).kind,
            MediaKind::Video
        );
    }

    #[test]
    fn main_video_index_prefers_largest_non_extra_video() {
        let entries = vec![
            fake_media_entry("Movie Trailer.mkv", 4_000, MediaKind::Video),
            fake_media_entry("Movie.mkv", 10_000, MediaKind::Video),
            fake_media_entry("Sample.mkv", 12_000, MediaKind::Video),
            fake_media_entry("Commentary.opus", 1_000, MediaKind::Audio),
        ];

        assert!(is_likely_extra_video(&entries[0]));
        assert!(is_likely_extra_video(&entries[2]));
        assert_eq!(main_video_index(&entries), Some(1));
    }

    #[test]
    fn main_video_index_ignores_extra_only_videos() {
        let entries = vec![
            MediaEntry {
                name: "Trailer.mkv".to_owned(),
                size: 10_000,
                kind: MediaKind::Video,
            },
            MediaEntry {
                name: "Sample.mkv".to_owned(),
                size: 20_000,
                kind: MediaKind::Video,
            },
        ];

        assert_eq!(main_video_index(&entries), None);

        let summary = MediaFolderSummary::from_entries(&entries);

        assert_eq!(summary.main_video, None);
        assert!(summary.primary_videos.is_empty());
        assert_eq!(summary.extras, vec![0, 1]);
        assert!(!summary.can_collapse_to_movie());
    }

    #[test]
    fn media_folder_summary_identifies_collapsible_movie_folder() {
        let entries = vec![
            fake_media_entry("Movie.mkv", 10_000, MediaKind::Video),
            fake_media_entry("Movie Trailer.mkv", 2_000, MediaKind::Video),
            fake_media_entry("Commentary.opus", 1_000, MediaKind::Audio),
            fake_media_entry("Behind The Scenes.mkv", 3_000, MediaKind::Video),
        ];

        let summary = MediaFolderSummary::from_entries(&entries);

        assert!(summary.can_collapse_to_movie());
        assert_eq!(summary.main_video, Some(0));
        assert_eq!(summary.extras, vec![1, 3]);
        assert_eq!(summary.audio_tracks, vec![2]);
    }

    #[test]
    fn media_folder_summary_keeps_non_movie_folder_open() {
        let entries = vec![
            fake_media_entry("Season 1", 0, MediaKind::Folder),
            fake_media_entry("Season 2", 0, MediaKind::Folder),
            fake_media_entry("Theme.opus", 1_000, MediaKind::Audio),
        ];

        let summary = MediaFolderSummary::from_entries(&entries);

        assert!(!summary.can_collapse_to_movie());
        assert_eq!(summary.main_video, None);
        assert!(summary.extras.is_empty());
        assert_eq!(summary.audio_tracks, vec![2]);
    }

    #[test]
    fn media_presentations_map_entries_without_hidden_scans() {
        let entries = vec![
            fake_media_entry("Movies", 0, MediaKind::Folder),
            fake_media_entry("Song.opus", 1_000, MediaKind::Audio),
            fake_media_entry("Movie.mkv", 10_000, MediaKind::Video),
        ];

        assert_eq!(
            media_presentations(&entries),
            vec![
                MediaPresentation::Folder { index: 0 },
                MediaPresentation::PlayableFile { index: 1 },
                MediaPresentation::PlayableFile { index: 2 },
            ]
        );
    }

    #[test]
    fn media_folder_summary_collapses_single_movie_folder() {
        let entries = vec![
            MediaEntry {
                name: "Movie.mkv".to_owned(),
                size: 10_000,
                kind: MediaKind::Video,
            },
            MediaEntry {
                name: "Trailer.mkv".to_owned(),
                size: 1_000,
                kind: MediaKind::Video,
            },
        ];

        let summary = MediaFolderSummary::from_entries(&entries);

        assert_eq!(summary.primary_videos, vec![0]);
        assert_eq!(summary.main_video, Some(0));
        assert_eq!(summary.extras, vec![1]);
        assert!(summary.can_collapse_to_movie());
    }

    #[test]
    fn media_folder_summary_does_not_collapse_movie_collection() {
        let entries = vec![
            MediaEntry {
                name: "Movie 1.mkv".to_owned(),
                size: 10_000,
                kind: MediaKind::Video,
            },
            MediaEntry {
                name: "Movie 2.mkv".to_owned(),
                size: 12_000,
                kind: MediaKind::Video,
            },
        ];

        let summary = MediaFolderSummary::from_entries(&entries);

        assert_eq!(summary.primary_videos, vec![0, 1]);
        assert_eq!(summary.main_video, Some(1));
        assert!(!summary.can_collapse_to_movie());
    }

    #[test]
    fn media_folder_summary_does_not_collapse_episode_folder() {
        let entries = vec![
            MediaEntry {
                name: "Episode 01.mkv".to_owned(),
                size: 1_000,
                kind: MediaKind::Video,
            },
            MediaEntry {
                name: "Episode 02.mkv".to_owned(),
                size: 1_100,
                kind: MediaKind::Video,
            },
            MediaEntry {
                name: "Episode 03.mkv".to_owned(),
                size: 1_200,
                kind: MediaKind::Video,
            },
        ];

        let summary = MediaFolderSummary::from_entries(&entries);

        assert_eq!(summary.primary_videos.len(), 3);
        assert_eq!(summary.main_video, Some(2));
        assert!(!summary.can_collapse_to_movie());
    }

    #[test]
    fn test_uri() {
        let uri = "smb://localhost/myshare/this/is/a/path";
        let config = resolve_smb_uri(uri).unwrap();

        assert_eq!(config.domain, None);
        assert_eq!(config.user, None);
        assert_eq!(config.password, None);
        assert_eq!(config.hostname, "localhost");
        assert_eq!(config.port, None);
        assert_eq!(config.share, Some("myshare"));
        assert_eq!(config.path, Some("this/is/a/path"));

        let uri = "smb://www.example.org:31337/foo";
        let config = resolve_smb_uri(uri).unwrap();
        assert_eq!(config.domain, None);
        assert_eq!(config.user, None);
        assert_eq!(config.password, None);
        assert_eq!(config.hostname, "www.example.org");
        assert_eq!(config.port, Some(31337));
        assert_eq!(config.share, Some("foo"));
        assert_eq!(config.path, None);

        let uri = "smb://127.0.0.1:445/share/foo";
        let config = resolve_smb_uri(uri).unwrap();
        assert_eq!(config.domain, None);
        assert_eq!(config.user, None);
        assert_eq!(config.password, None);
        assert_eq!(config.hostname, "127.0.0.1");
        assert_eq!(config.port, Some(445));
        assert_eq!(config.share, Some("share"));
        assert_eq!(config.path, Some("foo"));

        let uri = "smb://anonymous@localhost/public";
        let config = resolve_smb_uri(uri).unwrap();
        assert_eq!(config.domain, None);
        assert_eq!(config.user, Some("anonymous"));
        assert_eq!(config.password, None);
        assert_eq!(config.hostname, "localhost");
        assert_eq!(config.port, None);
        assert_eq!(config.share, Some("public"));
        assert_eq!(config.path, None);

        let uri = "smb://john:secret@localhost/closed";
        let config = resolve_smb_uri(uri).unwrap();
        assert_eq!(config.domain, None);
        assert_eq!(config.user, Some("john"));
        assert_eq!(config.password, Some("secret"));
        assert_eq!(config.hostname, "localhost");
        assert_eq!(config.port, None);
        assert_eq!(config.share, Some("closed"));
        assert_eq!(config.path, None);

        let uri = "smb://WORKGROUP;foo/bar";
        let config = resolve_smb_uri(uri).unwrap();
        assert_eq!(config.domain, Some("WORKGROUP"));
        assert_eq!(config.user, None);
        assert_eq!(config.password, None);
        assert_eq!(config.hostname, "foo");
        assert_eq!(config.port, None);
        assert_eq!(config.share, Some("bar"));
        assert_eq!(config.path, None);

        let uri = "smb://NOSTROMO;Ellen.Ripley:100375@Mother:445/interface/special/order/937.txt";
        let config = resolve_smb_uri(uri).unwrap();
        assert_eq!(config.domain, Some("NOSTROMO"));
        assert_eq!(config.user, Some("Ellen.Ripley"));
        assert_eq!(config.password, Some("100375"));
        assert_eq!(config.hostname, "Mother");
        assert_eq!(config.port, Some(445));
        assert_eq!(config.share, Some("interface"));
        assert_eq!(config.path, Some("special/order/937.txt"));

        let uri = "smb://10.0.1.1/";
        let config = resolve_smb_uri(uri).unwrap();

        assert_eq!(config.domain, None);
        assert_eq!(config.user, None);
        assert_eq!(config.password, None);
        assert_eq!(config.hostname, "10.0.1.1");
        assert_eq!(config.port, None);
        assert_eq!(config.share, None);
        assert_eq!(config.path, None);

        let uri = "smb://10.0.1.1";
        let config = resolve_smb_uri(uri).unwrap();

        assert_eq!(config.hostname, "10.0.1.1");
        assert_eq!(config.share, None);
        assert_eq!(config.path, None);
    }

    fn fake_stream(size: u64) -> super::FileStream {
        super::FileStream::new(super::Handle {
            tid: 1,
            fid: 2,
            oplock: crate::win::OpLockLevel::empty(),
            disposition: crate::win::CreateDisposition::OPEN,
            create_time: 0,
            access_time: 0,
            write_time: 0,
            change_time: 0,
            attributes: crate::win::ExtFileAttr::empty(),
            allocation_size: size,
            size,
            file_type: crate::win::ResourceType::DISK,
            directory: false,
        })
    }

    fn fake_dir_entry(filename: &str, directory: bool) -> super::DirInfo {
        let attributes = if directory {
            crate::win::ExtFileAttr::DIRECTORY
        } else {
            crate::win::ExtFileAttr::empty()
        };

        fake_dir_entry_with_attrs(filename, attributes)
    }

    fn fake_dir_entry_with_attrs(
        filename: &str,
        attributes: crate::win::ExtFileAttr,
    ) -> super::DirInfo {
        let now = Local::now();
        super::DirInfo {
            creation_time: now,
            access_time: now,
            write_time: now,
            change_time: now,
            filename: filename.to_owned(),
            filesize: 0,
            attributes,
        }
    }

    fn fake_media_entry(name: &str, size: u64, kind: MediaKind) -> MediaEntry {
        MediaEntry {
            name: name.to_owned(),
            size,
            kind,
        }
    }

    fn filenames(entries: &[super::DirInfo]) -> Vec<&str> {
        entries
            .iter()
            .map(|entry| entry.filename.as_str())
            .collect()
    }

    #[test]
    fn streaming_buffer_tracks_bytes_and_chunks() {
        let mut buffer = StreamingBuffer::new();

        assert!(buffer.is_empty());
        assert_eq!(buffer.buffered_len(), 0);
        assert_eq!(buffer.chunk_count(), 0);

        buffer.push(Bytes::from_static(b"abcd"));
        buffer.push(Bytes::from_static(b"ef"));

        assert!(!buffer.is_empty());
        assert_eq!(buffer.buffered_len(), 6);
        assert_eq!(buffer.chunk_count(), 2);
    }

    #[test]
    fn streaming_buffer_pop_bytes_splits_front_chunk() {
        let mut buffer = StreamingBuffer::new();

        buffer.push(Bytes::from_static(b"abcdef"));

        assert_eq!(buffer.pop_bytes(2).unwrap().as_ref(), b"ab");
        assert_eq!(buffer.buffered_len(), 4);
        assert_eq!(buffer.chunk_count(), 1);

        assert_eq!(buffer.pop_bytes(10).unwrap().as_ref(), b"cdef");
        assert_eq!(buffer.buffered_len(), 0);
        assert_eq!(buffer.chunk_count(), 0);
        assert!(buffer.is_empty());
    }

    #[test]
    fn streaming_buffer_pop_block_combines_chunks() {
        let mut buffer = StreamingBuffer::new();

        buffer.push(Bytes::from_static(b"ab"));
        buffer.push(Bytes::from_static(b"cdef"));
        buffer.push(Bytes::from_static(b"gh"));

        assert_eq!(buffer.pop_block(5).unwrap().as_ref(), b"abcde");
        assert_eq!(buffer.buffered_len(), 3);
        assert_eq!(buffer.chunk_count(), 2);

        assert_eq!(buffer.pop_block(10).unwrap().as_ref(), b"fgh");
        assert!(buffer.is_empty());
    }

    #[test]
    fn streaming_buffer_ignores_empty_chunks_and_clears() {
        let mut buffer = StreamingBuffer::new();

        buffer.push(Bytes::new());
        assert!(buffer.is_empty());
        assert_eq!(buffer.chunk_count(), 0);

        buffer.push(Bytes::from_static(b"abc"));
        buffer.push(Bytes::from_static(b"def"));
        assert_eq!(buffer.buffered_len(), 6);

        buffer.clear();

        assert!(buffer.is_empty());
        assert_eq!(buffer.buffered_len(), 0);
        assert_eq!(buffer.chunk_count(), 0);
        assert_eq!(buffer.pop_bytes(1), None);
    }

    #[test]
    fn streaming_worker_state_starts_empty_and_requests_buffered_read() {
        let options =
            StreamingWorkerOptions::new(StreamOptions::new(16, 4).unwrap(), 4, 12).unwrap();
        let state = StreamingWorkerState::new(100, options).unwrap();

        assert_eq!(state.stats().playback_position, 0);
        assert_eq!(state.source_position(), 0);
        assert_eq!(state.stats().file_size, 100);
        assert_eq!(state.buffered_len(), 0);
        assert!(!state.is_source_eof());
        assert_eq!(
            state.next_buffered_read_requests(4, 1),
            vec![StreamingWorkerReadRequest { offset: 0, len: 4 }]
        );

        let stats = state.stats();
        assert_eq!(stats.remaining(), 100);
        assert_eq!(stats.prefetched(), 0);
    }

    #[test]
    fn streaming_worker_state_rejects_source_overread() {
        let options =
            StreamingWorkerOptions::new(StreamOptions::new(16, 4).unwrap(), 2, 12).unwrap();
        let mut state = StreamingWorkerState::new(3, options).unwrap();

        assert!(
            state
                .push_source_chunk(Bytes::from_static(b"abcd"))
                .is_err()
        );
        assert_eq!(state.source_position(), 0);
        assert_eq!(state.buffered_len(), 0);
    }

    #[test]
    fn streaming_worker_state_tracks_eof_after_last_source_chunk() {
        let options =
            StreamingWorkerOptions::new(StreamOptions::new(16, 4).unwrap(), 2, 12).unwrap();
        let mut state = StreamingWorkerState::new(3, options).unwrap();

        assert_eq!(
            state.next_buffered_read_requests(10, 1),
            vec![StreamingWorkerReadRequest { offset: 0, len: 3 }]
        );
        state.push_source_chunk(Bytes::from_static(b"abc")).unwrap();

        assert!(state.is_source_eof());
        assert!(!state.is_finished());

        assert_eq!(state.pop_read(10).unwrap().as_ref(), b"abc");
        assert!(state.is_finished());
    }

    #[test]
    fn streaming_worker_state_seek_clears_buffer_and_resets_source_position() {
        let options =
            StreamingWorkerOptions::new(StreamOptions::new(16, 4).unwrap(), 2, 12).unwrap();
        let mut state = StreamingWorkerState::new(100, options).unwrap();

        state
            .push_source_chunk(Bytes::from_static(b"abcd"))
            .unwrap();
        assert_eq!(state.pop_read(2).unwrap().as_ref(), b"ab");

        assert_eq!(state.seek(SeekFrom::Start(50)).unwrap(), 50);
        assert_eq!(state.stats().playback_position, 50);
        assert_eq!(state.source_position(), 50);
        assert_eq!(state.buffered_len(), 0);
        assert!(!state.is_source_eof());
        assert_eq!(
            state.next_buffered_read_requests(4, 1),
            vec![StreamingWorkerReadRequest { offset: 50, len: 4 }]
        );
    }
    #[test]
    fn streaming_worker_state_seek_to_end_marks_source_eof() {
        let mut state = StreamingWorkerState::new(100, StreamingWorkerOptions::default()).unwrap();

        assert_eq!(state.seek(SeekFrom::End(0)).unwrap(), 100);
        assert_eq!(state.stats().playback_position, 100);
        assert_eq!(state.source_position(), 100);
        assert!(state.is_source_eof());
        assert!(state.is_finished());
    }
    #[test]
    fn streaming_worker_starts_from_stream_position() {
        let mut stream = fake_stream(100);
        stream.seek(SeekFrom::Start(25)).unwrap();

        let worker = StreamingWorker::new(stream, StreamingWorkerOptions::default()).unwrap();

        assert_eq!(worker.stats().playback_position, 25);
        assert_eq!(worker.stats().source_position, 25);
        assert_eq!(worker.stream.position(), 25);
    }

    #[test]
    fn streaming_worker_read_available_drains_buffer() {
        let mut stream = fake_stream(100);
        stream.seek(SeekFrom::Start(25)).unwrap();

        let mut worker = StreamingWorker::new(stream, StreamingWorkerOptions::default()).unwrap();

        worker
            .state
            .push_source_chunk(Bytes::from_static(b"abcdef"))
            .unwrap();

        assert_eq!(worker.read_available(2).unwrap().as_ref(), b"ab");
        assert_eq!(worker.stats().playback_position, 27);
        assert_eq!(worker.stats().buffered, 4);

        assert_eq!(worker.read_available(10).unwrap().as_ref(), b"cdef");
        assert_eq!(worker.stats().playback_position, 31);
        assert_eq!(worker.stats().buffered, 0);
    }

    #[test]
    fn streaming_worker_seek_clears_buffer_and_syncs_stream_position() {
        let mut worker =
            StreamingWorker::new(fake_stream(100), StreamingWorkerOptions::default()).unwrap();

        worker
            .state
            .push_source_chunk(Bytes::from_static(b"abcdef"))
            .unwrap();

        assert_eq!(worker.read_available(2).unwrap().as_ref(), b"ab");
        assert_eq!(worker.seek(SeekFrom::Start(50)).unwrap(), 50);

        assert_eq!(worker.stats().playback_position, 50);
        assert_eq!(worker.stats().source_position, 50);
        assert_eq!(worker.stats().buffered, 0);
        assert_eq!(worker.stream.position(), 50);
    }

    #[test]
    fn streaming_worker_is_finished_after_source_eof_and_buffer_drained() {
        let mut worker =
            StreamingWorker::new(fake_stream(3), StreamingWorkerOptions::default()).unwrap();

        worker
            .state
            .push_source_chunk(Bytes::from_static(b"abc"))
            .unwrap();

        assert!(!worker.is_finished());

        assert_eq!(worker.read_available(10).unwrap().as_ref(), b"abc");
        assert!(worker.is_finished());
    }

    #[test]
    fn streaming_worker_state_buffered_goal_request_reads_until_goal() {
        let options =
            StreamingWorkerOptions::new(StreamOptions::new(16, 4).unwrap(), 2, 12).unwrap();
        let mut state = StreamingWorkerState::new(100, options).unwrap();

        assert_eq!(
            state.next_buffered_read_requests(10, 1),
            vec![StreamingWorkerReadRequest { offset: 0, len: 4 }]
        );

        state
            .push_source_chunk(Bytes::from_static(b"abcd"))
            .unwrap();

        assert_eq!(
            state.next_buffered_read_requests(10, 1),
            vec![StreamingWorkerReadRequest { offset: 4, len: 4 }]
        );

        state
            .push_source_chunk(Bytes::from_static(b"efgh"))
            .unwrap();

        assert_eq!(
            state.next_buffered_read_requests(10, 1),
            vec![StreamingWorkerReadRequest { offset: 8, len: 2 }]
        );
    }

    #[test]
    fn streaming_worker_state_buffered_goal_request_stops_when_goal_reached() {
        let options =
            StreamingWorkerOptions::new(StreamOptions::new(16, 4).unwrap(), 2, 12).unwrap();
        let mut state = StreamingWorkerState::new(100, options).unwrap();

        state
            .push_source_chunk(Bytes::from_static(b"abcd"))
            .unwrap();

        assert!(state.next_buffered_read_requests(4, 1).is_empty());
        assert!(state.next_buffered_read_requests(3, 1).is_empty());
    }

    #[test]
    fn streaming_worker_state_buffered_goal_request_is_capped_by_high_watermark() {
        let options =
            StreamingWorkerOptions::new(StreamOptions::new(16, 4).unwrap(), 2, 8).unwrap();
        let mut state = StreamingWorkerState::new(100, options).unwrap();

        state
            .push_source_chunk(Bytes::from_static(b"abcd"))
            .unwrap();
        state
            .push_source_chunk(Bytes::from_static(b"efgh"))
            .unwrap();

        assert!(state.next_buffered_read_requests(16, 1).is_empty());
    }

    #[test]
    fn streaming_worker_state_buffered_goal_request_respects_file_end() {
        let options =
            StreamingWorkerOptions::new(StreamOptions::new(16, 4).unwrap(), 2, 12).unwrap();
        let state = StreamingWorkerState::new(3, options).unwrap();

        assert_eq!(
            state.next_buffered_read_requests(10, 1),
            vec![StreamingWorkerReadRequest { offset: 0, len: 3 }]
        );
    }

    #[test]
    fn streaming_worker_stats_reports_prefill_need() {
        let stats = StreamingWorkerStats {
            playback_position: 0,
            source_position: 0,
            file_size: 100,
            buffered: 4,
            buffered_chunks: 1,
            low_watermark: 4,
            high_watermark: 12,
            source_eof: false,
        };

        assert!(stats.should_prefill());
        assert_eq!(stats.high_watermark_deficit(), 8);

        let enough_buffered = StreamingWorkerStats {
            buffered: 8,
            ..stats
        };

        assert!(!enough_buffered.should_prefill());
        assert_eq!(enough_buffered.high_watermark_deficit(), 4);

        let eof = StreamingWorkerStats {
            source_eof: true,
            ..stats
        };

        assert!(!eof.should_prefill());
    }

    #[test]
    fn streaming_worker_state_reports_prefill_need_when_buffer_reaches_low_watermark() {
        let options =
            StreamingWorkerOptions::new(StreamOptions::new(16, 4).unwrap(), 4, 12).unwrap();
        let mut state = StreamingWorkerState::new(100, options).unwrap();

        assert!(state.should_prefill());

        state
            .push_source_chunk(Bytes::from_static(b"abcdefgh"))
            .unwrap();
        assert!(!state.should_prefill());

        assert_eq!(state.pop_read(4).unwrap().as_ref(), b"abcd");
        assert!(state.should_prefill());
    }

    #[test]
    fn smb_media_stream_options_use_playback_friendly_defaults() {
        let options = SmbMediaStreamOptions::default();

        assert_eq!(options.initial_buffer_size, 1024 * 1024);
        assert_eq!(options.worker_options.low_watermark, 1024 * 1024);
        assert_eq!(options.worker_options.high_watermark, 2 * 1024 * 1024);
        assert_eq!(
            options.worker_options.stream_options.read_ahead_capacity,
            8 * 1024 * 1024
        );
        assert!(options.validate().is_ok());
    }

    #[test]
    fn smb_media_stream_wraps_streaming_worker_state() {
        let options = SmbMediaStreamOptions::new(
            StreamingWorkerOptions::new(StreamOptions::new(16, 4).unwrap(), 4, 12).unwrap(),
            4,
        )
        .unwrap();

        let mut stream = SmbMediaStream::new(fake_stream(100), options).unwrap();

        assert_eq!(stream.stats().playback_position, 0);
        assert_eq!(stream.stats().source_position, 0);
        assert!(stream.should_prefill());

        stream
            .worker
            .state
            .push_source_chunk(Bytes::from_static(b"abcdefgh"))
            .unwrap();

        assert_eq!(stream.stats().buffered, 8);
        assert!(!stream.should_prefill());

        assert_eq!(stream.worker.read_available(4).unwrap().as_ref(), b"abcd");
        assert!(stream.should_prefill());
    }

    #[test]
    fn smb_media_stream_seek_resets_buffer_and_positions() {
        let mut stream =
            SmbMediaStream::new(fake_stream(100), SmbMediaStreamOptions::default()).unwrap();

        stream
            .worker
            .state
            .push_source_chunk(Bytes::from_static(b"abcdef"))
            .unwrap();

        assert_eq!(stream.worker.read_available(2).unwrap().as_ref(), b"ab");
        assert_eq!(stream.seek(SeekFrom::Start(50)).unwrap(), 50);

        assert_eq!(stream.stats().playback_position, 50);
        assert_eq!(stream.stats().source_position, 50);
        assert_eq!(stream.stats().buffered, 0);
    }

    #[test]
    fn media_filter_rejects_common_system_entries() {
        for name in [
            ".DS_Store",
            ".Trashes",
            ".Spotlight-V100",
            ".fseventsd",
            "@eaDir",
            "$RECYCLE.BIN",
            "System Volume Information",
            "Temporary Items",
            "Network Trash Folder",
            "TheVolumeSettingsFolder",
        ] {
            assert!(!is_media_entry(&fake_dir_entry(name, true)), "{name}");
        }
    }
    #[test]
    fn media_extra_kind_recognizes_explicit_flags() {
        assert_eq!(
            media_extra_kind("Ice.Age - Gone Nutty --short.mkv"),
            Some(MediaExtraKind::Short)
        );
        assert_eq!(
            media_extra_kind("Deleted Scenes --deleted.mkv"),
            Some(MediaExtraKind::DeletedScene)
        );
        assert_eq!(
            media_extra_kind("Making Of --featurette.mkv"),
            Some(MediaExtraKind::Featurette)
        );
        assert_eq!(
            media_extra_kind("Trailer --trailer.mkv"),
            Some(MediaExtraKind::Trailer)
        );
        assert_eq!(
            media_extra_kind("Interview --interview.mkv"),
            Some(MediaExtraKind::Interview)
        );
        assert_eq!(
            media_extra_kind("Bonus Clip --bonus.mkv"),
            Some(MediaExtraKind::Bonus)
        );
    }

    #[test]
    fn explicit_short_flag_keeps_movie_folder_collapsible() {
        let entries = vec![
            MediaEntry {
                name: "Ice Age.mkv".to_owned(),
                size: 10_000,
                kind: MediaKind::Video,
            },
            MediaEntry {
                name: "Ice.Age - Gone Nutty --short.mkv".to_owned(),
                size: 2_000,
                kind: MediaKind::Video,
            },
        ];

        let summary = MediaFolderSummary::from_entries(&entries);

        assert_eq!(summary.primary_videos, vec![0]);
        assert_eq!(summary.main_video, Some(0));
        assert_eq!(summary.extras, vec![1]);
        assert!(summary.can_collapse_to_movie());
    }

    #[test]
    fn unflagged_second_video_prevents_movie_folder_collapse() {
        let entries = vec![
            MediaEntry {
                name: "Ice Age.mkv".to_owned(),
                size: 10_000,
                kind: MediaKind::Video,
            },
            MediaEntry {
                name: "Ice.Age - Gone Nutty.mkv".to_owned(),
                size: 2_000,
                kind: MediaKind::Video,
            },
        ];

        let summary = MediaFolderSummary::from_entries(&entries);

        assert_eq!(summary.primary_videos, vec![0, 1]);
        assert_eq!(summary.main_video, Some(0));
        assert!(summary.extras.is_empty());
        assert!(!summary.can_collapse_to_movie());
    }

    #[test]
    fn streaming_worker_options_reject_invalid_pipeline_depth() {
        let stream_options = StreamOptions::default();

        assert!(StreamingWorkerOptions::new_with_pipeline_depth(stream_options, 1, 2, 0,).is_err());

        assert!(
            StreamingWorkerOptions::new_with_pipeline_depth(stream_options, 1, 2, 17,).is_err()
        );
    }

    #[test]
    fn streaming_worker_state_builds_pipelined_read_requests() {
        let options = StreamingWorkerOptions::new_with_pipeline_depth(
            StreamOptions::new(1024 * 1024, 65534).unwrap(),
            256 * 1024,
            512 * 1024,
            4,
        )
        .unwrap();

        let state = StreamingWorkerState::new(1024 * 1024, options).unwrap();
        let requests = state.next_buffered_read_requests(256 * 1024, 4);

        assert_eq!(requests.len(), 4);
        assert_eq!(requests[0].offset, 0);
        assert_eq!(requests[0].len, 65534);
        assert_eq!(requests[1].offset, 65534);
    }
}
