use std::ffi::{CStr, CString, c_char};
use std::fmt::Write as _;
use std::panic;
use std::ptr;
use std::time::Duration;

use crate::smb::SMB_LEGACY_READ_MAX;
use crate::{Auth, Cifs, Error, Handle, MediaEntry, Share};

const BRIDGE_VERSION: &str = "cifs-client-stream tvOS bridge ok";
const DEFAULT_PROBE_TIMEOUT_MS: u64 = 15_000;
const DEFAULT_LIST_ENTRIES: usize = 30;
const MAX_LIST_ENTRIES: usize = 100;

pub struct CifsClientStreamSession {
    runtime: tokio::runtime::Runtime,
    inner: Option<CifsClientStreamSessionInner>,
}

struct CifsClientStreamSessionInner {
    cifs: Cifs,
    share: Share,
    host: String,
    share_name: String,
}

pub struct CifsClientStreamMedia {
    session: *mut CifsClientStreamSession,
    handle: Option<Handle>,
    file_size: u64,
}

#[unsafe(no_mangle)]
pub extern "C" fn cifs_client_stream_bridge_version() -> *mut c_char {
    string_into_raw(BRIDGE_VERSION)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cifs_client_stream_smb_probe(
    host: *const c_char,
    share: *const c_char,
    user: *const c_char,
    password: *const c_char,
    timeout_ms: u64,
) -> *mut c_char {
    let result = panic::catch_unwind(|| unsafe {
        smb_probe_from_c(host, share, user, password, timeout_ms)
    });

    match result {
        Ok(Ok(message)) => string_into_raw(&message),
        Ok(Err(message)) => string_into_raw(&format!("SMB probe failed: {message}")),
        Err(_) => string_into_raw("SMB probe failed: Rust panic was caught"),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cifs_client_stream_smb_list(
    host: *const c_char,
    share: *const c_char,
    user: *const c_char,
    password: *const c_char,
    path: *const c_char,
    max_entries: u64,
    timeout_ms: u64,
) -> *mut c_char {
    let result = panic::catch_unwind(|| unsafe {
        smb_list_from_c(host, share, user, password, path, max_entries, timeout_ms)
    });

    match result {
        Ok(Ok(message)) => string_into_raw(&message),
        Ok(Err(message)) => string_into_raw(&format!("SMB list failed: {message}")),
        Err(_) => string_into_raw("SMB list failed: Rust panic was caught"),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cifs_client_stream_smb_list_json(
    host: *const c_char,
    share: *const c_char,
    user: *const c_char,
    password: *const c_char,
    path: *const c_char,
    max_entries: u64,
    timeout_ms: u64,
) -> *mut c_char {
    let result = panic::catch_unwind(|| unsafe {
        smb_list_json_from_c(host, share, user, password, path, max_entries, timeout_ms)
    });

    match result {
        Ok(Ok(message)) => string_into_raw(&message),
        Ok(Err(message)) => string_into_raw(&json_error("smb_list_json", &message)),
        Err(_) => string_into_raw(&json_error("smb_list_json", "Rust panic was caught")),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cifs_client_stream_session_open(
    host: *const c_char,
    share: *const c_char,
    user: *const c_char,
    password: *const c_char,
    timeout_ms: u64,
    out_message: *mut *mut c_char,
) -> *mut CifsClientStreamSession {
    let result = panic::catch_unwind(|| unsafe {
        session_open_from_c(host, share, user, password, timeout_ms)
    });

    match result {
        Ok(Ok((session, message))) => {
            unsafe {
                set_out_message(out_message, &message);
            }
            session
        }
        Ok(Err(message)) => {
            unsafe {
                set_out_message(out_message, &format!("SMB session open failed: {message}"));
            }
            std::ptr::null_mut()
        }
        Err(_) => {
            unsafe {
                set_out_message(
                    out_message,
                    "SMB session open failed: Rust panic was caught",
                );
            }
            std::ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cifs_client_stream_session_list_json(
    session: *mut CifsClientStreamSession,
    path: *const c_char,
    max_entries: u64,
    timeout_ms: u64,
) -> *mut c_char {
    let result = panic::catch_unwind(|| unsafe {
        session_list_json_from_c(session, path, max_entries, timeout_ms)
    });

    match result {
        Ok(Ok(message)) => string_into_raw(&message),
        Ok(Err(message)) => string_into_raw(&json_error("session_list_json", &message)),
        Err(_) => string_into_raw(&json_error("session_list_json", "Rust panic was caught")),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cifs_client_stream_session_open_media(
    session: *mut CifsClientStreamSession,
    path: *const c_char,
    timeout_ms: u64,
    out_message: *mut *mut c_char,
) -> *mut CifsClientStreamMedia {
    let result =
        panic::catch_unwind(|| unsafe { session_open_media_from_c(session, path, timeout_ms) });

    match result {
        Ok(Ok((media, message))) => {
            unsafe {
                set_out_message(out_message, &message);
            }
            media
        }
        Ok(Err(message)) => {
            unsafe {
                set_out_message(out_message, &format!("SMB media open failed: {message}"));
            }
            std::ptr::null_mut()
        }
        Err(_) => {
            unsafe {
                set_out_message(out_message, "SMB media open failed: Rust panic was caught");
            }
            std::ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cifs_client_stream_media_read_at(
    media: *mut CifsClientStreamMedia,
    offset: u64,
    buffer: *mut u8,
    buffer_len: u64,
    timeout_ms: u64,
    out_message: *mut *mut c_char,
) -> i64 {
    let result = panic::catch_unwind(|| unsafe {
        media_read_at_from_c(media, offset, buffer, buffer_len, timeout_ms)
    });

    match result {
        Ok(Ok(bytes_read)) => bytes_read,
        Ok(Err(message)) => {
            unsafe {
                set_out_message(out_message, &message);
            }
            -1
        }
        Err(_) => {
            unsafe {
                set_out_message(out_message, "SMB media read failed: Rust panic was caught");
            }
            -1
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cifs_client_stream_media_size(media: *mut CifsClientStreamMedia) -> u64 {
    if media.is_null() {
        return 0;
    }

    unsafe { (*media).file_size }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cifs_client_stream_media_close(media: *mut CifsClientStreamMedia) {
    if media.is_null() {
        return;
    }

    let _ = panic::catch_unwind(|| unsafe {
        let mut media = Box::from_raw(media);

        let Some(handle) = media.handle.take() else {
            return;
        };

        let Some(session) = media.session.as_mut() else {
            return;
        };

        let Some(inner) = session.inner.as_mut() else {
            return;
        };

        let _ = session
            .runtime
            .block_on(async { inner.cifs.close(handle).await });
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cifs_client_stream_session_close(session: *mut CifsClientStreamSession) {
    if session.is_null() {
        return;
    }

    let _ = panic::catch_unwind(|| unsafe {
        let mut session = Box::from_raw(session);

        let Some(inner) = session.inner.take() else {
            return;
        };

        let CifsClientStreamSessionInner {
            mut cifs, share, ..
        } = inner;

        let _ = session
            .runtime
            .block_on(async move { cifs.umount(share).await });
    });
}

/// Frees a string returned by cifs-client-stream FFI functions.
///
/// # Safety
///
/// `ptr` must be either null or a pointer previously returned by a
/// cifs-client-stream FFI function that transfers string ownership to the
/// caller. The pointer must not be freed more than once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cifs_client_stream_free_string(ptr: *mut c_char) {
    if ptr.is_null() {
        return;
    }

    unsafe {
        drop(CString::from_raw(ptr));
    }
}

unsafe fn smb_probe_from_c(
    host: *const c_char,
    share: *const c_char,
    user: *const c_char,
    password: *const c_char,
    timeout_ms: u64,
) -> Result<String, String> {
    let host = unsafe { required_c_string(host, "host") }?;
    let share = unsafe { required_c_string(share, "share") }?;
    let user = unsafe { optional_c_string(user) }?;
    let password = unsafe { optional_c_string(password) }?;

    let timeout = timeout_from_ms(timeout_ms);
    let runtime = build_runtime()?;

    runtime.block_on(run_smb_probe(host, share, user, password, timeout))
}

unsafe fn smb_list_from_c(
    host: *const c_char,
    share: *const c_char,
    user: *const c_char,
    password: *const c_char,
    path: *const c_char,
    max_entries: u64,
    timeout_ms: u64,
) -> Result<String, String> {
    let host = unsafe { required_c_string(host, "host") }?;
    let share = unsafe { required_c_string(share, "share") }?;
    let user = unsafe { optional_c_string(user) }?;
    let password = unsafe { optional_c_string(password) }?;
    let path = unsafe { optional_c_string(path) }?;

    let max_entries = normalize_max_entries(max_entries);
    let timeout = timeout_from_ms(timeout_ms);
    let runtime = build_runtime()?;

    runtime.block_on(run_smb_list(
        host,
        share,
        user,
        password,
        path,
        max_entries,
        timeout,
    ))
}

unsafe fn smb_list_json_from_c(
    host: *const c_char,
    share: *const c_char,
    user: *const c_char,
    password: *const c_char,
    path: *const c_char,
    max_entries: u64,
    timeout_ms: u64,
) -> Result<String, String> {
    let host = unsafe { required_c_string(host, "host") }?;
    let share = unsafe { required_c_string(share, "share") }?;
    let user = unsafe { optional_c_string(user) }?;
    let password = unsafe { optional_c_string(password) }?;
    let path = unsafe { optional_c_string(path) }?;

    let max_entries = normalize_max_entries(max_entries);
    let timeout = timeout_from_ms(timeout_ms);
    let runtime = build_runtime()?;

    runtime.block_on(run_smb_list_json(
        host,
        share,
        user,
        password,
        path,
        max_entries,
        timeout,
    ))
}

unsafe fn session_open_from_c(
    host: *const c_char,
    share: *const c_char,
    user: *const c_char,
    password: *const c_char,
    timeout_ms: u64,
) -> Result<(*mut CifsClientStreamSession, String), String> {
    let host = unsafe { required_c_string(host, "host") }?;
    let share = unsafe { required_c_string(share, "share") }?;
    let user = unsafe { optional_c_string(user) }?;
    let password = unsafe { optional_c_string(password) }?;

    let timeout = timeout_from_ms(timeout_ms);
    let runtime = build_runtime()?;

    let inner = runtime.block_on(open_session(
        host.clone(),
        share.clone(),
        user,
        password,
        timeout,
    ))?;

    let session = Box::new(CifsClientStreamSession {
        runtime,
        inner: Some(inner),
    });

    let message = format!("SMB session opened: host={host} share={share}");

    Ok((Box::into_raw(session), message))
}

unsafe fn session_list_json_from_c(
    session: *mut CifsClientStreamSession,
    path: *const c_char,
    max_entries: u64,
    timeout_ms: u64,
) -> Result<String, String> {
    let session = unsafe {
        session
            .as_mut()
            .ok_or_else(|| "session pointer is null".to_owned())?
    };

    let path = unsafe { optional_c_string(path) }?;
    let max_entries = normalize_max_entries(max_entries);
    let timeout = timeout_from_ms(timeout_ms);

    let inner = session
        .inner
        .as_mut()
        .ok_or_else(|| "session is already closed".to_owned())?;

    session
        .runtime
        .block_on(run_session_list_json(inner, path, max_entries, timeout))
}

unsafe fn session_open_media_from_c(
    session: *mut CifsClientStreamSession,
    path: *const c_char,
    timeout_ms: u64,
) -> Result<(*mut CifsClientStreamMedia, String), String> {
    let session = unsafe {
        session
            .as_mut()
            .ok_or_else(|| "session pointer is null".to_owned())?
    };

    let path = unsafe { required_c_string(path, "path") }?;
    let timeout = timeout_from_ms(timeout_ms);

    let inner = session
        .inner
        .as_mut()
        .ok_or_else(|| "session is already closed".to_owned())?;

    let handle =
        session
            .runtime
            .block_on(open_media_handle_for_session(inner, path.clone(), timeout))?;

    let file_size = handle.size;

    let media = Box::new(CifsClientStreamMedia {
        session,
        handle: Some(handle),
        file_size,
    });

    let message = format!("SMB media opened: path={path} size={file_size}");

    Ok((Box::into_raw(media), message))
}

unsafe fn media_read_at_from_c(
    media: *mut CifsClientStreamMedia,
    offset: u64,
    buffer: *mut u8,
    buffer_len: u64,
    timeout_ms: u64,
) -> Result<i64, String> {
    let media = unsafe {
        media
            .as_mut()
            .ok_or_else(|| "media pointer is null".to_owned())?
    };

    if buffer.is_null() {
        return Err("output buffer pointer is null".to_owned());
    }

    let requested_len = usize::try_from(buffer_len).unwrap_or(usize::MAX);

    if requested_len == 0 {
        return Ok(0);
    }

    let Some(handle) = media.handle.as_ref() else {
        return Err("media handle is already closed".to_owned());
    };

    let session = unsafe { media.session.as_mut() }
        .ok_or_else(|| "media parent session pointer is null".to_owned())?;

    let inner = session
        .inner
        .as_mut()
        .ok_or_else(|| "session is already closed".to_owned())?;

    let timeout = timeout_from_ms(timeout_ms);

    let bytes = session.runtime.block_on(read_media_handle_at_for_session(
        inner,
        handle,
        offset,
        requested_len,
        timeout,
    ))?;

    let bytes_to_copy = bytes.len().min(requested_len);

    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr(), buffer, bytes_to_copy);
    }

    let bytes_read = i64::try_from(bytes_to_copy)
        .map_err(|error| format!("media read size conversion failed: {error}"))?;

    Ok(bytes_read)
}

async fn open_media_handle_for_session(
    inner: &mut CifsClientStreamSessionInner,
    path: String,
    timeout: Duration,
) -> Result<Handle, String> {
    tokio::time::timeout(timeout, inner.cifs.openfile(&inner.share, &path))
        .await
        .map_err(|_| "timeout while opening SMB media handle".to_owned())?
        .map_err(format_error)
}

async fn read_media_handle_at_for_session(
    inner: &mut CifsClientStreamSessionInner,
    handle: &Handle,
    offset: u64,
    requested_len: usize,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    if requested_len == 0 {
        return Ok(Vec::new());
    }

    if offset >= handle.size {
        return Ok(Vec::new());
    }

    let remaining_file_bytes = handle.size.saturating_sub(offset);
    let mut remaining =
        requested_len.min(usize::try_from(remaining_file_bytes).unwrap_or(usize::MAX));

    if remaining == 0 {
        return Ok(Vec::new());
    }

    let mut cursor = offset;
    let mut output = Vec::with_capacity(remaining);

    while remaining > 0 {
        let read_len = remaining.min(SMB_LEGACY_READ_MAX as usize);
        let read_len_u32 = u32::try_from(read_len)
            .map_err(|error| format!("media read length conversion failed: {error}"))?;

        let chunk = tokio::time::timeout(timeout, inner.cifs.read_at(handle, cursor, read_len_u32))
            .await
            .map_err(|_| "timeout while reading SMB media handle".to_owned())?
            .map_err(format_error)?;

        if chunk.is_empty() {
            break;
        }

        let chunk_len = chunk.len();

        output.extend_from_slice(&chunk);

        cursor = cursor.saturating_add(
            u64::try_from(chunk_len)
                .map_err(|error| format!("media read offset conversion failed: {error}"))?,
        );

        remaining = remaining.saturating_sub(chunk_len);
    }

    Ok(output)
}

fn timeout_from_ms(timeout_ms: u64) -> Duration {
    let timeout_ms = if timeout_ms == 0 {
        DEFAULT_PROBE_TIMEOUT_MS
    } else {
        timeout_ms
    };

    Duration::from_millis(timeout_ms)
}

fn normalize_max_entries(max_entries: u64) -> usize {
    let max_entries = usize::try_from(max_entries).unwrap_or(usize::MAX);

    if max_entries == 0 {
        DEFAULT_LIST_ENTRIES
    } else {
        max_entries.min(MAX_LIST_ENTRIES)
    }
}

fn build_runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("failed to create Tokio runtime: {error}"))
}

async fn open_session(
    host: String,
    share_name: String,
    user: String,
    password: String,
    timeout: Duration,
) -> Result<CifsClientStreamSessionInner, String> {
    let auth = auth_for_host(&host, &user, &password);

    let mut cifs = Cifs::open_timeout(&host, None, auth, timeout)
        .await
        .map_err(format_error)?;

    let mount_path = format!("\\\\{host}\\{share_name}");

    let share = tokio::time::timeout(timeout, cifs.mount(&mount_path))
        .await
        .map_err(|_| "timeout while mounting SMB share".to_owned())?
        .map_err(format_error)?;

    Ok(CifsClientStreamSessionInner {
        cifs,
        share,
        host,
        share_name,
    })
}

async fn run_smb_probe(
    host: String,
    share_name: String,
    user: String,
    password: String,
    timeout: Duration,
) -> Result<String, String> {
    let auth = auth_for_host(&host, &user, &password);

    let mut cifs = Cifs::open_timeout(&host, None, auth, timeout)
        .await
        .map_err(format_error)?;

    let mount_path = format!("\\\\{host}\\{share_name}");

    let mounted_share = tokio::time::timeout(timeout, cifs.mount(&mount_path))
        .await
        .map_err(|_| "timeout while mounting SMB share".to_owned())?
        .map_err(format_error)?;

    let entries = list_media_entries(&mut cifs, &mounted_share, "/", timeout).await?;

    let folders = entries.iter().filter(|entry| entry.is_folder()).count();
    let audio = entries.iter().filter(|entry| entry.is_audio()).count();
    let video = entries.iter().filter(|entry| entry.is_video()).count();

    let umount_result = tokio::time::timeout(timeout, cifs.umount(mounted_share)).await;

    let mut message = format!(
        "SMB probe ok: host={host} share={share_name} entries={} folders={folders} audio={audio} video={video}",
        entries.len()
    );

    append_umount_warning(&mut message, umount_result);

    Ok(message)
}

async fn run_smb_list(
    host: String,
    share_name: String,
    user: String,
    password: String,
    path: String,
    max_entries: usize,
    timeout: Duration,
) -> Result<String, String> {
    let auth = auth_for_host(&host, &user, &password);

    let mut cifs = Cifs::open_timeout(&host, None, auth, timeout)
        .await
        .map_err(format_error)?;

    let mount_path = format!("\\\\{host}\\{share_name}");

    let mounted_share = tokio::time::timeout(timeout, cifs.mount(&mount_path))
        .await
        .map_err(|_| "timeout while mounting SMB share".to_owned())?
        .map_err(format_error)?;

    let entries = list_media_entries(&mut cifs, &mounted_share, &path, timeout).await?;

    let normalized_path = normalize_display_path(&path);
    let mut message = format!(
        "SMB list ok: host={host} share={share_name} path={normalized_path} entries={} showing={}\n",
        entries.len(),
        entries.len().min(max_entries)
    );

    for entry in entries.iter().take(max_entries) {
        writeln!(&mut message, "{}", format_entry_line(entry))
            .expect("writing to String should not fail");
    }

    if entries.len() > max_entries {
        writeln!(
            &mut message,
            "... {} more entries",
            entries.len().saturating_sub(max_entries)
        )
        .expect("writing to String should not fail");
    }

    let umount_result = tokio::time::timeout(timeout, cifs.umount(mounted_share)).await;
    append_umount_warning(&mut message, umount_result);

    Ok(message)
}

async fn run_smb_list_json(
    host: String,
    share_name: String,
    user: String,
    password: String,
    path: String,
    max_entries: usize,
    timeout: Duration,
) -> Result<String, String> {
    let auth = auth_for_host(&host, &user, &password);

    let mut cifs = Cifs::open_timeout(&host, None, auth, timeout)
        .await
        .map_err(format_error)?;

    let mount_path = format!("\\\\{host}\\{share_name}");

    let mounted_share = tokio::time::timeout(timeout, cifs.mount(&mount_path))
        .await
        .map_err(|_| "timeout while mounting SMB share".to_owned())?
        .map_err(format_error)?;

    let started = std::time::Instant::now();

    let entries = list_media_entries(&mut cifs, &mounted_share, &path, timeout).await?;

    let elapsed_ms = started.elapsed().as_millis() as u64;

    let umount_warning = match tokio::time::timeout(timeout, cifs.umount(mounted_share)).await {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(format_error(error)),
        Err(_) => Some("timeout".to_owned()),
    };

    Ok(format_list_json(
        &host,
        &share_name,
        &path,
        &entries,
        max_entries,
        umount_warning.as_deref(),
        Some(elapsed_ms),
    ))
}

async fn run_session_list_json(
    inner: &mut CifsClientStreamSessionInner,
    path: String,
    max_entries: usize,
    timeout: Duration,
) -> Result<String, String> {
    let started = std::time::Instant::now();
    let entries = list_media_entries(&mut inner.cifs, &inner.share, &path, timeout).await?;
    let elapsed_ms = started.elapsed().as_millis() as u64;

    Ok(format_list_json(
        &inner.host,
        &inner.share_name,
        &path,
        &entries,
        max_entries,
        None,
        Some(elapsed_ms),
    ))
}

async fn list_media_entries(
    cifs: &mut Cifs,
    share: &Share,
    path: &str,
    timeout: Duration,
) -> Result<Vec<MediaEntry>, String> {
    let pattern = list_pattern(path);

    let mut reader = cifs
        .open_dir_reader_timeout(share, &pattern, timeout)
        .await
        .map_err(format_error)?;

    reader
        .next_media_entries_timeout(cifs, timeout)
        .await
        .map_err(format_error)
        .map(|entries| entries.unwrap_or_default())
}

fn auth_for_host(host: &str, user: &str, password: &str) -> Option<Auth> {
    if user.is_empty() {
        None
    } else {
        Some(Auth::new(user, "APEXTVOS", host, password))
    }
}

fn list_pattern(path: &str) -> String {
    let path = path.trim();

    if path.is_empty() || path == "/" {
        "*".to_owned()
    } else {
        format!("{}/*", path.trim_end_matches('/'))
    }
}

fn normalize_display_path(path: &str) -> &str {
    let path = path.trim();

    if path.is_empty() { "/" } else { path }
}

fn format_entry_line(entry: &MediaEntry) -> String {
    let kind = entry_kind(entry);

    if entry.is_folder() {
        format!("[{kind}] {}", entry.name)
    } else {
        format!("[{kind}] {} size={}", entry.name, entry.size)
    }
}

fn append_umount_warning(
    message: &mut String,
    umount_result: Result<Result<(), Error>, tokio::time::error::Elapsed>,
) {
    match umount_result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            message.push_str(&format!("; umount warning: {}", format_error(error)));
        }
        Err(_) => {
            message.push_str("; umount warning: timeout");
        }
    }
}

fn format_list_json(
    host: &str,
    share: &str,
    path: &str,
    entries: &[MediaEntry],
    max_entries: usize,
    umount_warning: Option<&str>,
    elapsed_ms: Option<u64>,
) -> String {
    let normalized_path = normalize_display_path(path);
    let returned = entries.len().min(max_entries);

    let mut out = String::new();

    out.push_str("{\"status\":\"ok\"");
    out.push_str(",\"host\":");
    push_json_string(&mut out, host);
    out.push_str(",\"share\":");
    push_json_string(&mut out, share);
    out.push_str(",\"path\":");
    push_json_string(&mut out, normalized_path);
    out.push_str(",\"entries_total\":");
    out.push_str(&entries.len().to_string());
    out.push_str(",\"entries_returned\":");
    out.push_str(&returned.to_string());
    if let Some(elapsed_ms) = elapsed_ms {
        out.push_str(",\"elapsed_ms\":");
        out.push_str(&elapsed_ms.to_string());
    }

    if let Some(warning) = umount_warning {
        out.push_str(",\"umount_warning\":");
        push_json_string(&mut out, warning);
    }

    out.push_str(",\"entries\":[");

    for (index, entry) in entries.iter().take(max_entries).enumerate() {
        if index > 0 {
            out.push(',');
        }

        out.push('{');

        out.push_str("\"name\":");
        push_json_string(&mut out, &entry.name);

        out.push_str(",\"kind\":");
        push_json_string(&mut out, entry_kind(entry));

        out.push_str(",\"size\":");
        out.push_str(&entry.size.to_string());

        out.push('}');
    }

    out.push_str("]}");

    out
}

fn entry_kind(entry: &MediaEntry) -> &'static str {
    if entry.is_folder() {
        "folder"
    } else if entry.is_audio() {
        "audio"
    } else if entry.is_video() {
        "video"
    } else {
        "media"
    }
}

fn json_error(operation: &str, message: &str) -> String {
    let mut out = String::new();

    out.push_str("{\"status\":\"error\"");
    out.push_str(",\"operation\":");
    push_json_string(&mut out, operation);
    out.push_str(",\"message\":");
    push_json_string(&mut out, message);
    out.push('}');

    out
}

fn push_json_string(out: &mut String, value: &str) {
    out.push('"');

    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch <= '\u{1f}' => {
                write!(out, "\\u{:04x}", ch as u32).expect("writing to String should not fail");
            }
            ch => out.push(ch),
        }
    }

    out.push('"');
}

unsafe fn required_c_string(ptr: *const c_char, name: &str) -> Result<String, String> {
    let value = unsafe { optional_c_string(ptr) }?;

    if value.trim().is_empty() {
        Err(format!("{name} must not be empty"))
    } else {
        Ok(value)
    }
}

unsafe fn optional_c_string(ptr: *const c_char) -> Result<String, String> {
    if ptr.is_null() {
        return Ok(String::new());
    }

    unsafe { CStr::from_ptr(ptr) }
        .to_str()
        .map(str::to_owned)
        .map_err(|error| format!("invalid UTF-8 string from Swift/C: {error}"))
}

unsafe fn set_out_message(out_message: *mut *mut c_char, message: &str) {
    if out_message.is_null() {
        return;
    }

    unsafe {
        *out_message = string_into_raw(message);
    }
}

fn format_error(error: Error) -> String {
    format!(
        "{} [kind={:?}, retryable={}, timeout={}, connection_lost={}]",
        error,
        error.kind(),
        error.is_retryable(),
        error.is_timeout(),
        error.is_connection_lost()
    )
}

fn string_into_raw(value: &str) -> *mut c_char {
    CString::new(value)
        .expect("FFI strings must not contain interior NUL bytes")
        .into_raw()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_version_returns_c_string() {
        let ptr = cifs_client_stream_bridge_version();

        assert!(!ptr.is_null());

        let value = unsafe { CStr::from_ptr(ptr) }
            .to_str()
            .expect("bridge version should be UTF-8");

        assert_eq!(value, BRIDGE_VERSION);

        unsafe {
            cifs_client_stream_free_string(ptr);
        }
    }

    #[test]
    fn free_string_accepts_null() {
        unsafe {
            cifs_client_stream_free_string(std::ptr::null_mut());
        }
    }

    #[test]
    fn list_pattern_handles_root() {
        assert_eq!(list_pattern(""), "*");
        assert_eq!(list_pattern("/"), "*");
    }

    #[test]
    fn list_pattern_handles_nested_path() {
        assert_eq!(list_pattern("/Movies"), "/Movies/*");
        assert_eq!(list_pattern("/Movies/"), "/Movies/*");
    }

    #[test]
    fn json_string_escapes_special_characters() {
        let mut out = String::new();

        push_json_string(&mut out, "A\"B\\C\nКириллица");

        assert_eq!(out, "\"A\\\"B\\\\C\\nКириллица\"");
    }
}
