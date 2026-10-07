//! Location for the tracker's time sheet ("Calling in from home", and the
//! call-in / wrap check), which asks with the browser's own
//! `navigator.geolocation`.
//!
//! A browser asks the person; inside the app the webview asks the shell,
//! and before this the shell never answered, so the tracker reported "Your
//! device didn't share where it is". Now the tracker's own pages (https on
//! the tracker host) get location; any other page, the Frame.io player
//! included, does not. The operating system still asks the person the first
//! time (macOS: "CoolerBox Tracker would like to use your location"), and
//! the tracker's own page explains what it keeps.
//!
//! - Windows (WebView2) and Linux (WebKitGTK): Tauri's permission handler.
//! - macOS: Tauri does not route WKWebView's location request yet, so the
//!   shell adds WebKit's geolocation decision methods to the webview's UI
//!   delegate (see `macos`). Info.plist explains the use; the entitlement
//!   lets the hardened runtime reach Location Services.

use tauri::webview::{PermissionKind, PermissionResponse};

use crate::APP_HOST;

/// Whether a page at `scheme://host` may have the person's location.
pub(crate) fn origin_may_locate(scheme: &str, host: &str) -> bool {
    scheme == "https" && host == APP_HOST
}

/// Tauri's permission handler for the main window (Windows and Linux):
/// location for the tracker's own pages only; everything else as before.
pub(crate) fn decide(page: Option<&url::Url>, kind: PermissionKind) -> PermissionResponse {
    if kind != PermissionKind::Geolocation {
        return PermissionResponse::Default;
    }
    match page {
        Some(url) if origin_may_locate(url.scheme(), url.host_str().unwrap_or("")) => {
            PermissionResponse::Allow
        }
        _ => PermissionResponse::Deny,
    }
}

/// macOS: answer WKWebView's location requests for the main window.
#[cfg(target_os = "macos")]
pub(crate) fn enable<R: tauri::Runtime>(window: &tauri::WebviewWindow<R>) {
    let _ = window.with_webview(|webview| {
        // SAFETY: on the main thread (with_webview), with the live WKWebView.
        unsafe { macos::install(webview.inner().cast()) }
    });
}

#[cfg(target_os = "macos")]
mod macos {
    //! WebKit asks its UI delegate whether a page may have the location
    //! through `_webView:requestGeolocationPermissionForOrigin:…` (macOS 12+)
    //! or, on older systems, `…ForFrame:…`. With neither implemented the
    //! answer is no. wry's delegate implements neither, so both are added to
    //! its class here, and the delegate is set again because WebKit notes
    //! which methods a delegate has when it is set.

    use std::ffi::{c_char, CStr};

    use block2::Block;
    use objc2::ffi;
    use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
    use objc2::{msg_send, sel};

    /// WKPermissionDecision.
    const GRANT: isize = 1;
    const DENY: isize = 2;

    unsafe fn text(value: *mut AnyObject) -> String {
        if value.is_null() {
            return String::new();
        }
        let utf8: *const c_char = msg_send![value, UTF8String];
        if utf8.is_null() {
            return String::new();
        }
        CStr::from_ptr(utf8).to_string_lossy().into_owned()
    }

    /// Whether a WKSecurityOrigin is the tracker's own.
    unsafe fn allowed(origin: *mut AnyObject) -> bool {
        if origin.is_null() {
            return false;
        }
        let scheme: *mut AnyObject = msg_send![origin, protocol];
        let host: *mut AnyObject = msg_send![origin, host];
        super::origin_may_locate(&text(scheme), &text(host))
    }

    unsafe extern "C-unwind" fn for_origin(
        _this: *mut AnyObject,
        _sel: Sel,
        _webview: *mut AnyObject,
        origin: *mut AnyObject,
        _frame: *mut AnyObject,
        decide: *mut Block<dyn Fn(isize)>,
    ) {
        if let Some(decide) = decide.as_ref() {
            decide.call((if allowed(origin) { GRANT } else { DENY },));
        }
    }

    unsafe extern "C-unwind" fn for_frame(
        _this: *mut AnyObject,
        _sel: Sel,
        _webview: *mut AnyObject,
        frame: *mut AnyObject,
        decide: *mut Block<dyn Fn(Bool)>,
    ) {
        let origin: *mut AnyObject =
            if frame.is_null() { std::ptr::null_mut() } else { msg_send![frame, securityOrigin] };
        if let Some(decide) = decide.as_ref() {
            decide.call((Bool::new(allowed(origin)),));
        }
    }

    pub(super) unsafe fn install(webview: *mut AnyObject) {
        if webview.is_null() {
            return;
        }
        let delegate: *mut AnyObject = msg_send![webview, UIDelegate];
        if delegate.is_null() {
            return;
        }
        let class = ffi::object_getClass(delegate) as *mut AnyClass;
        let origin_imp: unsafe extern "C-unwind" fn(
            *mut AnyObject,
            Sel,
            *mut AnyObject,
            *mut AnyObject,
            *mut AnyObject,
            *mut Block<dyn Fn(isize)>,
        ) = for_origin;
        let frame_imp: unsafe extern "C-unwind" fn(
            *mut AnyObject,
            Sel,
            *mut AnyObject,
            *mut AnyObject,
            *mut Block<dyn Fn(Bool)>,
        ) = for_frame;
        // Adding fails harmlessly if the class already has the method (the
        // window is built once; a second call changes nothing).
        let _ = ffi::class_addMethod(
            class,
            sel!(_webView:requestGeolocationPermissionForOrigin:initiatedByFrame:decisionHandler:),
            std::mem::transmute::<_, Imp>(origin_imp),
            c"v@:@@@@?".as_ptr(),
        );
        let _ = ffi::class_addMethod(
            class,
            sel!(_webView:requestGeolocationPermissionForFrame:decisionHandler:),
            std::mem::transmute::<_, Imp>(frame_imp),
            c"v@:@@@?".as_ptr(),
        );
        let _: () = msg_send![webview, setUIDelegate: delegate];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(raw: &str) -> url::Url {
        raw.parse().expect("test URL parses")
    }

    #[test]
    fn tracker_pages_get_location() {
        let page = url("https://tracker.coolerboxbrothers.com/time-sheet");
        assert_eq!(decide(Some(&page), PermissionKind::Geolocation), PermissionResponse::Allow);
        assert!(origin_may_locate("https", "tracker.coolerboxbrothers.com"));
    }

    #[test]
    fn other_pages_do_not() {
        for raw in [
            "https://next.frame.io/share/abc",
            "http://tracker.coolerboxbrothers.com/time-sheet",
            "https://tracker.coolerboxbrothers.com.evil.example/",
            "https://accounts.google.com/",
        ] {
            assert_eq!(
                decide(Some(&url(raw)), PermissionKind::Geolocation),
                PermissionResponse::Deny,
                "{raw}"
            );
        }
        assert_eq!(decide(None, PermissionKind::Geolocation), PermissionResponse::Deny);
    }

    #[test]
    fn other_permissions_are_left_alone() {
        let page = url("https://tracker.coolerboxbrothers.com/");
        assert_eq!(decide(Some(&page), PermissionKind::Camera), PermissionResponse::Default);
    }
}
