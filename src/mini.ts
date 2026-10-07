import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";

// Quick panel: all state is pushed from Rust via eval (window.__cbbMiniShow).
// Clicks go back through the narrow mini commands (see mini.json capability).
// The timer and today's total tick locally every second between pushes.

type Need = { label: string; body: string; href: string };
type ClockAction = { id: string; label: string };
type View = {
  state: string;
  status: string;
  actions: ClockAction[];
  since_iso: string;
  worked_min: number;
  break_min: number;
  unread: number;
  needs: Need[];
  version: string;
  autostart: boolean;
  pinned: boolean;
};

declare global {
  interface Window {
    __cbbMiniShow?: (view: View) => void;
  }
}

let current: View | null = null;
let pushedAt = 0;

function el<T extends HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (!node) throw new Error(`missing #${id}`);
  return node as T;
}

function svg(path: string): SVGSVGElement {
  const ns = "http://www.w3.org/2000/svg";
  const icon = document.createElementNS(ns, "svg");
  icon.setAttribute("viewBox", "0 0 24 24");
  const p = document.createElementNS(ns, "path");
  p.setAttribute("d", path);
  icon.appendChild(p);
  return icon;
}

const ICONS: Record<string, string> = {
  in: "M8 5v14l11-7z",
  back: "M8 5v14l11-7z",
  break: "M5 8h12v5a5 5 0 0 1-5 5h-2a5 5 0 0 1-5-5zM17 9h1.5a2.5 2.5 0 0 1 0 5H17M8 2v3M12 2v3",
  wrap: "M14 4h4a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2h-4M9 16l-4-4 4-4M5 12h11",
  chevron: "M9 6l6 6-6 6",
};

