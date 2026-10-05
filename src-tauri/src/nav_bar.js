// Desktop-shell page helpers, injected into the main window (plain DOM, no
// IPC):
//  1. File links (chat attachments, Cooler Box items: same-origin
//     /api/media/ and /api/coolerbox/ anchors) are saved to Downloads instead
//     of opening as a bare page. The click is handed to the shell through a
//     cbb-download:// navigation, which the shell intercepts (download.rs).
//     No on-page buttons: the tracker page is left exactly as the website
//     made it.
//  2. window.__cbbToast(): the in-app message the shell uses to show the
//     download's progress ("Preparing…", "Downloading…" with a bar, "Download
//     complete" with a Show-in-folder button, or "Download failed").
(function () {
  if (window.top !== window || window.__cbbNavBar) return;
  window.__cbbNavBar = true;


  // ---- download message -------------------------------------------------
  var toastEl;
  var toastTimer;

  function el(tag, css, text) {
    var n = document.createElement(tag);
    if (css) n.style.cssText = css;
    if (text != null) n.textContent = text;
    return n;
  }

  // o: {kind: 'busy'|'ok'|'error'|'hide', title, detail, progress (0-100 or
  // null = unknown), action: {label, path}}. All text goes in via textContent.
  window.__cbbToast = function (o) {
    if (!document.body) return;
    clearTimeout(toastTimer);
    if (toastEl) toastEl.remove();
    toastEl = null;
    if (!o || o.kind === 'hide') return;

    var accent = o.kind === 'ok' ? '#2bb673' : o.kind === 'error' ? '#e5484d' : '#DD347C';
    var box = el(
      'div',
      'position:fixed;z-index:2147483001;left:50%;bottom:28px;transform:translateX(-50%);' +
        'min-width:300px;max-width:min(460px,calc(100vw - 32px));box-sizing:border-box;' +
        'background:#1f2330;color:#fff;border-radius:14px;padding:14px 18px;' +
        'box-shadow:0 8px 30px rgba(0,0,0,.35);border-left:5px solid ' + accent + ';' +
        'font:500 14px/1.4 -apple-system,Segoe UI,Helvetica,Arial,sans-serif;'
    );
    box.setAttribute('role', 'status');
    box.appendChild(el('div', 'font-weight:700;font-size:15px;', o.title || ''));
    if (o.detail) {
      box.appendChild(el('div', 'margin-top:4px;color:#c9ccd6;font-size:13px;white-space:pre-line;word-break:break-all;', o.detail));
    }
    if (o.kind === 'busy') {
      var track = el('div', 'margin-top:10px;height:5px;border-radius:99px;background:#3a3f52;overflow:hidden;');
      var bar = el('div', 'height:100%;border-radius:99px;background:#DD347C;');
      if (typeof o.progress === 'number') {
        bar.style.width = Math.max(2, Math.min(100, o.progress)) + '%';
        bar.style.transition = 'width .2s';
      } else {
        bar.style.width = '35%';
        bar.animate(
          [{ marginLeft: '-35%' }, { marginLeft: '100%' }],
          { duration: 1100, iterations: Infinity }
        );
      }
      track.appendChild(bar);
      box.appendChild(track);
      if (typeof o.progress === 'number') {
        box.appendChild(el('div', 'margin-top:6px;color:#c9ccd6;font-size:12px;', o.progress + '%'));
      }
    }
    if (o.action) {
      var go = el(
        'button',
        'margin-top:10px;margin-right:8px;font:600 13px/1 inherit;color:#fff;background:#DD347C;' +
          'border:0;border-radius:99px;padding:8px 14px;cursor:pointer;',
        o.action.label
      );
      go.type = 'button';
      go.addEventListener('click', function () {
        location.href = 'cbb-reveal://go?p=' + encodeURIComponent(o.action.path);
      });
      box.appendChild(go);
    }
    if (o.kind !== 'busy') {
      var close = el(
        'button',
        'margin-top:10px;font:600 13px/1 inherit;color:#c9ccd6;background:transparent;' +
          'border:1px solid #4a5068;border-radius:99px;padding:7px 12px;cursor:pointer;',
        'Dismiss'
      );
      close.type = 'button';
      close.addEventListener('click', function () { window.__cbbToast({ kind: 'hide' }); });
      box.appendChild(close);
      toastTimer = setTimeout(function () { window.__cbbToast({ kind: 'hide' }); }, o.kind === 'ok' ? 15000 : 10000);
    }
    document.body.appendChild(box);
    toastEl = box;
  };

  var FILE_PREFIXES = ['/api/media/', '/api/coolerbox/'];

  function isFileUrl(u) {
    if (u.origin !== location.origin) return false;
    for (var i = 0; i < FILE_PREFIXES.length; i++) {
      if (u.pathname.indexOf(FILE_PREFIXES[i]) === 0) return true;
    }
    return false;
  }

  document.addEventListener(
    'click',
    function (e) {
      if (e.defaultPrevented || e.button !== 0) return;
      var a = e.target && e.target.closest ? e.target.closest('a[href]') : null;
      if (!a) return;
      var u;
      try { u = new URL(a.href, location.href); } catch (err) { return; }
      if (!isFileUrl(u)) return;
      e.preventDefault();
      e.stopPropagation();
      // Instant feedback; the shell replaces this as soon as it knows more.
      window.__cbbToast({ kind: 'busy', title: 'Preparing your download\u2026' });
      location.href = 'cbb-download://go?u=' + encodeURIComponent(u.href);
    },
    true
  );

  // No on-page buttons: file links are intercepted above and saved through
  // the shell, so the tracker page is left exactly as the website made it.
  // The keyboard shortcut stays as an invisible convenience.
  document.addEventListener('keydown', function (e) {
    var mod = e.metaKey || e.ctrlKey;
    if ((mod && e.key === '[') || (e.altKey && e.key === 'ArrowLeft')) {
      e.preventDefault();
      history.back();
    }
  });
})();
