// Desktop-shell clock pill override, injected into the main window (plain
// DOM, no IPC):
//
// The website's own CSS leaves `summary.gd-clock-trigger` transparent and
// only paints the light pill on `:hover` (globals.css). On the dark header
// that means the elapsed text is invisible until hovered — only the state
// dot shows. Force the hover look permanently, desktop-shell only. The real
// one-line fix belongs on the website; this keeps the app readable meanwhile.
(function () {
  if (window.top !== window || window.__cbbClockPill) return;
  window.__cbbClockPill = true;

  var css =
    ".gd-menu > summary.gd-clock-trigger {" +
    "background: var(--g-panel, #fff) !important;" +
    "color: var(--g-ink, #111) !important;" +
    "}";

  function inject() {
    if (document.getElementById("cbb-clock-pill")) return;
    var style = document.createElement("style");
    style.id = "cbb-clock-pill";
    style.textContent = css;
    (document.head || document.documentElement).appendChild(style);
  }

  if (document.head || document.documentElement) inject();
  document.addEventListener("DOMContentLoaded", inject);
})();
