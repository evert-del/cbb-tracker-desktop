//! "Continue with Google" finishes in the system browser.
//!
//! Google refuses sign-in inside embedded WebViews ("This browser or app may
//! not be secure"), so the app hands that one step to the user's own
//! browser and takes the result back through a `tracker://` link.
//!
//! The sign-in still *starts* in the app: the tracker's own "Continue with
//! Google" sends the window to Supabase's `/auth/v1/authorize`, having left
//! its PKCE verifier in the app's cookie jar. The shell stops that one
//! navigation, swaps its `redirect_to` for `tracker://signed-in` and opens it
//! in the browser. Google → Supabase → `tracker://signed-in?code=…` brings it
//! back, and the main window loads the tracker's original callback with that
//! code, where the verifier is, as if it had never left. The shell holds no
//! tokens: the code is useless without the verifier in the app's jar.
//!
//! Only a return the shell is waiting for is followed (one, for a short
//! while), and only to the callback address the tracker itself asked for.
//! Supabase must list `tracker://signed-in` under Auth → URL Configuration →
//! Redirect URLs, or it sends the browser to the Site URL instead.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Runtime};

use crate::{APP_HOST, SUPABASE_HOST};

/// Where Supabase sends the browser back to. Must match the Supabase
/// redirect allow-list exactly.
const RETURN_TO: &str = "tracker://signed-in";

/// How long a sign-in started in the browser may take to come back.
const WAIT_FOR: Duration = Duration::from_secs(15 * 60);

/// The tracker callback the pending sign-in returns to, and when it began.
static PENDING: Mutex<Option<(url::Url, Instant)>> = Mutex::new(None);

/// When `url` is Supabase's authorize step for Google on its way back to
/// the tracker, the same URL returning to `RETURN_TO` instead, plus the
/// tracker callback it was going to.
pub(crate) fn hand_off(url: &url::Url) -> Option<(url::Url, url::Url)> {
    if url.scheme() != "https"
        || url.host_str() != Some(SUPABASE_HOST)
        || url.path() != "/auth/v1/authorize"
    {
        return None;
    }
    let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    if !pairs.iter().any(|(key, value)| key == "provider" && value == "google") {
        return None;
    }
    let callback = pairs
        .iter()
        .find(|(key, _)| key == "redirect_to")
        .and_then(|(_, value)| value.parse::<url::Url>().ok())
        .filter(|to| to.scheme() == "https" && to.host_str() == Some(APP_HOST))?;
    let mut browser = url.clone();
    browser.query_pairs_mut().clear().extend_pairs(pairs.iter().map(|(key, value)| {
        if key == "redirect_to" {
            (key.as_str(), RETURN_TO)
        } else {
            (key.as_str(), value.as_str())
        }
    }));
    Some((browser, callback))
}

/// The tracker callback to load for a `tracker://signed-in` return: the one
/// the tracker asked for, with the code (or error) Supabase sent back added.
pub(crate) fn callback_url(callback: &url::Url, returned: &url::Url) -> url::Url {
    let mut url = callback.clone();
    let sent: Vec<(String, String)> = returned.query_pairs().into_owned().collect();
    if !sent.is_empty() {
        url.query_pairs_mut().extend_pairs(sent);
    }
    if let Some(fragment) = returned.fragment() {
        url.set_fragment(Some(fragment));
    }
    url
}

/// Called for every main-window navigation: true when it was Google's
/// sign-in and has gone to the browser (the window stays where it is).
pub(crate) fn start<R: Runtime>(app: &AppHandle<R>, url: &url::Url) -> bool {
    let Some((browser, callback)) = hand_off(url) else {
        return false;
    };
    if let Ok(mut pending) = PENDING.lock() {
        *pending = Some((callback, Instant::now()));
    }
    crate::system_open::open_url(app, browser.as_str());
    crate::download::toast(
        app,
        serde_json::json!({
            "kind": "ok",
            "title": "Continue in your browser",
            "detail": "Sign in with Google there. The tracker comes back here when you're done.",
        }),
    );
    true
}

