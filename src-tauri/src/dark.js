// macOS: the tracker in dark mode when the Mac is (theme.rs). The tracker
// site has only a light design, and WebKit has no automatic dark mode like
// Edge's on Windows, so the page's colours are flipped here instead.
// Photos, video, the black header and the app's own messages are flipped
// back so they look as designed. Rust turns it on and off live through
// window.__cbbSetDark; the choice is kept for this tab's later page loads.
(function () {
  if (window.top !== window) return;
  var media = "img,video,picture,canvas,iframe,[style*='background-image']";
  var back = 'filter:invert(1) hue-rotate(180deg)';
  var css =
    'html.cbb-dark{filter:invert(.92) hue-rotate(180deg);background:#fff}' +
    // Media and the header go back to their own colours; anything inside
    // one of those (a logo in the header) is left alone.
    'html.cbb-dark :is(' + media + '),' +
    'html.cbb-dark .gd-header,' +
    "html.cbb-dark body>div[role=status][style*='2147483001']{" + back + '}' +
    'html.cbb-dark :is(' + media + ',.gd-header) :is(' + media + '){filter:none}';

  var style = document.createElement('style');
  style.id = 'cbb-dark';
  style.textContent = css;
  var wanted = false;
  function apply() {
    var root = document.documentElement;
    if (!root) return;
    if (!style.isConnected) (document.head || root).appendChild(style);
    root.classList.toggle('cbb-dark', wanted);
  }
  // At document start there may be no <html> yet: apply as soon as there is.
  if (!document.documentElement) {
    var watch = new MutationObserver(function () {
      if (document.documentElement) { watch.disconnect(); apply(); }
    });
    watch.observe(document, { childList: true });
  }
  document.addEventListener('DOMContentLoaded', apply);

  function set(on) {
    wanted = !!on;
    apply();
    try { sessionStorage.setItem('cbbDark', wanted ? '1' : '0'); } catch (e) {}
  }
  var saved = null;
  try { saved = sessionStorage.getItem('cbbDark'); } catch (e) {}
  set(saved === null ? !!window.__cbbDarkAtLaunch : saved === '1');
  window.__cbbSetDark = set;
})();
