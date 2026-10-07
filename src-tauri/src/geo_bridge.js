// macOS only (location.rs): an app's WKWebView has no location source, so
// on the tracker's own pages navigator.geolocation is replaced by one that
// asks the shell. Each request goes over as a cbb-geo://get?id=N navigation
// (the shell cancels it, so the page stays put); the answer comes back
// through window.__cbbGeoResult(id, {...}). No IPC; other sites and frames
// keep the webview's own (empty) geolocation.
(function () {
  if (window.top !== window || window.__cbbGeo) return;
  if (location.protocol !== 'https:' || location.hostname !== 'tracker.coolerboxbrothers.com') return;
  window.__cbbGeo = true;

  var nextId = 1;
  var waiting = {};

  function positionError(code, message) {
    return { code: code, message: message || '', PERMISSION_DENIED: 1, POSITION_UNAVAILABLE: 2, TIMEOUT: 3 };
  }

  window.__cbbGeoResult = function (id, r) {
    var w = waiting[id];
    if (!w) return;
    delete waiting[id];
    clearTimeout(w.timer);
    if (r && r.ok) {
      w.ok({
        coords: {
          latitude: r.lat, longitude: r.lon, accuracy: r.acc,
          altitude: null, altitudeAccuracy: null, heading: null, speed: null,
        },
        timestamp: r.ts || Date.now(),
      });
    } else if (typeof w.fail === 'function') {
      w.fail(positionError((r && r.code) || 2, r && r.message));
    }
  };

  function getCurrentPosition(ok, fail, options) {
    if (typeof ok !== 'function') return;
    var id = nextId++;
    var ms = options && typeof options.timeout === 'number' && isFinite(options.timeout) && options.timeout > 0
      ? options.timeout : 60000;
    waiting[id] = {
      ok: ok,
      fail: fail,
      // The first time, macOS asks the person first: give them the time.
      timer: setTimeout(function () {
        window.__cbbGeoResult(id, { ok: false, code: 3, message: 'Timed out' });
      }, Math.max(ms, 60000)),
    };
    location.href = 'cbb-geo://get?id=' + id;
  }

  var geolocation = {
    getCurrentPosition: getCurrentPosition,
    // One answer per watch: enough for a page that only needs where it is now.
    watchPosition: function (ok, fail, options) { getCurrentPosition(ok, fail, options); return 0; },
    clearWatch: function () {},
  };
  try {
    Object.defineProperty(navigator, 'geolocation', { value: geolocation, configurable: true });
  } catch (err) {}
})();
