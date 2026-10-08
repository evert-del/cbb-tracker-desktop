//! Location for the tracker's time sheet ("Calling in from home", and the
//! call-in / wrap check), which asks with the browser's own
//! `navigator.geolocation.getCurrentPosition`.
//!
//! A browser asks the person; inside the app the webview asks the shell,
//! and before this nothing answered, so the tracker reported "Your device
//! didn't share where it is". Now the tracker's own pages (https on the
//! tracker host) get location and any other page, the Frame.io player
//! included, does not. The operating system still asks the person the first
//! time (macOS: "CoolerBox Tracker would like to use your location").
//!
//! - Windows (WebView2) and Linux (WebKitGTK) have their own location
//!   source: Tauri's permission handler answers for the tracker (`decide`).
//! - macOS: an app's WKWebView has no location source of its own (Safari
//!   brings its own), so the request never reached Location Services. There
//!   `geo_bridge.js` stands in for `navigator.geolocation` on the tracker's
//!   pages and hands each request to the shell as a `cbb-geo://` navigation
//!   (like the downloads); the shell asks Core Location and evals the
//!   answer back (`macos`). Info.plist explains the use; Entitlements.plist
//!   lets the hardened runtime reach Location Services.

use tauri::webview::{PermissionKind, PermissionResponse};

use crate::APP_HOST;

/// Whether a page at `scheme://host` may have the person's location.
pub(crate) fn origin_may_locate(scheme: &str, host: &str) -> bool {
    scheme == "https" && host == APP_HOST
}

/// Whether the page at `url` may have the person's location.
pub(crate) fn page_may_locate(url: &url::Url) -> bool {
    origin_may_locate(url.scheme(), url.host_str().unwrap_or(""))
}

/// Tauri's permission handler for the main window (Windows and Linux):
/// location for the tracker's own pages only; everything else as before.
pub(crate) fn decide(page: Option<&url::Url>, kind: PermissionKind) -> PermissionResponse {
    if kind != PermissionKind::Geolocation {
        return PermissionResponse::Default;
    }
    match page {
        Some(url) if page_may_locate(url) => PermissionResponse::Allow,
        _ => PermissionResponse::Deny,
    }
}

/// The request id in a `cbb-geo://get?id=7` hand-over from geo_bridge.js.
pub(crate) fn request_id(url: &url::Url) -> Option<u64> {
    if url.scheme() != "cbb-geo" {
        return None;
    }
    url.query_pairs().find(|(key, _)| key == "id").and_then(|(_, value)| value.parse().ok())
}

/// What a request ends in, as handed to the page (`window.__cbbGeoResult`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Answer {
    Position { latitude: f64, longitude: f64, accuracy: f64, timestamp_ms: f64 },
    /// GeolocationPositionError codes: 1 permission denied, 2 unavailable.
    Failed { code: u8, message: String },
}

/// The script that answers request `id` in the page.
pub(crate) fn answer_script(id: u64, answer: &Answer) -> String {
    let value = match answer {
        Answer::Position { latitude, longitude, accuracy, timestamp_ms } => serde_json::json!({
            "ok": true, "lat": latitude, "lon": longitude, "acc": accuracy, "ts": timestamp_ms,
        }),
        Answer::Failed { code, message } => serde_json::json!({
            "ok": false, "code": code, "message": message,
        }),
    };
    format!("window.__cbbGeoResult && window.__cbbGeoResult({id}, {value})")
}

/// macOS: answer request `id` from Core Location (main thread).
#[cfg(target_os = "macos")]
/// The answer goes back to the tracker window `label` that asked (each
/// window numbers its own requests).
pub(crate) fn request<R: tauri::Runtime>(app: &tauri::AppHandle<R>, label: &str, id: u64) {
    macos::deliver_to(app);
    let label = label.to_string();
    let _ = app.run_on_main_thread(move || {
        // SAFETY: on the main thread, which owns the location manager.
        unsafe { macos::start(label, id) }
    });
}

