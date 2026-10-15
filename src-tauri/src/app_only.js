// The app is the tracker, not the website (lib.rs WEBSITE_PATHS). Plain DOM,
// no IPC.
//  - Links to the website's own pages (pricing, privacy, terms, blog, …) open
//    in the person's browser, handed over as cbb-browser://open?u=<page>
//    (cancelled by the shell, so the page stays put). The tracker changes
//    pages without loading them, so a click is caught here, before its router.
//  - Signed out, the app shows only the sign-in screens: "/" is the website's
//    homepage for a visitor (the dashboard once signed in), so a signed-out
//    "/" goes to /sign-in, also after the router changes page.
(function () {
  if (window.top !== window || window.__cbbAppOnly) return;
  if (location.hostname !== 'tracker.coolerboxbrothers.com') return;
  window.__cbbAppOnly = true;

  // A hand-over to the shell that never asks "Leave site?" (hand_over.js).
  function handOver(url) {
    if (typeof window.__cbbHandOver === 'function') window.__cbbHandOver(url);
    else location.href = url;
  }

  // Must mirror WEBSITE_PATHS in lib.rs.
  var WEBSITE = ['pricing', 'watch', 'privacy', 'terms', 'blog', 'compare', 'templates',
    'affiliates', 'questions', 'landing', 'download', 'cbb-casting', 'dev-ui-kit', 'unsubscribe'];

  function isWebsite(path) {
    var first = (path || '/').split('/')[1] || '';
    return WEBSITE.indexOf(first) !== -1;
  }

  // Signed in: the signed-in sidebar, or a sign-in cookie the page can see.
  // Since 2026-10-15 the cookie is HttpOnly, so the sidebar is what counts
  // (session.rs SIGNED_IN_JS says the same).
  function signedIn() {
    if (document.querySelector('.gd-sidebar')) return true;
    return document.cookie.split('; ').some(function (pair) {
      var name = pair.split('=')[0];
      return name.indexOf('sb-') === 0 && name.indexOf('-auth-token') !== -1;
    });
  }

  function openInBrowser(href) {
    handOver('cbb-browser://open?u=' + encodeURIComponent(href));
  }

  document.addEventListener('click', function (e) {
    if (e.defaultPrevented || e.button !== 0) return;
    var a = e.target && e.target.closest ? e.target.closest('a[href]') : null;
    if (!a) return;
    var u;
    try { u = new URL(a.href, location.href); } catch (err) { return; }
    if (u.host !== location.host) return;
    if (isWebsite(u.pathname)) {
      e.preventDefault();
      e.stopPropagation();
      openInBrowser(u.href);
    } else if (u.pathname === '/' && !signedIn()) {
      // The logo on the sign-in screen: stay on sign in.
      e.preventDefault();
      e.stopPropagation();
      if (location.pathname !== '/sign-in') location.assign('/sign-in');
    }
  }, true);

  // Pages the router reaches without a click (a redirect after signing out,
  // a script): check where we are.
  var last = '';
  setInterval(function () {
    var here = location.pathname;
    if (here === last) return;
    // "/" is only judged once the page is there: the sidebar comes with it.
    if (here === '/' && document.readyState !== 'complete') return;
    last = here;
    if (here === '/' && !signedIn()) {
      location.replace('/sign-in');
    } else if (isWebsite(here)) {
      openInBrowser(location.href);
      // After the hand-over has reached the shell.
      setTimeout(function () { location.replace(signedIn() ? '/' : '/sign-in'); }, 300);
    }
  }, 400);
})();
