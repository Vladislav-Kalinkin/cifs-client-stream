mod error;
mod netbios;
mod ntlm;
mod smb;
mod utils;
pub mod win;

use std::collections::VecDeque;
use std::future::Future;
use std::io::SeekFrom;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use lazy_static::lazy_static;
use regex::Regex;

use crate::netbios::NetBios;
use crate::smb::info::{Cmd, Flags2, Info, Status};
use crate::smb::{msg, reply, trans, trans2, Capabilities, DirInfo, SMB_READ_MAX};
use crate::utils::sanitize_path;
use crate::win::{ExtFileAttr, FileAttr, NTStatus, NotifyAction};

const DEFAULT_READ_AHEAD_CAPACITY: usize = 8 * 1024 * 1024;
const DEFAULT_STREAM_CHUNK_SIZE: u16 = SMB_READ_MAX;
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
pub use trans::NotifyMode;

#[derive(Debug)]
pub struct Cifs {
    netbios: NetBios,
    auth: Auth,

    max_smb_size: usize,
    use_unicode: bool,
    uid: u16,
    mid: u16,
}

#[derive(Debug)]
pub struct FileStream {
    handle: Handle,
    position: u64,
}

#[derive(Debug)]
pub struct ReadAhead {
    stream: FileStream,
    position: u64,
    chunks: VecDeque<Bytes>,
    buffered: usize,
    options: StreamOptions,
    eof: bool,
}

#[derive(Debug)]
pub struct DirectoryReader {
    tid: u16,
    sid: u16,
    pending: Option<Vec<DirInfo>>,
    last_file: Option<String>,
    end: bool,
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
pub struct ReadAheadStats {
    pub position: u64,
    pub source_position: u64,
    pub file_size: u64,
    pub buffered: usize,
    pub buffered_chunks: usize,
    pub read_ahead_capacity: usize,
    pub chunk_size: u16,
    pub eof: bool,
}

impl ReadAheadStats {
    pub fn remaining(&self) -> u64 {
        self.file_size.saturating_sub(self.position)
    }

    pub fn buffer_free(&self) -> usize {
        self.read_ahead_capacity.saturating_sub(self.buffered)
    }

    pub fn prefetched(&self) -> u64 {
        self.source_position.saturating_sub(self.position)
    }