#[cfg(target_os = "macos")]
mod macos {
    //! Core Location through the Objective-C runtime: one CLLocationManager
    //! on the main thread with a small delegate class. Requests made while
    //! one is under way share its answer.

    use std::cell::Cell;
    use std::sync::{Mutex, OnceLock};

    use objc2::encode::{Encode, Encoding};
    use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, Sel};
    use objc2::{msg_send, sel};
    use tauri::{AppHandle, Manager, Runtime};

    use super::{answer_script, Answer};

    #[link(name = "CoreLocation", kind = "framework")]
    unsafe extern "C" {}

    /// CLLocationCoordinate2D.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Coordinate {
        latitude: f64,
        longitude: f64,
    }

    // SAFETY: matches CLLocationCoordinate2D's layout and encoding.
    unsafe impl Encode for Coordinate {
        const ENCODING: Encoding =
            Encoding::Struct("CLLocationCoordinate2D", &[f64::ENCODING, f64::ENCODING]);
    }

    /// kCLLocationAccuracyBest.
    const ACCURACY_BEST: f64 = -1.0;

    /// Requests waiting for the next answer.
    /// (window label, request id) of each request waiting for an answer.
    static PENDING: Mutex<Vec<(String, u64)>> = Mutex::new(Vec::new());

    /// Hands a script to a tracker window.
    static DELIVER: OnceLock<Box<dyn Fn(&str, String) + Send + Sync>> = OnceLock::new();

    thread_local! {
        /// The location manager (main thread only; kept for the app's life).
        static MANAGER: Cell<*mut AnyObject> = const { Cell::new(std::ptr::null_mut()) };
    }

    pub(super) fn deliver_to<R: Runtime>(app: &AppHandle<R>) {
        let app = app.clone();
        let _ = DELIVER.set(Box::new(move |label: &str, script: String| {
            if let Some(window) = app.get_webview_window(label) {
                let _ = window.eval(script.as_str());
            }
        }));
    }

    fn answer_all(answer: Answer) {
        let ids = PENDING.lock().map(|mut ids| std::mem::take(&mut *ids)).unwrap_or_default();
        if let Some(deliver) = DELIVER.get() {
            for (label, id) in ids {
                deliver(&label, answer_script(id, &answer));
            }
        }
    }

    fn denied() -> Answer {
        Answer::Failed {
            code: 1,
            message: "Location is turned off for CoolerBox Tracker in System Settings → Privacy & Security → Location Services.".into(),
        }
    }

    fn unavailable(message: &str) -> Answer {
        Answer::Failed { code: 2, message: message.into() }
    }

    fn delegate_class() -> &'static AnyClass {
        static CLASS: OnceLock<&'static AnyClass> = OnceLock::new();
        CLASS.get_or_init(|| {
            let superclass = AnyClass::get(c"NSObject").expect("NSObject exists");
            let mut builder = ClassBuilder::new(c"CBBLocationDelegate", superclass)
                .expect("delegate class name is free");
            // SAFETY: each signature matches CLLocationManagerDelegate's.
            unsafe {
                builder.add_method(
                    sel!(locationManager:didUpdateLocations:),
                    did_update as unsafe extern "C-unwind" fn(_, _, _, _),
                );
                builder.add_method(
                    sel!(locationManager:didFailWithError:),
                    did_fail as unsafe extern "C-unwind" fn(_, _, _, _),
                );
                builder.add_method(
                    sel!(locationManagerDidChangeAuthorization:),
                    did_change_authorization as unsafe extern "C-unwind" fn(_, _, _),
                );
            }
            builder.register()
        })
    }

    unsafe extern "C-unwind" fn did_update(
        _this: &AnyObject,
        _sel: Sel,
        _manager: *mut AnyObject,
        locations: *mut AnyObject,
    ) {
        if locations.is_null() {
            return;
        }
        let location: *mut AnyObject = msg_send![locations, lastObject];
        if location.is_null() {
            return;
        }
        let coordinate: Coordinate = msg_send![location, coordinate];
        let accuracy: f64 = msg_send![location, horizontalAccuracy];
        let date: *mut AnyObject = msg_send![location, timestamp];
        let seconds: f64 = if date.is_null() { 0.0 } else { msg_send![date, timeIntervalSince1970] };
        answer_all(Answer::Position {
            latitude: coordinate.latitude,
            longitude: coordinate.longitude,
            accuracy,
            timestamp_ms: seconds * 1000.0,
        });
    }

    unsafe extern "C-unwind" fn did_fail(
        _this: &AnyObject,
        _sel: Sel,
        _manager: *mut AnyObject,
        error: *mut AnyObject,
    ) {
        let code: isize = if error.is_null() { 0 } else { msg_send![error, code] };
        // kCLErrorDenied = 1; anything else: no fix right now.
        answer_all(if code == 1 {
            denied()
        } else {
            unavailable("This Mac couldn't work out where it is. Try again in a moment.")
        });
    }

    unsafe extern "C-unwind" fn did_change_authorization(
        _this: &AnyObject,
        _sel: Sel,
        manager: *mut AnyObject,
    ) {
        proceed(manager);
    }

    /// Move the waiting requests on: ask permission, refuse, or locate.
    unsafe fn proceed(manager: *mut AnyObject) {
        if manager.is_null() || PENDING.lock().map(|ids| ids.is_empty()).unwrap_or(true) {
            return;
        }
        let Some(class) = AnyClass::get(c"CLLocationManager") else {
            answer_all(unavailable("Location Services isn't available on this Mac."));
            return;
        };
        let enabled: bool = msg_send![class, locationServicesEnabled];
        if !enabled {
            answer_all(unavailable(
                "Location Services is turned off on this Mac (System Settings → Privacy & Security).",
            ));
            return;
        }
        // CLAuthorizationStatus: 0 not determined, 1 restricted, 2 denied,
        // 3 authorized (always), 4 when in use.
        let status: i32 = msg_send![manager, authorizationStatus];
        match status {
            0 => {
                let _: () = msg_send![manager, requestWhenInUseAuthorization];
            }
            1 | 2 => answer_all(denied()),
            _ => {
                let _: () = msg_send![manager, requestLocation];
            }
        }
    }

    pub(super) unsafe fn start(label: String, id: u64) {
        if let Ok(mut ids) = PENDING.lock() {
            ids.push((label, id));
        }
        let mut manager = MANAGER.with(Cell::get);
        if manager.is_null() {
            let Some(class) = AnyClass::get(c"CLLocationManager") else {
                answer_all(unavailable("Location Services isn't available on this Mac."));
                return;
            };
            manager = msg_send![class, new];
            let delegate: *mut AnyObject = msg_send![delegate_class(), new];
            let _: () = msg_send![manager, setDesiredAccuracy: ACCURACY_BEST];
            let _: () = msg_send![manager, setDelegate: delegate];
            MANAGER.with(|slot| slot.set(manager));
        }
        proceed(manager);
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
        assert!(page_may_locate(&page));
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

    #[test]
    fn hand_over_carries_a_numeric_id() {
        assert_eq!(request_id(&url("cbb-geo://get?id=7")), Some(7));
        assert_eq!(request_id(&url("cbb-geo://get?id=x")), None);
        assert_eq!(request_id(&url("cbb-download://get?id=7")), None);
    }

    #[test]
    fn answers_reach_the_page_as_json() {
        let ok = answer_script(
            3,
            &Answer::Position { latitude: -33.9, longitude: 18.4, accuracy: 11.0, timestamp_ms: 1.0 },
        );
        assert!(ok.starts_with("window.__cbbGeoResult && window.__cbbGeoResult(3, {"));
        assert!(ok.contains(r#""lat":-33.9"#) && ok.contains(r#""ok":true"#));
        let failed = answer_script(4, &Answer::Failed { code: 1, message: "it's \"off\"".into() });
        assert!(failed.contains(r#""code":1"#) && failed.contains(r#"it's \"off\""#));
    }
}