/** `1:05:09`, or `5:09` under the hour. */
function fmtTimer(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = String(s % 60).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${sec}` : `${m}:${sec}`;
}

/** `8h 12m`, or `45m` under the hour (the tracker's own minute language). */
function fmtSpan(mins: number): string {
  const m = Math.max(0, Math.floor(mins));
  const h = Math.floor(m / 60);
  return h > 0 ? `${h}h ${String(m % 60).padStart(2, "0")}m` : `${m}m`;
}

/** Badge colours for a notification label, the same for every row of it. */
const BADGES: Array<[string, string]> = [
  ["#fbe4ee", "#8f1d55"],
  ["#e9e7fd", "#3c3489"],
  ["#dff4ec", "#0f5c47"],
  ["#fdf0d9", "#7a4a06"],
];
function badgeFor(label: string): [string, string] {
  let h = 0;
  for (const ch of label) h = (h * 31 + ch.charCodeAt(0)) >>> 0;
  return BADGES[h % BADGES.length];
}

function tick() {
  const view = current;
  if (!view) return;
  const onClock = view.state === "in" || view.state === "break";
  const since = Date.parse(view.since_iso);
  el("timer").textContent = !onClock ? "0:00" : Number.isNaN(since) ? "—" : fmtTimer(Date.now() - since);
  // Today's totals were worked out at the last push; the open stretch keeps
  // growing until the next one.
  const grown = (Date.now() - pushedAt) / 60000;
  const worked = view.worked_min + (view.state === "in" ? grown : 0);
  const rest = view.break_min + (view.state === "break" ? grown : 0);
  el("today").textContent =
    view.state === "none"
      ? ""
      : worked < 1 && rest < 1
        ? "Nothing on the time sheet yet today"
        : `Today ${fmtSpan(worked)} worked` + (rest >= 1 ? ` · ${fmtSpan(rest)} break` : "");
}

function renderActions(view: View) {
  const box = el("actions");
  box.replaceChildren();
  el("no-sheet").hidden = view.state !== "none";
  box.hidden = view.actions.length === 0;
  for (const action of view.actions) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "act";
    if (action.id === "in" || action.id === "back") btn.classList.add("primary");
    if (action.id === "wrap") btn.classList.add("wrap");
    btn.append(svg(ICONS[action.id] ?? ICONS.in), document.createTextNode(action.label));
    btn.addEventListener("click", () => {
      void invoke("mini_action", { action: action.id });
    });
    box.appendChild(btn);
  }
}

function renderNeeds(view: View) {
  const list = el("needs");
  list.replaceChildren();
  const shown = view.needs.slice(0, 3);
  for (const need of shown) {
    const row = document.createElement("button");
    row.type = "button";
    row.className = "need";
    row.title = need.body;
    const [bg, fg] = badgeFor(need.label);
    const badge = document.createElement("span");
    badge.className = "badge";
    badge.style.background = bg;
    badge.style.color = fg;
    badge.textContent = (need.label.trim()[0] ?? "•").toUpperCase();
    const txt = document.createElement("span");
    txt.className = "txt";
    const body = document.createElement("div");
    body.className = "body";
    body.textContent = need.body;
    const label = document.createElement("div");
    label.className = "label";
    label.textContent = need.label;
    txt.append(body, label);
    row.append(badge, txt, svg(ICONS.chevron));
    row.addEventListener("click", () => {
      void invoke("mini_open", { href: need.href });
    });
    list.appendChild(row);
  }
  el("empty").hidden = view.unread > 0 || shown.length > 0;
  el("needs-label").textContent = view.unread > 0 ? `Needs you · ${view.unread}` : "Needs you";
  el("see-all-text").textContent =
    view.unread > shown.length && shown.length > 0
      ? `See all ${view.unread} notifications`
      : "See all notifications";
}

window.__cbbMiniShow = (view) => {
  current = view;
  pushedAt = Date.now();
  el("head").dataset.state = view.state;
  el("status").textContent = view.status;
  el("version").textContent = `CoolerBox Tracker ${view.version}`;
  el("version-hint").textContent = `You have ${view.version}.`;
  const on = String(view.autostart);
  el("autostart-row").setAttribute("aria-checked", on);
  el("autostart-switch").setAttribute("aria-checked", on);
  renderPin(view.pinned);
  renderActions(view);
  renderNeeds(view);
  tick();
};

function renderPin(pinned: boolean) {
  const pin = el("pin");
  pin.setAttribute("aria-pressed", String(pinned));
  const label = pinned ? "Unpin" : "Pin as a floating timer";
  pin.setAttribute("aria-label", label);
  pin.title = label;
  el("head").classList.toggle("pinned", pinned);
  el("pin-hint").hidden = !pinned;
}

function showSettings(open: boolean) {
  el("main-view").hidden = open;
  el("settings").hidden = !open;
}

function menu(item: string) {
  void invoke("mini_menu", { item });
}

window.addEventListener("DOMContentLoaded", () => {
  window.setInterval(tick, 1000);
  const click = (id: string, run: () => void) => el(id).addEventListener("click", run);
  click("open-top", () => void invoke("mini_expand"));
  click("open-tracker", () => void invoke("mini_expand"));
  click("see-all", () => void invoke("mini_expand_notifications"));
  click("open-offline", () => menu("offline"));
  click("update", () => menu("update"));
  click("update-row", () => menu("update"));
  click("diagnostics-row", () => menu("diagnostics"));
  click("autostart-row", () => menu("autostart"));
  click("quit", () => menu("quit"));
  click("quit-row", () => menu("quit"));
  click("open-settings", () => showSettings(true));
  click("close-settings", () => showSettings(false));
  click("pin", () => {
    const pinned = el("pin").getAttribute("aria-pressed") !== "true";
    renderPin(pinned);
    void invoke("mini_pin", { pinned });
  });
  // Pinned, the header drags the window (not from its buttons).
  el("head").addEventListener("mousedown", (e) => {
    if (e.button !== 0 || !el("head").classList.contains("pinned")) return;
    if ((e.target as HTMLElement).closest("button")) return;
    void getCurrentWindow().startDragging();
  });
  window.addEventListener("keydown", (e) => {
    if (e.key !== "Escape") return;
    if (!el("settings").hidden) showSettings(false);
    else void invoke("mini_hide");
  });
  // Each opening starts on the main view.
  window.addEventListener("blur", () => showSettings(false));
});