/// A `tracker://signed-in` link: load the pending callback in the main
/// window. Ignored when no sign-in is waiting (or it waited too long).
pub(crate) fn finish<R: Runtime>(app: &AppHandle<R>, returned: &url::Url) {
    let pending = PENDING.lock().ok().and_then(|mut pending| pending.take());
    let Some((callback, began)) = pending else { return };
    if began.elapsed() > WAIT_FOR {
        return;
    }
    if let Some(window) = tauri::Manager::get_webview_window(app, "main") {
        let _ = window.navigate(callback_url(&callback, returned));
    }
    crate::show_main(app);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(raw: &str) -> url::Url {
        url::Url::parse(raw).expect("test URL parses")
    }

    const AUTHORIZE: &str = "https://wlwdhorybvelwbmhtftw.supabase.co/auth/v1/authorize?provider=google&redirect_to=https%3A%2F%2Ftracker.coolerboxbrothers.com%2Fauth%2Fcallback%3Fnext%3D%252Fdashboard&code_challenge=abc&code_challenge_method=s256";

    #[test]
    fn google_authorize_goes_to_browser_returning_to_app() {
        let (browser, callback) = hand_off(&parsed(AUTHORIZE)).expect("handed off");
        let pairs: Vec<(String, String)> = browser.query_pairs().into_owned().collect();
        assert_eq!(browser.host_str(), Some(SUPABASE_HOST));
        assert!(pairs.contains(&("redirect_to".into(), "tracker://signed-in".into())));
        assert!(pairs.contains(&("code_challenge".into(), "abc".into())));
        assert!(pairs.contains(&("provider".into(), "google".into())));
        assert_eq!(
            callback.as_str(),
            "https://tracker.coolerboxbrothers.com/auth/callback?next=%2Fdashboard"
        );
    }

    #[test]
    fn other_providers_and_pages_stay_in_app() {
        for raw in [
            "https://wlwdhorybvelwbmhtftw.supabase.co/auth/v1/authorize?provider=github&redirect_to=https%3A%2F%2Ftracker.coolerboxbrothers.com%2Fauth%2Fcallback",
            "https://wlwdhorybvelwbmhtftw.supabase.co/auth/v1/token?provider=google",
            "https://accounts.google.com/o/oauth2/v2/auth",
            "https://tracker.coolerboxbrothers.com/sign-in",
        ] {
            assert!(hand_off(&parsed(raw)).is_none(), "{raw}");
        }
    }

    #[test]
    fn foreign_or_missing_callback_is_not_handed_off() {
        for raw in [
            "https://wlwdhorybvelwbmhtftw.supabase.co/auth/v1/authorize?provider=google&redirect_to=https%3A%2F%2Fevil.example%2Fcb",
            "https://wlwdhorybvelwbmhtftw.supabase.co/auth/v1/authorize?provider=google",
            "https://evil.example/auth/v1/authorize?provider=google&redirect_to=https%3A%2F%2Ftracker.coolerboxbrothers.com%2Fauth%2Fcallback",
        ] {
            assert!(hand_off(&parsed(raw)).is_none(), "{raw}");
        }
    }

    #[test]
    fn return_carries_code_to_the_tracker_callback() {
        let callback = parsed("https://tracker.coolerboxbrothers.com/auth/callback?next=%2Fdashboard");
        let url = callback_url(&callback, &parsed("tracker://signed-in?code=xyz"));
        assert_eq!(
            url.as_str(),
            "https://tracker.coolerboxbrothers.com/auth/callback?next=%2Fdashboard&code=xyz"
        );
    }

    #[test]
    fn return_carries_errors_and_fragments() {
        let callback = parsed("https://tracker.coolerboxbrothers.com/auth/callback");
        let url = callback_url(
            &callback,
            &parsed("tracker://signed-in?error=access_denied#error_description=no"),
        );
        assert_eq!(
            url.as_str(),
            "https://tracker.coolerboxbrothers.com/auth/callback?error=access_denied#error_description=no"
        );
    }
}
