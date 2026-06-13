use std::ffi::{CString, c_char};

const BRIDGE_VERSION: &str = "cifs-client-stream tvOS bridge ok";

#[unsafe(no_mangle)]
pub extern "C" fn cifs_client_stream_bridge_version() -> *mut c_char {
    string_into_raw(BRIDGE_VERSION)
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

fn string_into_raw(value: &str) -> *mut c_char {
    CString::new(value)
        .expect("FFI string constants must not contain interior NUL bytes")
        .into_raw()
}
