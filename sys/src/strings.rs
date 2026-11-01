//! Strings across the C boundary.

use std::ffi::{c_char, CString};

/// A C string for one of the arguments. C can't take an interior NUL, so those
/// become spaces instead of cutting the string short.
pub fn cstr(s: &str) -> CString {
    match CString::new(s) {
        Ok(c) => c,
        Err(_) => {
            let cleaned: String = s.chars().map(|c| if c == '\0' { ' ' } else { c }).collect();
            CString::new(cleaned).unwrap_or_else(|_| CString::new("").expect("empty"))
        }
    }
}

/// A string the C side passed in; NULL reads as empty.
///
/// Called with the pointers a shim hands a `go*` callback (`mac/fio_export.h`
/// declares them): a NUL-terminated string that is live for the call.
pub fn from_c_str(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// A C string for a path or a script: text with an interior NUL can't cross,
/// and the call is made with an empty string instead.
pub fn cstr_or_empty(s: &str) -> CString {
    CString::new(s).unwrap_or_default()
}