    pub fn is_buffering(&self) -> bool {
        !self.eof && self.buffered < self.read_ahead_capacity
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamOptions {
    pub read_ahead_capacity: usize,
    pub chunk_size: u16,
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
    pub fn new(read_ahead_capacity: usize, chunk_size: u16) -> Result<Self, Error> {
        let options = Self {
            read_ahead_capacity,
            chunk_size,
        };
        options.validate()?;
        Ok(options)
    }

    pub fn validate(&self) -> Result<(), Error> {
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

    fn normalized(self) -> Self {
        Self {
            read_ahead_capacity: self.read_ahead_capacity,
            chunk_size: self.chunk_size.min(SMB_READ_MAX),
        }
    }
}

impl FileStream {
    pub fn new(handle: Handle) -> Self {
        Self {
            handle,
            position: 0,
        }
    }

    pub fn handle(&self) -> &Handle {
        &self.handle
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

    pub fn is_eof(&self) -> bool {
        self.position >= self.size()
    }

    pub fn seek(&mut self, pos: SeekFrom) -> Result<u64, Error> {
        self.position = seek_position(self.size(), self.position, pos)?;
        Ok(self.position)
    }

    pub async fn read_next(
        &mut self,
        cifs: &mut Cifs,
        max_count: u16,
    ) -> Result<Option<Bytes>, Error> {
        if self.is_eof() || max_count == 0 {
            return Ok(None);
        }

        let remaining = self.size() - self.position;
        let count = read_count_for(remaining.min(u64::from(max_count)));
        let chunk = cifs.read_at(&self.handle, self.position, count).await?;
        if chunk.is_empty() {
            return Ok(None);
        }
        if chunk.len() > usize::from(count) || chunk.len() as u64 > remaining {
            return Err(Error::InternalError(
                "server returned more data than requested".to_owned(),
            ));
        }

        self.position += chunk.len() as u64;
        Ok(Some(chunk))
    }

    pub async fn read_next_timeout(
        &mut self,
        cifs: &mut Cifs,
        max_count: u16,
        timeout: Duration,
    ) -> Result<Option<Bytes>, Error> {
        with_timeout(timeout, self.read_next(cifs, max_count)).await
    }
}

impl ReadAhead {
    pub fn new(stream: FileStream, capacity: usize, chunk_size: u16) -> Self {
        let position = stream.position();
        let options = StreamOptions {
            read_ahead_capacity: capacity,
            chunk_size,
        }
        .normalized();

        Self {
            stream,
            position,
            chunks: VecDeque::new(),
            buffered: 0,
            options,
            eof: false,
        }
    }

    pub fn with_options(stream: FileStream, options: StreamOptions) -> Result<Self, Error> {
        options.validate()?;
        let position = stream.position();
        let options = options.normalized();

        Ok(Self {
            stream,
            position,
            chunks: VecDeque::new(),
            buffered: 0,
            options,
            eof: false,
        })
    }

    pub fn stream(&self) -> &FileStream {
        &self.stream
    }

    pub fn into_stream(self) -> FileStream {
        self.stream
    }

    pub fn position(&self) -> u64 {
        self.position
    }

    pub fn source_position(&self) -> u64 {
        self.stream.position()
    }

    pub fn options(&self) -> StreamOptions {
        self.options
    }

    pub fn buffered_len(&self) -> usize {
        self.buffered
    }

    pub fn buffered_chunks(&self) -> usize {
        self.chunks.len()
    }

    pub fn buffer_free(&self) -> usize {
        self.options
            .read_ahead_capacity
            .saturating_sub(self.buffered)
    }

    pub fn stats(&self) -> ReadAheadStats {
        ReadAheadStats {
            position: self.position,
            source_position: self.source_position(),
            file_size: self.stream.size(),
            buffered: self.buffered,
            buffered_chunks: self.buffered_chunks(),
            read_ahead_capacity: self.options.read_ahead_capacity,
            chunk_size: self.options.chunk_size,
            eof: self.is_eof(),
        }
    }

    pub fn is_eof(&self) -> bool {
        self.eof && self.chunks.is_empty()
    }

    pub fn seek(&mut self, pos: SeekFrom) -> Result<u64, Error> {
        self.chunks.clear();
        self.buffered = 0;
        self.eof = false;
        self.position = self.stream.seek(pos)?;
        Ok(self.position)
    }

    pub fn discard_buffer(&mut self) {
        self.chunks.clear();
        self.buffered = 0;
        self.position = self.stream.position();
    }

    pub async fn fill(&mut self, cifs: &mut Cifs) -> Result<(), Error> {
        while self.can_buffer_more() && !self.eof {
            let count = self.next_read_count();
            match self.stream.read_next(cifs, count).await? {
                Some(chunk) => self.push_chunk(chunk),
                None => self.eof = true,
            }
        }

        Ok(())
    }

    pub async fn fill_timeout(&mut self, cifs: &mut Cifs, timeout: Duration) -> Result<(), Error> {
        with_timeout(timeout, self.fill(cifs)).await
    }

    pub async fn next(&mut self, cifs: &mut Cifs) -> Result<Option<Bytes>, Error> {
        self.fill(cifs).await?;
        Ok(self.pop_chunk())
    }

    pub async fn next_timeout(
        &mut self,
        cifs: &mut Cifs,
        timeout: Duration,
    ) -> Result<Option<Bytes>, Error> {
        with_timeout(timeout, self.next(cifs)).await
    }

    pub async fn read(&mut self, cifs: &mut Cifs, max_len: usize) -> Result<Option<Bytes>, Error> {
        if max_len == 0 {
            return Ok(None);
        }

        self.fill(cifs).await?;
        Ok(self.pop_bytes(max_len))
    }

    pub async fn read_timeout(
        &mut self,
        cifs: &mut Cifs,
        max_len: usize,
        timeout: Duration,
    ) -> Result<Option<Bytes>, Error> {
        with_timeout(timeout, self.read(cifs, max_len)).await
    }

    pub async fn read_block(
        &mut self,
        cifs: &mut Cifs,
        max_len: usize,
    ) -> Result<Option<Bytes>, Error> {
        if max_len == 0 {
            return Ok(None);
        }

        self.fill(cifs).await?;
        Ok(self.pop_block(max_len))
    }

    pub async fn read_block_timeout(
        &mut self,
        cifs: &mut Cifs,
        max_len: usize,
        timeout: Duration,
    ) -> Result<Option<Bytes>, Error> {
        with_timeout(timeout, self.read_block(cifs, max_len)).await
    }

    fn can_buffer_more(&self) -> bool {
        self.options.read_ahead_capacity > self.buffered && self.options.chunk_size > 0
    }

    fn next_read_count(&self) -> u16 {
        let free = self.options.read_ahead_capacity - self.buffered;
        read_count_for(free.min(usize::from(self.options.chunk_size)) as u64)
    }

    fn push_chunk(&mut self, chunk: Bytes) {
        self.buffered += chunk.len();
        self.chunks.push_back(chunk);
    }

    fn pop_chunk(&mut self) -> Option<Bytes> {
        let chunk = self.chunks.pop_front()?;
        self.buffered -= chunk.len();
        self.position += chunk.len() as u64;
        Some(chunk)
    }

    fn pop_bytes(&mut self, max_len: usize) -> Option<Bytes> {
        if max_len == 0 {
            return None;
        }

        let mut chunk = self.chunks.pop_front()?;
        if chunk.len() > max_len {
            let out = chunk.split_to(max_len);
            self.buffered -= out.len();
            self.position += out.len() as u64;
            self.chunks.push_front(chunk);
            return Some(out);
        }

        self.buffered -= chunk.len();
        self.position += chunk.len() as u64;
        Some(chunk)
    }

    fn pop_block(&mut self, max_len: usize) -> Option<Bytes> {
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

pub fn retain_media_entries(entries: &mut Vec<DirInfo>) {
    entries.retain(is_media_entry);
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

    pub async fn openfile(&mut self, share: &Share, path: &str) -> Result<Handle, Error> {
        self.command(msg::Open::file_ro(share.tid, sanitize_path(path)))
            .await
    }

    pub async fn open_stream(&mut self, share: &Share, path: &str) -> Result<FileStream, Error> {
        Ok(FileStream::new(self.openfile(share, path).await?))
    }

    pub async fn open_read_ahead(
        &mut self,
        share: &Share,
        path: &str,
        capacity: usize,
        chunk_size: u16,
    ) -> Result<ReadAhead, Error> {
        self.open_read_ahead_with_options(share, path, StreamOptions::new(capacity, chunk_size)?)
            .await
    }

    pub async fn open_read_ahead_with_options(
        &mut self,
        share: &Share,
        path: &str,
        options: StreamOptions,
    ) -> Result<ReadAhead, Error> {
        ReadAhead::with_options(self.open_stream(share, path).await?, options)
    }

    pub async fn opendir(&mut self, share: &Share, path: &str) -> Result<Handle, Error> {
        self.command(msg::Open::dir(share.tid, sanitize_path(path)))
            .await
    }

    pub async fn close_ref(&mut self, file: &Handle) -> Result<(), Error> {
        let _: reply::Close = self.command(msg::Close::handle(file)).await?;
        Ok(())
    }

    pub async fn close(&mut self, file: Handle) -> Result<(), Error> {
        self.close_ref(&file).await
    }

    pub async fn close_stream(&mut self, stream: FileStream) -> Result<(), Error> {
        self.close(stream.into_handle()).await
    }

    pub async fn close_read_ahead(&mut self, read_ahead: ReadAhead) -> Result<(), Error> {
        self.close_stream(read_ahead.into_stream()).await
    }

    pub async fn read(&mut self, file: &Handle, offset: u64) -> Result<Bytes, Error> {
        self.read_at(file, offset, SMB_READ_MAX).await
    }

    pub async fn read_at(
        &mut self,
        file: &Handle,
        offset: u64,
        count: u16,
    ) -> Result<Bytes, Error> {
        if count == 0 {
            return Ok(Bytes::new());
        }

        let reply: reply::Read = self.command(msg::Read::handle(file, offset, count)).await?;
        Ok(reply.data)
    }

    pub async fn read_at_timeout(
        &mut self,
        file: &Handle,
        offset: u64,
        count: u16,
        timeout: Duration,
    ) -> Result<Bytes, Error> {
        with_timeout(timeout, self.read_at(file, offset, count)).await
    }

    pub async fn read_range_at(
        &mut self,
        file: &Handle,
        offset: u64,
        len: u64,
    ) -> Result<Vec<Bytes>, Error> {
        let mut chunks = Vec::new();
        let mut read = 0;

        while read < len {
            let cursor = offset
                .checked_add(read)
                .ok_or_else(|| Error::InternalError("read offset overflow".to_owned()))?;
            let chunk = self
                .read_at(file, cursor, read_count_for(len - read))
                .await?;
            if chunk.is_empty() {
                break;
            }

            read += chunk.len() as u64;
            chunks.push(chunk);
        }

        Ok(chunks)
    }

    pub async fn read_range_at_timeout(
        &mut self,
        file: &Handle,
        offset: u64,
        len: u64,
        timeout: Duration,
    ) -> Result<Vec<Bytes>, Error> {
        with_timeout(timeout, self.read_range_at(file, offset, len)).await
    }

    pub async fn download(&mut self, share: &Share, path: &str) -> Result<Vec<u8>, Error> {
        let file = self.openfile(share, path).await?;

        let mut data = Vec::new();
        while (data.len() as u64) < file.size {
            let remaining = file.size - data.len() as u64;
            let chunk = self
                .read_at(&file, data.len() as u64, read_count_for(remaining))
                .await?;
            if chunk.is_empty() {
                return Err(Error::InternalError(
                    "server returned no data before EOF".to_owned(),
                ));
            }
            data.extend_from_slice(chunk.as_ref());
        }

        self.close(file).await?;
        Ok(data)
    }

    pub async fn notify(&mut self, handle: &Handle) -> Result<Vec<(String, NotifyAction)>, Error> {
        self.notify_about(handle, NotifyMode::all()).await
    }

    pub async fn notify_about(
        &mut self,
        handle: &Handle,
        what: NotifyMode,
    ) -> Result<Vec<(String, NotifyAction)>, Error> {
        // sub-command we want to run via SMB transact
        let cmd = trans::NotifySetup::new(handle.fid, what, false);

        // get sub-command response via transact
        tracing::debug!("waiting for {:?} notification", what);
        let reply: trans::Notification = self.transact(handle.tid, cmd).await?;

        Ok(reply.changes)
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

    /// receives a reply of type R and given mid.
    ///
    /// Note: for simplification this function will drop any response that
    /// does not match the given mid.
    async fn recv<R: reply::Reply>(&mut self, mid: u16) -> Result<R, Error> {
        // wait for a frame with the correct mid
        let (info, body) = loop {
            let mut frame = self.netbios.recv_message().await?;
            let info = Info::parse(&mut frame)?;
            if info.mid == mid {
                break (info, frame);
            }
        };

        // check command identifier
        if info.cmd != R::CMD {
            return Err(Error::UnexpectedReply(R::CMD, info.cmd));
        }

        // check status
        if let Status::Known(status) = info.status {
            match status {
                NTStatus::SUCCESS => (),
                NTStatus::MORE_PROCESSING if info.cmd == Cmd::SessionSetup => (),

                _ => return Err(Error::ServerError(info.status)),
            }
        } else {
            return Err(Error::ServerError(info.status));
        }

        // finally parse the response body into our desired result
        R::parse(info, body).map_err(|e| e.into())
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

    async fn transact<C, R>(&mut self, tid: u16, cmd: C) -> Result<R, Error>
    where
        C: trans::SubCmd,
        R: trans::SubReply,
    {
        let msg = msg::Transact::new(tid, cmd);
        let reply: reply::Transact<R> = self.command(msg).await?;

        Ok(reply.subcmd)
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

fn read_count_for(remaining: u64) -> u16 {
    remaining.min(u64::from(SMB_READ_MAX)) as u16
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

fn is_hidden_entry(entry: &DirInfo) -> bool {
    entry.filename.starts_with('.')
        || entry
            .attributes
            .intersects(ExtFileAttr::HIDDEN | ExtFileAttr::SYSTEM)
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
    pub share: &'a str,
    pub path: Option<&'a str>,
}

///
/// Helper function that decodes an SMB URI and returns a CifsConfig
///
pub fn resolve_smb_uri<'a>(uri: &'a str) -> Result<CifsConfig<'a>, Error> {
    lazy_static! {
        static ref URI_REGEX: Regex =
            Regex::new(r"smb://((?P<domain>\w+);)?((?P<user>[\w\.\+_-]+)(:(?P<passwd>[^@]*))?@)?(?P<host>\w[\w\.-]*)(:(?P<port>\d+))?/(?P<share>[\w\._-]+)(/(?P<path>.*))?")
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

        share: uri_match.name("share").ok_or(Error::InvalidUri)?.as_str(),

        path: uri_match.name("path").map(|m| m.as_str()),
    };

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::{
        is_media_entry, read_count_for, resolve_smb_uri, retain_media_entries, seek_position,
        sort_dir_entries, ReadAhead, StreamOptions, SMB_READ_MAX,
    };
    use bytes::Bytes;
    use chrono::Local;
    use std::io::SeekFrom;

    #[test]
    fn read_count_is_capped_to_smb_limit() {
        assert_eq!(read_count_for(0), 0);
        assert_eq!(read_count_for(1), 1);
        assert_eq!(read_count_for(u64::from(SMB_READ_MAX)), SMB_READ_MAX);
        assert_eq!(read_count_for(u64::from(SMB_READ_MAX) + 1), SMB_READ_MAX);
    }

    #[test]
    fn stream_options_have_streaming_defaults() {
        let options = StreamOptions::default();

        assert_eq!(
            options.read_ahead_capacity,
            super::DEFAULT_READ_AHEAD_CAPACITY
        );
        assert_eq!(options.chunk_size, SMB_READ_MAX);
        assert!(options.validate().is_ok());
    }

    #[test]
    fn stream_options_reject_zero_values() {
        assert!(StreamOptions::new(0, 1).is_err());
        assert!(StreamOptions::new(1, 0).is_err());
    }

    #[test]
    fn stream_options_cap_chunk_size_to_smb_limit() {
        let buffer = ReadAhead::with_options(
            fake_stream(100),
            StreamOptions {
                read_ahead_capacity: usize::from(SMB_READ_MAX) + 10,
                chunk_size: u16::MAX,
            },
        )
        .unwrap();

        assert_eq!(buffer.next_read_count(), SMB_READ_MAX);
    }

    #[test]
    fn read_ahead_exposes_normalized_options() {
        let buffer = ReadAhead::new(fake_stream(100), 10, u16::MAX);

        assert_eq!(
            buffer.options(),
            StreamOptions {
                read_ahead_capacity: 10,
                chunk_size: SMB_READ_MAX,
            }
        );
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
    fn read_ahead_tracks_buffered_bytes() {
        let mut buffer = ReadAhead::new(fake_stream(100), 10, 4);

        buffer.push_chunk(Bytes::from_static(b"abcd"));
        buffer.push_chunk(Bytes::from_static(b"ef"));

        assert_eq!(buffer.buffered_len(), 6);
        assert_eq!(buffer.buffered_chunks(), 2);
        assert_eq!(buffer.buffer_free(), 4);
        assert_eq!(buffer.position(), 0);
        assert_eq!(buffer.pop_chunk().unwrap().as_ref(), b"abcd");
        assert_eq!(buffer.position(), 4);
        assert_eq!(buffer.buffered_len(), 2);
        assert_eq!(buffer.buffered_chunks(), 1);
        assert_eq!(buffer.buffer_free(), 8);
        assert_eq!(buffer.pop_chunk().unwrap().as_ref(), b"ef");
        assert_eq!(buffer.position(), 6);
        assert_eq!(buffer.buffered_len(), 0);
        assert_eq!(buffer.buffered_chunks(), 0);
        assert_eq!(buffer.buffer_free(), 10);
    }

    #[test]
    fn read_ahead_seek_clears_buffer() {
        let mut buffer = ReadAhead::new(fake_stream(100), 10, 4);
        buffer.push_chunk(Bytes::from_static(b"abcd"));

        assert_eq!(buffer.seek(SeekFrom::Start(25)).unwrap(), 25);
        assert_eq!(buffer.buffered_len(), 0);
        assert_eq!(buffer.position(), 25);
        assert_eq!(buffer.source_position(), 25);
        assert_eq!(buffer.pop_chunk(), None);
        assert!(!buffer.is_eof());
    }

    #[test]
    fn read_ahead_discard_buffer_drops_prefetched_data() {
        let mut stream = fake_stream(100);
        stream.seek(SeekFrom::Start(8)).unwrap();
        let mut buffer = ReadAhead::new(stream, 10, 4);

        buffer.push_chunk(Bytes::from_static(b"abcd"));
        buffer.stream.seek(SeekFrom::Start(12)).unwrap();
        buffer.discard_buffer();

        assert_eq!(buffer.position(), 12);
        assert_eq!(buffer.source_position(), 12);
        assert_eq!(buffer.buffered_len(), 0);
        assert_eq!(buffer.buffered_chunks(), 0);
        assert_eq!(buffer.pop_chunk(), None);
    }

    #[test]
    fn read_ahead_next_read_count_respects_capacity() {
        let mut buffer = ReadAhead::new(fake_stream(100), 10, 8);

        assert_eq!(buffer.next_read_count(), 8);
        buffer.push_chunk(Bytes::from_static(b"abcdef"));
        assert_eq!(buffer.next_read_count(), 4);
    }

    #[test]
    fn read_ahead_pop_bytes_splits_front_chunk() {
        let mut buffer = ReadAhead::new(fake_stream(100), 10, 8);
        buffer.push_chunk(Bytes::from_static(b"abcdef"));

        assert_eq!(buffer.pop_bytes(2).unwrap().as_ref(), b"ab");
        assert_eq!(buffer.buffered_len(), 4);
        assert_eq!(buffer.position(), 2);
        assert_eq!(buffer.pop_bytes(10).unwrap().as_ref(), b"cdef");
        assert_eq!(buffer.buffered_len(), 0);
        assert_eq!(buffer.position(), 6);
    }

    #[test]
    fn read_ahead_pop_bytes_rejects_zero_len() {
        let mut buffer = ReadAhead::new(fake_stream(100), 10, 8);
        buffer.push_chunk(Bytes::from_static(b"abcdef"));

        assert_eq!(buffer.pop_bytes(0), None);
        assert_eq!(buffer.buffered_len(), 6);
    }

    #[test]
    fn read_ahead_pop_block_combines_buffered_chunks() {
        let mut buffer = ReadAhead::new(fake_stream(100), 10, 8);
        buffer.push_chunk(Bytes::from_static(b"ab"));
        buffer.push_chunk(Bytes::from_static(b"cdef"));
        buffer.push_chunk(Bytes::from_static(b"gh"));

        assert_eq!(buffer.pop_block(5).unwrap().as_ref(), b"abcde");
        assert_eq!(buffer.position(), 5);
        assert_eq!(buffer.buffered_len(), 3);
        assert_eq!(buffer.pop_block(10).unwrap().as_ref(), b"fgh");
        assert_eq!(buffer.position(), 8);
        assert_eq!(buffer.buffered_len(), 0);
    }

    #[test]
    fn read_ahead_stats_report_playback_and_source_positions() {
        let mut stream = fake_stream(100);
        stream.seek(SeekFrom::Start(16)).unwrap();
        let mut buffer = ReadAhead::new(stream, 10, 8);

        buffer.push_chunk(Bytes::from_static(b"abcdef"));
        buffer.stream.seek(SeekFrom::Start(22)).unwrap();
        assert_eq!(buffer.pop_bytes(2).unwrap().as_ref(), b"ab");

        assert_eq!(
            buffer.stats(),
            super::ReadAheadStats {
                position: 18,
                source_position: 22,
                file_size: 100,
                buffered: 4,
                buffered_chunks: 1,
                read_ahead_capacity: 10,
                chunk_size: 8,
                eof: false,
            }
        );
        assert_eq!(buffer.stats().remaining(), 82);
        assert_eq!(buffer.stats().buffer_free(), 6);
        assert_eq!(buffer.stats().prefetched(), 4);
        assert!(buffer.stats().is_buffering());
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
    fn test_uri() {
        let uri = "smb://localhost/myshare/this/is/a/path";
        let config = resolve_smb_uri(uri).unwrap();

        assert_eq!(config.domain, None);
        assert_eq!(config.user, None);
        assert_eq!(config.password, None);
        assert_eq!(config.hostname, "localhost");
        assert_eq!(config.port, None);
        assert_eq!(config.share, "myshare");
        assert_eq!(config.path, Some("this/is/a/path"));

        let uri = "smb://www.example.org:31337/foo";
        let config = resolve_smb_uri(uri).unwrap();
        assert_eq!(config.domain, None);
        assert_eq!(config.user, None);
        assert_eq!(config.password, None);
        assert_eq!(config.hostname, "www.example.org");
        assert_eq!(config.port, Some(31337));
        assert_eq!(config.share, "foo");
        assert_eq!(config.path, None);

        let uri = "smb://127.0.0.1:445/share/foo";
        let config = resolve_smb_uri(uri).unwrap();
        assert_eq!(config.domain, None);
        assert_eq!(config.user, None);
        assert_eq!(config.password, None);
        assert_eq!(config.hostname, "127.0.0.1");
        assert_eq!(config.port, Some(445));
        assert_eq!(config.share, "share");
        assert_eq!(config.path, Some("foo"));

        let uri = "smb://anonymous@localhost/public";
        let config = resolve_smb_uri(uri).unwrap();
        assert_eq!(config.domain, None);
        assert_eq!(config.user, Some("anonymous"));
        assert_eq!(config.password, None);
        assert_eq!(config.hostname, "localhost");
        assert_eq!(config.port, None);
        assert_eq!(config.share, "public");
        assert_eq!(config.path, None);

        let uri = "smb://john:secret@localhost/closed";
        let config = resolve_smb_uri(uri).unwrap();
        assert_eq!(config.domain, None);
        assert_eq!(config.user, Some("john"));
        assert_eq!(config.password, Some("secret"));
        assert_eq!(config.hostname, "localhost");
        assert_eq!(config.port, None);
        assert_eq!(config.share, "closed");
        assert_eq!(config.path, None);

        let uri = "smb://WORKGROUP;foo/bar";
        let config = resolve_smb_uri(uri).unwrap();
        assert_eq!(config.domain, Some("WORKGROUP"));
        assert_eq!(config.user, None);
        assert_eq!(config.password, None);
        assert_eq!(config.hostname, "foo");
        assert_eq!(config.port, None);
        assert_eq!(config.share, "bar");
        assert_eq!(config.path, None);

        let uri = "smb://NOSTROMO;Ellen.Ripley:100375@Mother:445/interface/special/order/937.txt";
        let config = resolve_smb_uri(uri).unwrap();
        assert_eq!(config.domain, Some("NOSTROMO"));
        assert_eq!(config.user, Some("Ellen.Ripley"));
        assert_eq!(config.password, Some("100375"));
        assert_eq!(config.hostname, "Mother");
        assert_eq!(config.port, Some(445));
        assert_eq!(config.share, "interface");
        assert_eq!(config.path, Some("special/order/937.txt"));
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

    fn filenames(entries: &[super::DirInfo]) -> Vec<&str> {
        entries
            .iter()
            .map(|entry| entry.filename.as_str())
            .collect()
    }
}
