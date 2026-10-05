// Back / Download buttons for the desktop shell. Injected into the main
// window and the viewer windows files open in (plain DOM, no IPC). Back appears whenever there is history to
// go back to; Download appears on a file page (an attachment opened in the
// app) and saves it through the same-origin download hook -> Downloads.
(function () {
  if (window.top !== window || window.__cbbNavBar) return;
  window.__cbbNavBar = true;

  var bar;
  var back;
  var save;

  function isFilePage() {
    var p = location.pathname;
    return p.indexOf('/api/media/') === 0 || p.indexOf('/api/coolerbox/') === 0;
  }

  function button(label, title) {
    var b = document.createElement('button');
    b.type = 'button';
    b.textContent = label;
    b.title = title;
    b.setAttribute('aria-label', title);
    b.style.cssText =
      'font:600 13px/1 -apple-system,Segoe UI,Helvetica,Arial,sans-serif;color:#1f2330;' +
      'background:#fff;border:1px solid #d6d9e2;border-radius:999px;padding:8px 14px;' +
      'box-shadow:0 1px 4px rgba(0,0,0,.18);cursor:pointer;';
    return b;
  }

  function mount() {
    if (bar || !document.body) return;
    bar = document.createElement('div');
    bar.id = 'cbb-nav-bar';
    bar.style.cssText =
      'position:fixed;z-index:2147483000;display:none;gap:8px;top:14px;left:12px;';

    back = button('‹  Back', 'Back (Cmd/Ctrl + [)');
    back.addEventListener('click', function () { history.back(); });

    save = button('↓  Download', 'Download this file');
    save.addEventListener('click', function () {
      var a = document.createElement('a');
      a.href = location.href;
      a.download = '';
      a.style.display = 'none';
      document.body.appendChild(a);
      a.click();
      a.remove();
    });

    bar.appendChild(back);
    bar.appendChild(save);
    document.body.appendChild(bar);
  }

  function place() {
    // On a file page sit bottom-right: the PDF viewer's own toolbar runs
    // along the top. On the tracker's own pages keep clear of the sidebar.
    if (isFilePage()) {
      bar.style.top = 'auto';
      bar.style.left = 'auto';
      bar.style.bottom = '16px';
      bar.style.right = '16px';
      return;
    }
    bar.style.top = '14px';
    bar.style.bottom = 'auto';
    bar.style.right = 'auto';
    var sidebar = document.querySelector('.gd-sidebar');
    var left = 12;
    if (sidebar) {
      var r = sidebar.getBoundingClientRect();
      if (r.width > 0) left = Math.round(r.right) + 14;
    }
    bar.style.left = left + 'px';
  }

  function update() {
    mount();
    if (!bar) return;
    var file = isFilePage();
    back.style.display = history.length > 1 ? '' : 'none';
    save.style.display = file ? '' : 'none';
    bar.style.display = history.length > 1 || file ? 'flex' : 'none';
    place();
  }

  document.addEventListener('keydown', function (e) {
    var mod = e.metaKey || e.ctrlKey;
    if ((mod && e.key === '[') || (e.altKey && e.key === 'ArrowLeft')) {
      e.preventDefault();
      history.back();
    }
  });

  // The tracker is a single-page app, so poll instead of hooking every
  // navigation; it is a handful of cheap checks twice a second.
  setInterval(update, 500);
  document.addEventListener('DOMContentLoaded', update);
})();
