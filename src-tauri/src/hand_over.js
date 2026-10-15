// Hand-overs to the shell (lib.rs on_navigation). The main window has no IPC,
// so the page tells the app something by navigating to one of its own
// addresses (cbb-walkie://, cbb-download://, cbb-window:// ...), which the
// shell takes and cancels. The browser still treats that as leaving the page
// and fires beforeunload first, so a tracker page that warns before leaving
// (a cut uploading, a transfer, unsaved call sheet) asked "Leave site?" on
// every hand-over: found live 2026-10-15, once per Walkie line during an
// upload. Loaded before every other script, this listener is the window's
// first, and it stops the page's own warnings for a hand-over only. A real
// reload, link or close still warns.
(function () {
  if (window.top !== window || window.__cbbHandOver) return;

  var pending = 0;
  var until = 0;

  window.addEventListener(
    'beforeunload',
    function (e) {
      if (pending > 0 && Date.now() < until) {
        pending -= 1;
        e.stopImmediatePropagation();
      } else {
        pending = 0;
      }
    },
    true
  );

  Object.defineProperty(window, '__cbbHandOver', {
    value: function (url) {
      pending += 1;
      until = Date.now() + 1500;
      location.href = url;
    }
  });
})();
