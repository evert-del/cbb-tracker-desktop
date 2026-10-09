// Walkie, at once (walkie.rs). The tracker's walkie dock reloads its list
// (GET /api/walkie/channels) the moment Realtime says something was said; that
// is when its squawk plays. This sees the same answer as it arrives, leaves it
// on window.__cbbWalkie and hands over a cbb-walkie://rail navigation, which
// the shell cancels and answers by taking it. So the banner, the menu-bar
// count and the quick panel change with the squawk, not on the next poll.
// Nothing is fetched or sent here; the answer only waits in memory.
(function () {
  if (window.top !== window || window.__cbbWalkieBridge || typeof window.fetch !== 'function') return;
  window.__cbbWalkieBridge = true;

  var pageFetch = window.fetch;
  window.fetch = function (input, init) {
    var answer = pageFetch.apply(this, arguments);
    try {
      var method = (init && init.method) || (input && input.method) || 'GET';
      var url = new URL(typeof input === 'string' ? input : (input && input.url) || String(input), location.href);
      if (url.origin === location.origin && url.pathname === '/api/walkie/channels' && method.toUpperCase() === 'GET') {
        answer.then(function (r) {
          if (!r.ok) return;
          r.clone().text().then(function (text) {
            window.__cbbWalkie = text;
            location.href = 'cbb-walkie://rail';
          }, function () {});
        }, function () {});
      }
    } catch (e) {}
    return answer;
  };
})();
