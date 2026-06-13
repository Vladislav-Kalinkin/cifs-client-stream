use std::ffi::{CStr, CString, c_char};
use std::panic;
use std::time::Duration;

use crate::{Auth, Cifs, Error};

const BRIDGE_VERSION: &str = "cifs-client-stream tvOS bridge ok";
const DEFAULT_PROBE_TIMEOUT_MS: u64 = 15_000;

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

    let timeout_ms = if timeout_ms == 0 {
        DEFAULT_PROBE_TIMEOUT_MS
    } else {
        timeout_ms
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("failed to create Tokio runtime: {error}"))?;

    runtime.block_on(run_smb_probe(
        host,
        share,
        user,
        password,
        Duration::from_millis(timeout_ms),
    ))
}

async fn run_smb_probe(
    host: String,
    share_name: String,
    user: String,
    password: String,
    timeout: Duration,
) -> Result<String, String> {
    let auth = if user.is_empty() {
        None
    } else {
        Some(Auth::new(&user, "APEXTVOS", &host, &password))
    };

    let mut cifs = Cifs::open_timeout(&host, None, auth, timeout)
        .await
        .map_err(format_error)?;

    let mount_path = format!("\\\\{host}\\{share_name}");

    let mounted_share = tokio::time::timeout(timeout, cifs.mount(&mount_path))
        .await
        .map_err(|_| "timeout while mounting SMB share".to_owned())?
        .map_err(format_error)?;

    let mut reader = cifs
        .open_dir_reader_timeout(&mounted_share, "*", timeout)
        .await
        .map_err(format_error)?;

    let entries = reader
        .next_media_entries_timeout(&mut cifs, timeout)
        .await
        .map_err(format_error)?
        .unwrap_or_default();

    let folders = entries.iter().filter(|entry| entry.is_folder()).count();
    let audio = entries.iter().filter(|entry| entry.is_audio()).count();
    let video = entries.iter().filter(|entry| entry.is_video()).count();

    let umount_result = tokio::time::timeout(timeout, cifs.umount(mounted_share)).await;

    let mut message = format!(
        "SMB probe ok: host={host} share={share_name} entries={} folders={folders} audio={audio} video={video}",
        entries.len()
    );

    match umount_result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            message.push_str(&format!("; umount warning: {}", format_error(error)));
        }
        Err(_) => {
            message.push_str("; umount warning: timeout");
        }
    }

    Ok(message)
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
}
