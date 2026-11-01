//! Finder's right-click menu.
//!
//! "Upload with FioTransfer" and "Create Watch Folder in FioTransfer"
//! (NSServices in `build/darwin/Info.plist`, served by
//! `mac/services_darwin.m`, unchanged from the Go app). The provider calls
//! `goService` back, so that entry point is defined here and forwards to the
//! app through `set_service_hook`.
//!
//! Threading: `goService` arrives on the main thread from the AppKit service
//! provider.

use std::ffi::{c_char, c_int};
use std::sync::Mutex;

use crate::strings::from_c_str;

#[cfg(target_os = "macos")]
extern "C" {
    fn fioServicesInit();
}

/// What a right-click request does with the request's JSON paths.
type Service = fn(i32, &str);

static SERVICE: Mutex<Option<Service>> = Mutex::new(None);

/// Hands the service requests to the app; `kind` is 1 for the watch-folder
/// service and 0 for upload, and the JSON paths are the pasteboard's.
pub fn set_service_hook(service: Service) {
    *SERVICE.lock().unwrap_or_else(|e| e.into_inner()) = Some(service);
}

/// Called from services_darwin.m with the pasteboard's paths as a JSON array.
/// `kind` is 1 for the watch-folder service and 0 for upload.
#[no_mangle]
#[allow(non_snake_case)]
pub extern "C" fn goService(kind: c_int, paths: *const c_char) {
    let service = *SERVICE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(f) = service {
        f(kind, &from_c_str(paths));
    }
}

/// Installs the Finder services provider.
#[cfg(target_os = "macos")]
pub fn services_init() {
    unsafe { fioServicesInit() };
}

/// Installs the Finder services provider; there is none outside macOS.
#[cfg(not(target_os = "macos"))]
pub fn services_init() {}
