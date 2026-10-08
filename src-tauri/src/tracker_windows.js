// More tracker windows, for comparing one review page with another (lib.rs
// open_tracker_window). Plain DOM, no IPC: each request is handed to the
// shell as a cbb-window://open?u=<page>[&tab=1] navigation, which the shell
// cancels, so the page stays put.
//  - Cmd+T (Ctrl+T on Windows/Linux): this page again, in a new tab (macOS
//    window tabs; a new window elsewhere).
//  - Cmd+N (Ctrl+N): this page again, in a new window.
//  - Cmd/Ctrl-click or middle-click on a tracker link: that page in a new tab.
// File links (/api/…) are left to nav_bar.js.
(function () {
  if (window.top !== window || window.__cbbWindows) return;
  window.__cbbWindows = true;

  var mac = /Mac/i.test(navigator.platform || navigator.userAgent);

  function open(href, tab) {
    location.href = 'cbb-window://open?u=' + encodeURIComponent(href) + (tab ? '&tab=1' : '');
  }

  function withModifier(e) {
    return mac ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey;
  }

  document.addEventListener('keydown', function (e) {
    if (!withModifier(e) || e.altKey || e.shiftKey || e.repeat) return;
    var key = (e.key || '').toLowerCase();
    if (key !== 't' && key !== 'n') return;
    e.preventDefault();
    e.stopPropagation();
    open(location.href, key === 't');
  }, true);

  function trackerLink(e) {
    var a = e.target && e.target.closest ? e.target.closest('a[href]') : null;
    if (!a) return null;
    var u;
    try { u = new URL(a.href, location.href); } catch (err) { return null; }
    if (u.protocol !== 'https:' || u.host !== location.host) return null;
    if (u.pathname.indexOf('/api/') === 0) return null;
    return u;
  }

  document.addEventListener('click', function (e) {
    if (e.button !== 0 || !withModifier(e)) return;
    var u = trackerLink(e);
    if (!u) return;
    e.preventDefault();
    e.stopPropagation();
    open(u.href, true);
  }, true);

  document.addEventListener('auxclick', function (e) {
    if (e.button !== 1) return;
    var u = trackerLink(e);
    if (!u) return;
    e.preventDefault();
    e.stopPropagation();
    open(u.href, true);
  }, true);
})();
