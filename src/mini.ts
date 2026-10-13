import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";

// Quick panel: all state is pushed from Rust via eval (window.__cbbMiniShow).
// Clicks go back through the narrow mini commands (see mini.json capability).
// The timer and today's total tick locally every second between pushes.

type Need = { label: string; body: string; href: string };
type ClockAction = { id: string; label: string };
type LabelCount = { label: string; count: number };
/** The next booking not yet over (schedule.rs). */
type NextCall = {
  starts_at: string;
  ends_at: string;
  day: string;
  title: string;
  project: string;
  place: string | null;
  map_url: string | null;
  pencilled: boolean;
  sheet_href: string | null;
};

/** Which notifications pop up, and the tracker's do-not-disturb (quiet.rs). */
type Quiet = { tracker: boolean; walkie: boolean; channel1: boolean; dnd_until: string };

/** The project timer, for people who bill by the hour (hourly.rs). */
/** `retainerMinutesLeft`: the client's retainer this month (negative: over), null without one. */
type HourlyProject = {
  id: string;
  name: string;
  projectNumber: string;
  clientName: string | null;
  billable: boolean;
  retainerMinutesLeft?: number | null;
};
/** `projectId` null: started with no project, "Not yet filed" (never billable). */
type HourlyRunning = {
  id: string;
  projectId: string | null;
  projectName: string | null;
  clientName?: string | null;
  billable: boolean;
  /** Finished entries only: the running time is taken off here (liveRetainer). */
  retainerMinutesLeft?: number | null;
  startedAt: string;
  note: string | null;
};
type Hourly = {
  running: HourlyRunning | null;
  quick: HourlyProject[];
  projects: HourlyProject[];
  /** A question about idle time is waiting for an answer. */
  asking: boolean;
  /** An unfiled stretch is waiting for its project. */
  filing: boolean;
  /** Settings: minutes without input before idle time counts. */
  idleMinutes: number;
  /** Pinned project ids (favourites), in order. */
  pinned?: string[];
  /** The global start/stop shortcut ("⌘⇧Y", "Ctrl+Alt+Y"). */
  shortcut: string;
  /** The project they're booked on today (My Schedule), first in `quick`. */
  booked?: string | null;
  /** "Suggest time from my work" (detector.rs). */
  detector?: Detector;
  /** Today's entries and totals; null while the tracker doesn't have the list. */
  today?: Today | null;
  /** The company's clients, for "New project…" (empty: they can't start projects). */
  clients?: { id: string; name: string }[];
};

type TodayEntry = {
  id: string;
  projectId: string | null;
  projectName: string | null;
  clientName: string | null;
  billable: boolean;
  startedAt: string;
  endedAt: string | null;
  note: string | null;
  /** On an invoice: locked. */
  billed: boolean;
  minutes: number;
  /** The note shows on the client's Hours page and invoice. */
  noteForClient?: boolean;
};
type Today = { day: string; entries: TodayEntry[]; totalMinutes: number; billableMinutes: number };

type Detector = {
  on: boolean;
  apps: { id: string; label: string; kind: string; on: boolean }[];
  folders: { key: string; folder: string; project: string }[];
  titlesAllowed: boolean;
  titlesSupported: boolean;
  /** macOS: the permission works once the app restarts. */
  restart?: boolean;
  /** macOS: what the permission line says ("ask", "restart", "stuck"). */
  permission?: "ask" | "restart" | "stuck" | null;
  /** "Right now": what it's seeing (on this computer only). */
  now?: {
    kind: "timer" | "away" | "app";
    app: string;
    title: string;
    picked: boolean;
    project: string | null;
    counting: string | null;
    since: number | null;
    canLog: boolean;
  } | null;
  suggestion: {
    id: number;
    projectId: string;
    label: string;
    unrecognised?: boolean;
    billable: boolean;
    from: number;
    to: number;
  } | null;
  more: number;
};

type ChatRow = {
  key: string;
  kind: string;
  name: string;
  preview: string;
  unread: number;
  last_at: string;
  ringing: string | null;
};
type View = {
  state: string;
  status: string;
  actions: ClockAction[];
  since_iso: string;
  worked_min: number;
  break_min: number;
  unread: number;
  needs: Need[];
  summary: LabelCount[];
  walkie_unread: number;
  chats: ChatRow[];
  quiet: Quiet;
  next_call: NextCall | null;
  auto_offline: boolean;
  hourly: Hourly | null;
  version: string;
  autostart: boolean;
  close_quits: boolean;
  pinned: boolean;
  compact: boolean;
  platform: string;
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

/** Badge colour (a `tone-N` class in mini.html, light and dark) for a
 * notification label, the same for every row of it. */
const TONES = 6;
function toneFor(label: string): string {
  let h = 0;
  for (const ch of label) h = (h * 31 + ch.charCodeAt(0)) >>> 0;
  return `tone-${h % TONES}`;
}

function tick() {
  const view = current;
  if (!view) return;
  const onClock = view.state === "in" || view.state === "break";
  const since = Date.parse(view.since_iso);
  const timer = !onClock ? "0:00" : Number.isNaN(since) ? "—" : fmtTimer(Date.now() - since);
  el("timer").textContent = timer;
  el("c-timer").textContent = timer;
  // Today's totals were worked out at the last push; the open stretch keeps
  // growing until the next one.
  const grown = (Date.now() - pushedAt) / 60000;
  const worked = view.worked_min + (view.state === "in" ? grown : 0);
  const rest = view.break_min + (view.state === "break" ? grown : 0);
  const running = view.hourly?.running;
  const started = running ? Date.parse(running.startedAt) : NaN;
  el("hourly-elapsed").textContent = Number.isNaN(started) ? "" : fmtTimer(Date.now() - started);
  if (!el("hourly-auto").hidden) renderAutoCard();
  if (running) renderRunningRetainer(running);
  el("today").textContent =
    view.state === "none"
      ? ""
      : worked < 1 && rest < 1
        ? "Nothing on the time sheet yet today"
        : `Today ${fmtSpan(worked)} worked` + (rest >= 1 ? ` · ${fmtSpan(rest)} break` : "");
}

/**
 * After a clock tap the clock buttons rest until the time sheet's new state
 * arrives (the next push) or 3 s pass, so a double-click can't send the tap
 * twice. Seen live: two "break" taps a second apart.
 */
const REST_MS = 3000;
let tapped: { state: string; until: number } | null = null;

function clockResting(): boolean {
  if (!tapped) return false;
  if (Date.now() > tapped.until || (current !== null && current.state !== tapped.state)) {
    tapped = null;
    return false;
  }
  return true;
}

function restClockButtons(): void {
  const resting = clockResting();
  document.querySelectorAll<HTMLButtonElement>("#actions .act, #c-action").forEach((btn) => {
    btn.disabled = resting;
  });
}

function tapClock(action: string, from: "panel" | "mini_timer"): void {
  if (clockResting()) return;
  tapped = { state: current?.state ?? "", until: Date.now() + REST_MS };
  restClockButtons();
  void invoke("mini_action", { action, from });
  window.setTimeout(restClockButtons, REST_MS + 50);
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
    btn.addEventListener("click", () => tapClock(action.id, "panel"));
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
    const badge = document.createElement("span");
    badge.className = `badge ${toneFor(need.label)}`;
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

/** "Approval 3 · Phase 2" (plus any unread the poll didn't list). */
function summaryText(view: View): string {
  if (view.unread === 0) return "No notifications waiting";
  const listed = view.summary.reduce((n, c) => n + c.count, 0);
  const parts = view.summary.slice(0, 3).map((c) => `${c.label} ${c.count}`);
  const rest = view.unread - view.summary.slice(0, 3).reduce((n, c) => n + c.count, 0);
  if (parts.length === 0 || listed === 0) return `${view.unread} unread`;
  if (rest > 0) parts.push(`${rest} more`);
  return parts.join(" · ");
}

function renderSummary(view: View) {
  const box = el("summary");
  box.replaceChildren();
  box.hidden = view.unread === 0 || view.summary.length === 0;
  for (const kind of view.summary.slice(0, 4)) {
    const chip = document.createElement("span");
    chip.className = `chip ${toneFor(kind.label)}`;
    chip.textContent = `${kind.label} ${kind.count}`;
    box.appendChild(chip);
  }
  el("c-summary").textContent = summaryText(view);
  const count = el("c-count");
  count.hidden = view.unread === 0;
  count.textContent = view.unread > 99 ? "99+" : String(view.unread);
  el("c-bell").title = view.unread > 0 ? `${view.unread} unread notifications` : "Notifications";
}

/** The mini timer's one clock tap: call in or come back first, else wrap. */
function renderCompactAction(view: View) {
  const btn = el<HTMLButtonElement>("c-action");
  const action =
    view.actions.find((a) => a.id === "in" || a.id === "back") ??
    view.actions.find((a) => a.id === "wrap");
  btn.hidden = !action;
  if (action) {
    btn.textContent = action.label;
    btn.dataset.action = action.id;
  }
}

function renderMode(view: View) {
  const compact = view.compact;
  el("compact-view").hidden = !compact;
  if (compact) {
    el("main-view").hidden = true;
    el("settings").hidden = true;
    el("hourly-pick").hidden = true;
    el("hourly-idle-view").hidden = true;
    el("hourly-add-view").hidden = true;
    el("hourly-edit-view").hidden = true;
    return;
  }
  el("hourly-idle-view").hidden = !hourlyIdle;
  el("hourly-pick").hidden = !hourlyPick || !!hourlyIdle;
  el("hourly-add-view").hidden = !hourlyAdd || !!hourlyIdle || !!hourlyPick;
  el("hourly-edit-view").hidden = !hourlyEdit || !!hourlyIdle || !!hourlyPick || hourlyAdd;
  if (hourlyPick || hourlyIdle || hourlyAdd || hourlyEdit) el("main-view").hidden = true;
  else if (el("settings").hidden && !mood && !chat && !whatsNew) {
    el("main-view").hidden = false;
  }
}

window.__cbbMiniShow = (view) => {
  current = view;
  pushedAt = Date.now();
  document.documentElement.dataset.platform = view.platform;
  const quit = view.platform === "windows" ? "Exit" : "Quit";
  el("quit-text").textContent = `${quit} CoolerBox Tracker`;
  el("quit").title = quit;
  el("quit").setAttribute("aria-label", `${quit} CoolerBox Tracker`);
  el("compact-view").dataset.state = view.state;
  el("c-status").textContent = view.status;
  // The thin mini timer shows the status and the summary on hover.
  el("compact-view").title = [view.status, summaryText(view)].filter(Boolean).join("\n");
  el("head").dataset.state = view.state;
  el("status").textContent = view.status;
  el("version").textContent = `CoolerBox Tracker ${view.version}`;
  el("version-hint").textContent = `You have ${view.version}.`;
  const on = String(view.autostart);
  el("autostart-row").setAttribute("aria-checked", on);
  el("autostart-switch").setAttribute("aria-checked", on);
  el("close-keep").setAttribute("aria-checked", String(!view.close_quits));
  el("close-quit").setAttribute("aria-checked", String(view.close_quits));
  el("close-quit").textContent = `${quit} the app`;
  const place = view.platform === "macos" ? "menu bar" : "system tray";
  el("close-hint").textContent = view.close_quits
    ? "Closing stops your timer, reminders and notifications until you reopen it."
    : `Stays in the ${place} so your timer, reminders and notifications keep going.`;
  renderPin(view.pinned);
  renderQuiet(view.quiet);
  renderNext(view.next_call);
  renderHourly(view.hourly);
  el("auto-offline").setAttribute("aria-checked", String(view.auto_offline));
  renderActions(view);
  renderNeeds(view);
  renderSummary(view);
  renderChats(view);
  renderCompactAction(view);
  restClockButtons();
  renderMode(view);
  tick();
};

/** "16:00", or "Tue 08:00" when it isn't today. */
function quietTime(iso: string): string {
  const at = new Date(iso);
  if (Number.isNaN(at.getTime())) return "";
  const time = at.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  return at.toDateString() === new Date().toDateString()
    ? time
    : `${at.toLocaleDateString([], { weekday: "short" })} ${time}`;
}

// Times and days as the tracker says them: Johannesburg, 24-hour.
const TRACKER_TZ = "Africa/Johannesburg";
const TRACKER_DAY = new Intl.DateTimeFormat("en-CA", { timeZone: TRACKER_TZ });
const TRACKER_TIME = new Intl.DateTimeFormat("en-GB", { hour: "2-digit", minute: "2-digit", hour12: false, timeZone: TRACKER_TZ });

/** "Today", "Tomorrow", or "Wed 14 Oct" for a business day (yyyy-mm-dd). */
function nextDayLabel(day: string): string {
  const today = TRACKER_DAY.format(new Date());
  const tomorrow = TRACKER_DAY.format(new Date(Date.parse(`${today}T12:00:00Z`) + 86_400_000));
  if (day === today) return "Today";
  if (day === tomorrow) return "Tomorrow";
  const at = new Date(`${day}T12:00:00Z`);
  return Number.isNaN(at.getTime())
    ? day
    : at.toLocaleDateString("en-GB", { weekday: "short", day: "numeric", month: "short", timeZone: "UTC" });
}

function renderNext(next: NextCall | null) {
  el("next").hidden = next === null;
  if (!next) return;
  el("next-day").textContent = nextDayLabel(next.day);
  const starts = new Date(next.starts_at);
  const ends = new Date(next.ends_at);
  el("next-time").textContent = Number.isNaN(starts.getTime())
    ? ""
    : Number.isNaN(ends.getTime())
      ? TRACKER_TIME.format(starts)
      : `${TRACKER_TIME.format(starts)}–${TRACKER_TIME.format(ends)}`;
  el("next-pencil").hidden = !next.pencilled;
  el("next-title").textContent = [next.project, next.title].filter(Boolean).join(" · ");
  el("next-title").title = el("next-title").textContent ?? "";
  const place = el<HTMLButtonElement>("next-place");
  place.hidden = !next.place;
  place.disabled = !next.map_url;
  place.title = next.map_url ? "Open in Maps" : "";
  el("next-place-text").textContent = next.place ?? "";
  el("next-place-text").classList.toggle("plain", !next.map_url);
  el("next-sheet").hidden = !next.sheet_href;
}

/** The do-not-disturb choice last made here (the tracker only says when it ends). */
let dndChoice: string | null = null;
let dndBusy = false;

function renderQuiet(quiet: Quiet) {
  const sw = (id: string, on: boolean) => el(id).setAttribute("aria-checked", String(on));
  sw("banners-tracker", quiet.tracker);
  sw("banners-walkie", quiet.walkie);
  sw("banners-channel1", quiet.channel1);
  el("banners-channel1").toggleAttribute("disabled", !quiet.walkie);
  const on = quiet.dnd_until !== "";
  if (!on) dndChoice = null;
  const until = on ? quietTime(quiet.dnd_until) : "";
  el("quiet-chip").hidden = !on;
  // Short, so the clock status beside it keeps its room; the title says it all.
  el("quiet-chip-text").textContent = until || "Quiet";
  el("quiet-chip").title = until ? `Do not disturb until ${until}. Change in Settings.` : "Do not disturb";
  el("quiet-chip").setAttribute("aria-label", el("quiet-chip").title);
  if (!dndBusy) {
    el("dnd-hint").textContent = on
      ? `On until ${until}. Call alerts still ring.`
      : "Also quiets the tracker website. Call alerts still ring.";
  }
  document.querySelectorAll<HTMLButtonElement>("#dnd-choice .seg").forEach((seg) => {
    const choice = seg.dataset.dnd ?? "";
    seg.setAttribute("aria-checked", String(on ? choice === dndChoice : choice === "off"));
    seg.disabled = dndBusy;
  });
}

function setDnd(choice: string) {
  if (dndBusy) return;
  dndBusy = true;
  dndChoice = choice === "off" ? null : choice;
  el("dnd-hint").textContent = choice === "off" ? "Turning off…" : "Turning on…";
  if (current) renderQuiet(current.quiet);
  invoke("mini_dnd", { choice })
    .catch((error) => dndFailed(String(error)))
    .finally(() => window.setTimeout(() => { dndBusy = false; if (current) renderQuiet(current.quiet); }, 400));
}

function dndFailed(message: string) {
  dndBusy = false;
  el("dnd-hint").textContent = message;
  document.querySelectorAll<HTMLButtonElement>("#dnd-choice .seg").forEach((seg) => { seg.disabled = false; });
}

declare global {
  interface Window {
    __cbbMiniDndFailed?: (message: string) => void;
  }
}
window.__cbbMiniDndFailed = (message) => dndFailed(message);

function renderPin(pinned: boolean) {
  const pin = el("pin");
  pin.setAttribute("aria-pressed", String(pinned));
  const label = pinned ? "Unpin" : "Pin as a floating timer";
  pin.setAttribute("aria-label", label);
  pin.title = label;
  el("head").classList.toggle("pinned", pinned);
  el("compact-view").classList.toggle("pinned", pinned);
  el("pin-hint").hidden = !pinned;
  const cPin = el("c-pin");
  cPin.setAttribute("aria-pressed", String(pinned));
  cPin.setAttribute("aria-label", label);
  cPin.title = label;
}

// ── Mood check-in (mini.rs ask_mood / mini_mood). Health information: it is
// only ever sent to the tracker through the app, never stored, logged or
// counted here. Words mirror the tracker's src/lib/mood-checkin.ts.
const MOOD_BANDS: Array<[number, number, string[]]> = [
  [1, 2, ["Drained", "Overwhelmed", "Low"]],
  [3, 4, ["Tired", "Stressed", "Flat"]],
  [5, 6, ["Okay", "Steady", "Meh"]],
  [7, 8, ["Good", "Focused", "Motivated"]],
  [9, 10, ["Great", "Energised", "On fire"]],
];
const MOOD_CAUSES: Array<[string, string]> = [["WORK", "Work"], ["PERSONAL", "Personal"], ["BOTH", "Both"], ["UNSAID", "Rather not say"]];

let mood: {
  moment: "IN" | "WRAP";
  stage: "ask" | "talk" | "done";
  score: number | null;
  word: string | null;
  ownWords: boolean;
  cause: string | null;
  sent: "answer" | "skip" | "talk-yes" | "talk-no" | null;
} | null = null;

function moodWordsFor(score: number): string[] {
  return MOOD_BANDS.find(([from, to]) => score >= from && score <= to)?.[2] ?? [];
}

function chip(label: string, on: boolean, onClick: () => void, cls = "mood-chip"): HTMLButtonElement {
  const b = document.createElement("button");
  b.type = "button";
  b.className = cls;
  b.setAttribute("role", "radio");
  b.setAttribute("aria-checked", String(on));
  b.textContent = label;
  b.addEventListener("click", onClick);
  return b;
}

function renderMood() {
  if (!mood) return;
  const m = mood;
  el("mood-ask").hidden = m.stage !== "ask";
  el("mood-talk").hidden = m.stage !== "talk";
  el("mood-done").hidden = m.stage !== "done";
  el("mood-actions").hidden = m.stage === "done";
  el("mood-question").textContent = m.moment === "IN" ? "How are you feeling today?" : "How was today?";

  const scale = el("mood-scale");
  scale.replaceChildren(...Array.from({ length: 10 }, (_, i) => i + 1).map((n) =>
    chip(String(n), m.score === n, () => {
      m.score = n;
      if (m.word && !moodWordsFor(n).includes(m.word)) m.word = null;
      renderMood();
    }, "mood-score")));
  el("mood-more").hidden = m.score === null;

  const words = el("mood-words");
  words.replaceChildren(
    ...(m.score === null ? [] : moodWordsFor(m.score)).map((w) =>
      chip(w, m.word === w && !m.ownWords, () => { m.word = m.word === w ? null : w; m.ownWords = false; renderMood(); })),
    chip("Something else…", m.ownWords, () => { m.ownWords = !m.ownWords; m.word = null; renderMood(); }),
  );
  el("mood-note").hidden = !m.ownWords;

  const causes = el("mood-causes");
  causes.replaceChildren(causes.querySelector(".label") as Node,
    ...MOOD_CAUSES.map(([key, label]) => chip(label, m.cause === key, () => { m.cause = m.cause === key ? null : key; renderMood(); })));

  el("mood-save").textContent = m.stage === "talk" ? "Yes, please" : "Save";
  el<HTMLButtonElement>("mood-save").disabled = m.sent !== null || (m.stage === "ask" && m.score === null);
  el<HTMLButtonElement>("mood-skip").disabled = m.sent !== null;
}

// The tracker's greeting after a clock tap: under the timer, or in place of
// the timer in the thin mini timer, for a few seconds.
let greetTimer = 0;
function greet(text: string) {
  window.clearTimeout(greetTimer);
  el("greeting").textContent = text;
  el("c-greeting").textContent = text;
  el("greeting").hidden = false;
  el("c-greeting").hidden = false;
  el("c-timer").hidden = true;
  greetTimer = window.setTimeout(() => {
    el("greeting").hidden = true;
    el("c-greeting").hidden = true;
    el("c-timer").hidden = false;
  }, 6000);
}

// What's new after an update (whats_new.rs): its own page, until "Got it".
let whatsNew = false;
function openWhatsNew(page: { version: string; items: { title: string; how: string }[] }) {
  if (mood || chat) return;
  whatsNew = true;
  el("whats-new-title").textContent = `New in ${page.version}`;
  el("whats-new-items").replaceChildren(...page.items.map((item) => {
    const li = document.createElement("li");
    const t = document.createElement("span");
    t.className = "t";
    t.textContent = item.title;
    const h = document.createElement("span");
    h.className = "h";
    h.textContent = item.how;
    li.append(t, h);
    return li;
  }));
  el("main-view").hidden = true;
  el("settings").hidden = true;
  el("compact-view").hidden = true;
  el("whats-new-view").hidden = false;
  el("whats-new-view").scrollTop = 0;
  el<HTMLButtonElement>("whats-new-done").focus({ preventScroll: true });
}

function closeWhatsNew() {
  if (!whatsNew) return;
  whatsNew = false;
  el("whats-new-view").hidden = true;
  el("main-view").hidden = false;
  void invoke("mini_whats_new_done");
}

function openMood(moment: "IN" | "WRAP", greeting?: string | null) {
  mood = { moment, stage: "ask", score: null, word: null, ownWords: false, cause: null, sent: null };
  el("mood-greeting").textContent = greeting ?? "";
  el("mood-greeting").hidden = !greeting;
  el<HTMLTextAreaElement>("mood-note").value = "";
  el("mood-error").hidden = true;
  el("main-view").hidden = true;
  el("settings").hidden = true;
  el("compact-view").hidden = true;
  el("mood-view").hidden = false;
  renderMood();
}

function closeMood() {
  if (!mood) return;
  mood = null;
  el("mood-view").hidden = true;
  el("main-view").hidden = false;
  void invoke("mini_mood_done");
}

function sendMood(request: Record<string, unknown>, sent: NonNullable<NonNullable<typeof mood>["sent"]>) {
  if (!mood) return;
  mood.sent = sent;
  el("mood-error").hidden = true;
  renderMood();
  invoke("mini_mood", { request }).catch((error) => moodResult({ ok: false, error: String(error) }));
}

function moodResult(r: { ok: boolean; offerTalk?: boolean; told?: string[]; error?: string }) {
  if (!mood) return;
  const sent = mood.sent;
  mood.sent = null;
  if (!r.ok) {
    el("mood-error").textContent = r.error || "That didn't save. Try again.";
    el("mood-error").hidden = false;
    renderMood();
    return;
  }
  if (sent === "answer" && r.offerTalk) { mood.stage = "talk"; renderMood(); return; }
  if (sent === "skip" || sent === "talk-no") { closeMood(); return; }
  const told = r.told ?? [];
  el("mood-done").textContent = sent === "talk-yes"
    ? `${told.length > 1 ? told.slice(0, -1).join(", ") + " and " + told[told.length - 1] : told[0] ?? "Your HR contact"} ${told.length > 1 ? "have" : "has"} been told. They'll be in touch.`
    : "Thanks. Noted, without your name.";
  mood.stage = "done";
  renderMood();
  window.setTimeout(closeMood, 2500);
}

declare global {
  interface Window {
    __cbbMiniMood?: (ask: { moment: "IN" | "WRAP"; greeting?: string | null }) => void;
    __cbbMiniGreet?: (text: string) => void;
    __cbbMiniWhatsNew?: (page: { version: string; items: { title: string; how: string }[] }) => void;
    __cbbMiniMoodResult?: (r: { ok: boolean; offerTalk?: boolean; told?: string[]; error?: string }) => void;
  }
}
window.__cbbMiniMood = (ask) => openMood(ask.moment, ask.greeting);
window.__cbbMiniGreet = (text) => greet(text);
window.__cbbMiniWhatsNew = (page) => openWhatsNew(page);
window.__cbbMiniMoodResult = (r) => moodResult(r);

// ── Walkie quick chat (walkie.rs). What people say is only drawn here: never
// stored, logged or counted. Opening a conversation marks it read, so Back
// without replying clears it too.
type Reaction = { key: StickerKey; count: number; mine: boolean; who: string[] };
type Line = {
  id: string; body: string; author: string; mine: boolean; createdAt: string; call: boolean; file: boolean;
  fileName?: string; fileGone?: boolean; reactions?: Reaction[];
};

// The tracker's walkie stickers (its walkie-reactions.tsx): drawn, never
// Unicode emoji, a black ink line over a brand-colour fill slipped a little
// off-register. Same shapes, colours and order, so the panel matches the web.
type StickerKey = "THUMBS_UP" | "HEART" | "FIRE" | "KISS" | "CRYING" | "FROWN" | "THINKING";
const INK = 'fill="none" stroke="#000" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"';
const FACE = "M12 3.6a8.4 8.4 0 1 1 0 16.8a8.4 8.4 0 1 1 0-16.8z";
const STICKERS: Record<StickerKey, { label: string; shape: string; fill: string; detail: string }> = {
  THUMBS_UP: {
    label: "Thumbs up", fill: "#0777BF", detail: "",
    shape: "M7.4 10.8l3.3-5.3c.6-.9 2-.5 2 .6v3.4h4.5c1.2 0 2 1.1 1.8 2.2l-1.2 5.6c-.2 1-1.1 1.7-2.1 1.7H7.4zM3.9 10.8h3.5v8.2H3.9z",
  },
  HEART: {
    label: "Heart", fill: "#DD347C",
    shape: "M12 20s-7.8-4.6-7.8-10.2A4.2 4.2 0 0 1 12 7.3a4.2 4.2 0 0 1 7.8 2.5C19.8 15.4 12 20 12 20z",
    detail: `<path d="M7.4 9.4a2 2 0 0 1 1.7-1.6" ${INK} stroke-width="1.1"/>`,
  },
  FIRE: {
    label: "Fire", fill: "url(#GRAD)",
    shape: "M12 21c-3.9 0-6.6-2.7-6.6-6.3 0-3.4 2.7-5.3 3.5-8.4 1.4 1.2 2 2.6 2 4.1 1.1-.9 1.7-2.7 1.4-4.8 3.4 2 6.3 5.3 6.3 9.1 0 3.6-2.7 6.3-6.6 6.3z",
    detail: '<path d="M12 21c-1.6 0-2.8-1.1-2.8-2.7 0-1.6 1.3-2.5 2-3.8 1.9 1.1 3.6 2.4 3.6 4 0 1.5-1.2 2.5-2.8 2.5z" fill="#E4BED0" stroke="#000" stroke-width="1.2" stroke-linejoin="round"/>',
  },
  KISS: {
    label: "Kiss", fill: "#E4BED0", shape: FACE,
    detail: `<path d="M8.3 10.3q1.3 1.1 2.6 0" ${INK}/><circle cx="14.8" cy="10.1" r="1" fill="#000"/>`
      + `<path d="M11.6 13.9q2 .5.2 1.3q2 .5.2 1.4" ${INK} stroke-width="1.4"/>`
      + '<path d="M18.6 3.4s-2.2-1.3-2.2-2.9a1.1 1.1 0 0 1 2.2-.4a1.1 1.1 0 0 1 2.2.4c0 1.6-2.2 2.9-2.2 2.9z" transform="translate(1.2 1.6)" fill="#DD347C" stroke="#000" stroke-width="1" stroke-linejoin="round"/>',
  },
  CRYING: {
    label: "Crying", fill: "#98D4EA", shape: FACE,
    detail: `<path d="M8.2 10.6q1.3-1 2.6 0M13.2 10.6q1.3-1 2.6 0" ${INK}/><path d="M9.4 16.3q2.6-2.2 5.2 0" ${INK}/>`
      + '<path d="M16.2 12.3s-1.5 2-1.5 3a1.5 1.5 0 0 0 3 0c0-1-1.5-3-1.5-3z" fill="#0777BF" stroke="#000" stroke-width="1.1" stroke-linejoin="round"/>',
  },
  FROWN: {
    label: "Frown", fill: "#D9B3F5", shape: FACE,
    detail: `<path d="M7.7 8.2l2.6.9M16.3 8.2l-2.6.9" ${INK}/><circle cx="9.3" cy="10.9" r="1" fill="#000"/>`
      + `<circle cx="14.7" cy="10.9" r="1" fill="#000"/><path d="M9 16.4q3-2.7 6 0" ${INK}/>`,
  },
  THINKING: {
    label: "Thinking", fill: "#D9B3F5", shape: "M11 5.6a7.6 7.6 0 1 1 0 15.2a7.6 7.6 0 1 1 0-15.2z",
    detail: `<path d="M12.6 8.9q1.3-.9 2.6 0" ${INK}/><circle cx="8.6" cy="11.8" r="1" fill="#000"/>`
      + `<circle cx="13.6" cy="11.4" r="1" fill="#000"/><path d="M9.2 16.1l3.8-.7" ${INK}/>`
      + '<circle cx="19.4" cy="5.6" r="1.4" fill="#A64EA6" stroke="#000" stroke-width="1"/><circle cx="21.7" cy="2.6" r=".9" fill="#A64EA6" stroke="#000" stroke-width="1"/>',
  },
};

let stickerIds = 0;
/** One sticker as an SVG element (constant markup only, no message text). */
function sticker(key: StickerKey, size = 16): Element {
  const s = STICKERS[key];
  const grad = `fire-${++stickerIds}`;
  const holder = document.createElement("span");
  holder.innerHTML =
    `<svg class="sticker" viewBox="0 0 24 24" width="${size}" height="${size}" aria-hidden="true" overflow="visible">`
    + (key === "FIRE" ? `<defs><linearGradient id="${grad}" x1="0" y1="1" x2="0" y2="0"><stop offset="0" stop-color="#DD347C"/><stop offset="1" stop-color="#A64EA6"/></linearGradient></defs>` : "")
    + `<path d="${s.shape}" fill="${s.fill.replace("GRAD", grad)}" transform="translate(1.3 1.2)"/>`
    + `<path d="${s.shape}" ${INK}/>${s.detail}</svg>`;
  return holder.firstElementChild as Element;
}

/** The stickers on a message, as chips: the sticker, how many, and who. */
function reactionChips(reactions: Reaction[], mine: boolean): HTMLElement | null {
  const known = reactions.filter((r) => r.key in STICKERS && r.count > 0);
  if (known.length === 0) return null;
  const bar = document.createElement("div");
  bar.className = "reactions";
  bar.classList.toggle("mine", mine);
  for (const r of known) {
    const chip = document.createElement("span");
    chip.className = "chip-react";
    chip.classList.toggle("yours", r.mine);
    chip.title = `${STICKERS[r.key].label}: ${r.who.join(", ")}`;
    chip.setAttribute("aria-label", chip.title);
    chip.appendChild(sticker(r.key));
    if (r.count > 1) {
      const n = document.createElement("span");
      n.textContent = String(r.count);
      chip.appendChild(n);
    }
    bar.appendChild(chip);
  }
  return bar;
}
type ChatResult = {
  ok: boolean;
  op?: "open" | "send";
  seq?: number;
  channelId?: string;
  name?: string;
  lines?: Line[];
  error?: string;
};

let chat: {
  key: string;
  kind: string;
  name: string;
  channelId: string | null;
  lines: Line[];
  busy: "open" | "send" | null;
  seq: number;
} | null = null;
let chatSeq = 0;
/** Unsent replies by conversation, while the app runs. */
const drafts = new Map<string, string>();

/** "just now", "12m", "3h", "2d" (the tracker's sinceLabel). */
function sinceLabel(iso: string): string {
  const at = Date.parse(iso);
  if (Number.isNaN(at)) return "";
  const minutes = Math.round((Date.now() - at) / 60_000);
  if (minutes < 1) return "just now";
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours}h`;
  return `${Math.round(hours / 24)}d`;
}

function initials(name: string): string {
  const words = name.trim().split(/\s+/).filter(Boolean);
  const letters = words.length > 1 ? [words[0][0], words[words.length - 1][0]] : [name.trim()[0] ?? "•"];
  return letters.join("").toUpperCase();
}

function renderChats(view: View) {
  const list = el("chats");
  list.replaceChildren();
  el("walkie").hidden = view.chats.length === 0;
  el("walkie-label").textContent = view.walkie_unread > 0 ? `Walkie · ${view.walkie_unread}` : "Walkie";
  for (const row of view.chats) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "chat-row";
    btn.classList.toggle("ringing", row.ringing !== null);
    const face = document.createElement("span");
    face.className = "face";
    face.textContent = initials(row.name);
    const txt = document.createElement("span");
    txt.className = "txt";
    const top = document.createElement("span");
    top.className = "top";
    const name = document.createElement("span");
    name.className = "name";
    name.textContent = row.name;
    const when = document.createElement("span");
    when.className = "when";
    when.textContent = sinceLabel(row.last_at);
    top.append(name, when);
    const preview = document.createElement("div");
    preview.className = "preview";
    preview.textContent = row.ringing !== null ? `${row.ringing || "Someone"} is calling` : row.preview;
    txt.append(top, preview);
    btn.append(face, txt);
    if (row.unread > 0) {
      const pill = document.createElement("span");
      pill.className = "pill";
      pill.textContent = row.unread > 99 ? "99+" : String(row.unread);
      btn.appendChild(pill);
    }
    btn.addEventListener("click", () => openChat(row));
    list.appendChild(btn);
  }
  const count = el("c-walkie-count");
  el("c-walkie").hidden = view.walkie_unread === 0 || view.chats.length === 0;
  count.textContent = view.walkie_unread > 99 ? "99+" : String(view.walkie_unread);
  el("c-walkie").title = `${view.walkie_unread} unread on the walkie`;
}

function renderChat() {
  if (!chat) return;
  const c = chat;
  el("chat-name").textContent = c.name;
  const box = el("chat-lines");
  box.replaceChildren();
  if (c.lines.length === 0) {
    const note = document.createElement("p");
    note.id = "chat-note";
    note.textContent = c.busy === "open" ? "Loading…" : c.channelId ? "Nothing said yet." : "";
    box.appendChild(note);
  }
  const group = c.kind !== "DIRECT";
  let previous: Line | null = null;
  for (const line of c.lines) {
    const row = document.createElement("div");
    row.className = "line";
    if (line.call) {
      row.classList.add("call");
      row.textContent = line.mine ? "You sent a call alert" : `${line.author || "Someone"} sent a call alert`;
    } else {
      row.classList.toggle("mine", line.mine);
      if (group && !line.mine && (previous === null || previous.call || previous.mine || previous.author !== line.author)) {
        const who = document.createElement("span");
        who.className = "who";
        who.textContent = line.author;
        row.appendChild(who);
      }
      const bubble = document.createElement("div");
      bubble.className = "bubble";
      if (line.body) bubble.textContent = line.body;
      if (line.file || line.fileGone) {
        const file = document.createElement("span");
        file.className = "file-name";
        file.textContent = line.fileGone ? "File removed" : line.fileName || "A file";
        if (!line.body) bubble.classList.add("file");
        bubble.appendChild(file);
      }
      bubble.title = new Date(line.createdAt).toLocaleString();
      row.appendChild(bubble);
      const chips = reactionChips(line.reactions ?? [], line.mine);
      if (chips) row.appendChild(chips);
    }
    box.appendChild(row);
    previous = line;
  }
  box.scrollTop = box.scrollHeight;
  el<HTMLButtonElement>("chat-send").disabled = c.busy !== null || c.channelId === null;
  el<HTMLTextAreaElement>("chat-input").disabled = c.channelId === null && c.busy === "open";
}

function showChatError(text: string | null) {
  el("chat-error").textContent = text ?? "";
  el("chat-error").hidden = text === null;
}

function growInput() {
  const input = el<HTMLTextAreaElement>("chat-input");
  input.style.height = "auto";
  input.style.height = `${Math.min(input.scrollHeight, 96)}px`;
}

function openChat(row: Pick<ChatRow, "key" | "kind" | "name">) {
  if (mood) return;
  chatSeq += 1;
  chat = { key: row.key, kind: row.kind, name: row.name, channelId: null, lines: [], busy: "open", seq: chatSeq };
  const input = el<HTMLTextAreaElement>("chat-input");
  input.value = drafts.get(row.key) ?? "";
  growInput();
  showChatError(null);
  el("main-view").hidden = true;
  el("settings").hidden = true;
  el("compact-view").hidden = true;
  el("chat-view").hidden = false;
  renderChat();
  invoke("mini_walkie_open", { key: row.key, seq: chatSeq }).catch((error) =>
    chatResult({ ok: false, op: "open", seq: chat?.seq, error: String(error) }));
}

function closeChat() {
  if (!chat) return;
  const draft = el<HTMLTextAreaElement>("chat-input").value;
  if (draft.trim()) drafts.set(chat.key, draft);
  else drafts.delete(chat.key);
  chat = null;
  el("chat-view").hidden = true;
  el("main-view").hidden = false;
  if (current) renderMode(current);
}

function sendReply() {
  if (!chat || chat.busy !== null || chat.channelId === null) return;
  const body = el<HTMLTextAreaElement>("chat-input").value.trim();
  if (!body) return;
  chatSeq += 1;
  chat.seq = chatSeq;
  chat.busy = "send";
  showChatError(null);
  renderChat();
  invoke("mini_walkie_send", { channelId: chat.channelId, body, seq: chatSeq }).catch((error) =>
    chatResult({ ok: false, op: "send", seq: chat?.seq, error: String(error) }));
}

function chatResult(r: ChatResult) {
  if (!chat || r.seq !== chat.seq) return;
  const op = chat.busy;
  chat.busy = null;
  if (!r.ok) {
    showChatError(r.error || (op === "send" ? "That didn't send. Try again." : "Couldn't open that chat."));
    renderChat();
    return;
  }
  chat.channelId = r.channelId ?? chat.channelId;
  if (r.name) chat.name = r.name;
  chat.lines = r.lines ?? [];
  if (r.op === "send") {
    el<HTMLTextAreaElement>("chat-input").value = "";
    drafts.delete(chat.key);
    growInput();
  }
  showChatError(null);
  renderChat();
  el<HTMLTextAreaElement>("chat-input").focus();
}

declare global {
  interface Window {
    __cbbMiniWalkieResult?: (r: ChatResult) => void;
    __cbbMiniOpenChat?: (row: { key: string; kind: string; name: string }) => void;
  }
}
window.__cbbMiniWalkieResult = (r) => chatResult(r);
// A clicked walkie banner opens its conversation here (mini.rs open_chat).
window.__cbbMiniOpenChat = (row) => {
  if (mood) return;
  if (whatsNew) closeWhatsNew();
  el("settings").hidden = true;
  openChat({ key: row.key, kind: row.kind, name: row.name || "Walkie" });
};

// ── Quick search (search.rs): the tracker's own search-as-you-type. ──
type Hit = { title: string; subtitle: string; href: string };
type SearchResult = { seq: number; ok: boolean; groups?: { label: string; items: Hit[] }[]; error?: string };

let searchSeq = 0;
let searchTimer: number | undefined;
let hits: Hit[] = [];
let activeHit = 0;

const HIT_ICONS: Record<string, string> = {
  Projects: "M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z",
  Deliverables: "M5 4h14v16H5zM9 8h6M9 12h6M9 16h3",
  Ideas: "M9 18h6M10 21h4M12 3a6 6 0 0 0-3.5 10.9V16h7v-2.1A6 6 0 0 0 12 3z",
};

function searchNote(text: string) {
  const box = el("search-results");
  const note = document.createElement("div");
  note.id = "search-note";
  note.textContent = text;
  box.replaceChildren(note);
  hits = [];
}

function setSearching(on: boolean) {
  el("lists").classList.toggle("searching", on);
  el("search-results").hidden = !on;
}

function clearSearch() {
  const input = el<HTMLInputElement>("search-input");
  input.value = "";
  searchSeq += 1;
  window.clearTimeout(searchTimer);
  el("search-clear").hidden = true;
  setSearching(false);
  el("search-results").replaceChildren();
  hits = [];
}

function onSearchInput() {
  const input = el<HTMLInputElement>("search-input");
  const q = input.value.trim();
  el("search-clear").hidden = input.value === "";
  window.clearTimeout(searchTimer);
  searchSeq += 1;
  if (q === "") return clearSearch();
  setSearching(true);
  if (q.length < 2) return searchNote("Keep typing…");
  const seq = searchSeq;
  searchTimer = window.setTimeout(() => {
    if (!hits.length) searchNote("Searching…");
    invoke("mini_search", { q, seq }).catch((error) => searchResult({ seq, ok: false, error: String(error) }));
  }, 200);
}

function searchResult(r: SearchResult) {
  if (r.seq !== searchSeq || el<HTMLInputElement>("search-input").value.trim().length < 2) return;
  if (!r.ok) return searchNote(r.error || "Could not search just now.");
  const box = el("search-results");
  box.replaceChildren();
  hits = [];
  for (const group of r.groups ?? []) {
    if (!group.items.length) continue;
    const label = document.createElement("div");
    label.className = "section";
    label.textContent = group.label;
    box.appendChild(label);
    for (const hit of group.items) {
      const index = hits.length;
      hits.push(hit);
      const row = document.createElement("button");
      row.type = "button";
      row.className = "hit";
      row.setAttribute("role", "option");
      row.dataset.index = String(index);
      const face = document.createElement("span");
      face.className = "face";
      face.appendChild(svg(HIT_ICONS[group.label] ?? HIT_ICONS.Projects));
      const txt = document.createElement("span");
      txt.className = "txt";
      const title = document.createElement("div");
      title.className = "title";
      title.textContent = hit.title;
      const sub = document.createElement("div");
      sub.className = "sub";
      sub.textContent = hit.subtitle;
      txt.append(title, sub);
      row.append(face, txt);
      row.addEventListener("click", () => openHit(index));
      row.addEventListener("mousemove", () => markHit(index));
      box.appendChild(row);
    }
  }
  if (!hits.length) return searchNote("Nothing found. The tracker searches projects, deliverables and ideas.");
  markHit(0);
}

function markHit(index: number) {
  activeHit = Math.max(0, Math.min(index, hits.length - 1));
  document.querySelectorAll<HTMLElement>("#search-results .hit").forEach((row) => {
    const on = Number(row.dataset.index) === activeHit;
    row.classList.toggle("active", on);
    row.setAttribute("aria-selected", String(on));
    if (on) row.scrollIntoView({ block: "nearest" });
  });
}

function openHit(index: number) {
  const hit = hits[index];
  if (!hit) return;
  void invoke("mini_open", { href: hit.href });
  clearSearch();
}

declare global {
  interface Window {
    __cbbMiniSearchResult?: (r: SearchResult) => void;
  }
}
window.__cbbMiniSearchResult = (r) => searchResult(r);

// ── the project timer (hourly.rs) ──

/** "Other…" is open: every project, billed by the hour first, then quoted. */
/**
 * "Other…" (`start`), "Which project was it for?" (`file`), another project
 * for a suggested stretch (`suggest`), or the project for a linked folder
 * (`link`).
 */
let hourlyPick: "start" | "file" | "suggest" | "link" | "now" | null = null;
let linkFolder = "";
/** Back from time away with the timer running: the question being asked. */
type HourlyIdleAsk = { project: string; billable: boolean; unfiled?: boolean; from: number; to: number };
let hourlyIdle: HourlyIdleAsk | null = null;
/**
 * After a tap the timer's buttons rest until the tracker's answer arrives
 * (the running timer changes) or 5 s pass, so a double-click can't start twice.
 */
let hourlyTapped: { running: string; until: number } | null = null;
let hourlyErrorTimer = 0;

function hourlyResting(): boolean {
  if (!hourlyTapped) return false;
  const running = current?.hourly?.running?.id ?? "";
  if (Date.now() > hourlyTapped.until || running !== hourlyTapped.running) {
    hourlyTapped = null;
    return false;
  }
  return true;
}

function restHourlyButtons() {
  const resting = hourlyResting();
  document.querySelectorAll<HTMLButtonElement>("#hourly-stop, #hourly-quick .h-chip:not(.other), #hourly-pick-list .h-row").forEach((btn) => {
    btn.disabled = resting;
  });
}

function hourlyError(text: string | null) {
  const box = el("hourly-error");
  window.clearTimeout(hourlyErrorTimer);
  box.textContent = text ?? "";
  box.hidden = !text;
  if (text) hourlyErrorTimer = window.setTimeout(() => hourlyError(null), 8000);
}

function hourlyTap(action: "start" | "stop", projectId?: string) {
  if (hourlyResting()) return;
  hourlyTapped = { running: current?.hourly?.running?.id ?? "", until: Date.now() + 5000 };
  restHourlyButtons();
  window.setTimeout(restHourlyButtons, 5050);
  hourlyError(null);
  const noteInput = el<HTMLInputElement>("hourly-note-input");
  const note = action === "start" ? noteInput.value.trim() : "";
  if (action === "start") noteInput.value = "";
  invoke("mini_hourly", { action, projectId: projectId ?? null, note: note || null }).catch((error) => {
    hourlyTapped = null;
    restHourlyButtons();
    hourlyError(String(error));
  });
}

/** "Vimeo · Logo refresh": client, then project, never the project code. */
function projectLabel(p: HourlyProject): string {
  const name = p.name || p.projectNumber;
  const client = p.clientName?.trim();
  return client ? `${client} · ${name}` : name;
}

/**
 * Turns a project `<select>` into the app's own picker: a button showing the
 * choice (project, then client and number), and a list with search under
 * it, inside the panel. The select stays, hidden, and keeps the value: call
 * the returned function after filling it.
 */
function projectPicker(select: HTMLSelectElement): () => void {
  const wrap = document.createElement("div");
  wrap.className = "pp";
  const button = document.createElement("button");
  button.type = "button";
  button.className = "pp-btn";
  button.setAttribute("aria-haspopup", "listbox");
  const shown = document.createElement("span");
  shown.className = "pp-text";
  button.innerHTML = "";
  button.append(shown);
  button.insertAdjacentHTML("beforeend", '<svg viewBox="0 0 24 24"><path d="M6 9l6 6 6-6"/></svg>');
  const pop = document.createElement("div");
  pop.className = "pp-pop";
  pop.hidden = true;
  const find = document.createElement("input");
  find.type = "search";
  find.placeholder = "Find a project";
  find.autocomplete = "off";
  find.spellcheck = false;
  find.setAttribute("aria-label", "Find a project");
  const list = document.createElement("div");
  list.className = "pp-list";
  list.setAttribute("role", "listbox");
  pop.append(find, list);
  select.hidden = true;
  select.after(wrap);
  wrap.append(button, pop);
  // A label's click would open the hidden select: send it to the button.
  select.closest("label")?.addEventListener("click", (e) => {
    if (!wrap.contains(e.target as Node)) {
      e.preventDefault();
      button.focus();
    }
  });

  const texts = (value: string, fallback: string): [string, string] => {
    const p = current?.hourly?.projects.find((x) => x.id === value);
    if (!p) return [fallback, ""];
    const sub = [p.clientName?.trim(), p.projectNumber, retainerText(p)].filter(Boolean).join(" · ");
    return [p.name || p.projectNumber, sub];
  };
  const lines = (into: HTMLElement, value: string, fallback: string) => {
    const [name, sub] = texts(value, fallback);
    const a = document.createElement("span");
    a.className = "pp-name";
    a.textContent = name;
    into.replaceChildren(a);
    if (sub) {
      const b = document.createElement("span");
      b.className = "pp-sub";
      b.textContent = sub;
      into.append(b);
    }
  };
  const refresh = () => {
    const option = select.selectedOptions[0];
    lines(shown, select.value, option?.text ?? "Choose a project");
  };

  let active = 0;
  const rows = () => [...list.querySelectorAll<HTMLButtonElement>(".pp-row")];
  const mark = (i: number) => {
    const all = rows();
    if (all.length === 0) return;
    active = Math.max(0, Math.min(all.length - 1, i));
    all.forEach((r, n) => r.classList.toggle("active", n === active));
    all[active].scrollIntoView({ block: "nearest" });
  };
  const draw = () => {
    const words = find.value.trim().toLowerCase().split(/\s+/).filter(Boolean);
    list.replaceChildren();
    const add = (option: HTMLOptionElement) => {
      const [name, sub] = texts(option.value, option.text);
      if (!words.every((w) => `${name} ${sub}`.toLowerCase().includes(w))) return;
      const row = document.createElement("button");
      row.type = "button";
      row.className = "pp-row";
      row.setAttribute("role", "option");
      row.setAttribute("aria-selected", String(option.value === select.value));
      const text = document.createElement("span");
      text.className = "pp-text";
      lines(text, option.value, option.text);
      row.insertAdjacentHTML("afterbegin", '<svg class="pp-check" viewBox="0 0 24 24"><path d="M5 12l5 5 9-10"/></svg>');
      row.append(text);
      row.addEventListener("click", () => choose(option.value));
      list.append(row);
    };
    for (const child of [...select.children]) {
      if (child instanceof HTMLOptGroupElement) {
        const before = list.childElementCount;
        const head = document.createElement("div");
        head.className = "pp-section";
        head.textContent = child.label;
        list.append(head);
        [...child.children].forEach((o) => add(o as HTMLOptionElement));
        if (list.childElementCount === before + 1) head.remove();
      } else if (child instanceof HTMLOptionElement) add(child);
    }
    if (rows().length === 0) {
      const empty = document.createElement("div");
      empty.className = "pp-empty";
      empty.textContent = "No project matches.";
      list.append(empty);
    }
    const chosen = rows().findIndex((r) => r.getAttribute("aria-selected") === "true");
    mark(words.length > 0 || chosen < 0 ? 0 : chosen);
  };
  const open = (yes: boolean) => {
    pop.hidden = !yes;
    wrap.classList.toggle("open", yes);
    button.setAttribute("aria-expanded", String(yes));
    if (!yes) return;
    find.value = "";
    draw();
    find.focus();
  };
  const choose = (value: string) => {
    select.value = value;
    select.dispatchEvent(new Event("change", { bubbles: true }));
    refresh();
    open(false);
    button.focus();
  };

  button.addEventListener("click", () => open(pop.hidden !== false));
  find.addEventListener("input", draw);
  pop.addEventListener("keydown", (e) => {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      mark(active + (e.key === "ArrowDown" ? 1 : -1));
    } else if (e.key === "Enter" && !e.isComposing) {
      e.preventDefault();
      rows()[active]?.click();
    } else if (e.key === "Escape") {
      // Closes the list, not the form under it.
      e.preventDefault();
      e.stopPropagation();
      open(false);
      button.focus();
    }
  });
  document.addEventListener("mousedown", (e) => {
    if (!pop.hidden && !wrap.contains(e.target as Node)) open(false);
  });
  // A form that hides closes its list too.
  new MutationObserver(() => {
    if (!pop.hidden && wrap.offsetParent === null) open(false);
  }).observe(document.body, { attributes: true, attributeFilter: ["hidden"], subtree: true });
  return refresh;
}

const projectPickers = new Map<string, () => void>();

/** Show a project select's (new) choice in its picker. */
function refreshPicker(id: string) {
  let refresh = projectPickers.get(id);
  if (!refresh) {
    refresh = projectPicker(el<HTMLSelectElement>(id));
    projectPickers.set(id, refresh);
  }
  refresh();
}

/** "2h 30m", "1h", "45m". */
function fmtMinutes(mins: number): string {
  const m = Math.round(Math.abs(mins));
  const h = Math.floor(m / 60);
  const rest = m % 60;
  return h === 0 ? `${rest}m` : rest === 0 ? `${h}h` : `${h}h ${rest}m`;
}

/** "Retainer · 2h 30m left", "Retainer used · 1h over", or null. */
function retainerText(p: HourlyProject | undefined): string | null {
  return retainerLine(p ? liveRetainer(p.retainerMinutesLeft, p.billable, p.clientName) : null);
}

function retainerLine(left: number | null): string | null {
  if (left === null) return null;
  return left >= 0 ? `Retainer · ${fmtMinutes(left)} left` : `Retainer used · ${fmtMinutes(left)} over`;
}

/** The running timer's label from its own client and project, or the list's. */
function runningLabel(r: HourlyRunning, projects: HourlyProject[]): string {
  if (r.projectId === null) return "Not yet filed";
  const name = r.projectName?.trim();
  if (name) {
    const client = r.clientName?.trim();
    return client ? `${client} · ${name}` : name;
  }
  const p = projects.find((x) => x.id === r.projectId);
  return p ? projectLabel(p) : "";
}

/** Minutes the running timer has run. */
function elapsedMinutes(r: HourlyRunning | null | undefined): number {
  const started = r ? Date.parse(r.startedAt) : NaN;
  return Number.isNaN(started) ? 0 : Math.max(0, Math.floor((Date.now() - started) / 60000));
}

/**
 * A retainer as it stands now: the tracker counts finished entries only, so
 * a billable timer running for the same client is taken off (a retainer is
 * per client).
 */
function liveRetainer(left: number | null | undefined, billable: boolean, clientName: string | null | undefined): number | null {
  if (left === null || left === undefined) return null;
  const r = current?.hourly?.running;
  const same = r && r.billable && billable && (r.clientName ?? null) === (clientName ?? null);
  return same ? left - elapsedMinutes(r) : left;
}

function projectTitle(p: HourlyProject): string {
  return [projectLabel(p), p.billable ? "Billable" : "Tracked only", retainerText(p)].filter(Boolean).join(" · ");
}

const PLAY = "M8 5v14l11-7z";

// ── Today: the entries so far, carried on or edited ──

/** "Vimeo · Logo refresh" for an entry, or "Not yet filed". */
function entryLabel(e: TodayEntry): string {
  if (e.projectId === null) return "Not yet filed";
  const name = e.projectName?.trim() || "A project";
  const client = e.clientName?.trim();
  return client ? `${client} · ${name}` : name;
}

function renderToday(t: Today | null) {
  el("hourly-today").hidden = t === null || t.entries.length === 0;
  if (!t) return;
  el("hourly-today-total").textContent =
    t.billableMinutes > 0 && t.billableMinutes < t.totalMinutes
      ? `${fmtMinutes(t.totalMinutes)} · ${fmtMinutes(t.billableMinutes)} billable`
      : fmtMinutes(t.totalMinutes);
  const list = el("hourly-today-list");
  list.replaceChildren();
  for (const e of t.entries) {
    const row = document.createElement("div");
    row.className = "t-row";
    const main = document.createElement("button");
    main.type = "button";
    main.className = "t-main";
    const running = e.endedAt === null;
    const from = TRACKER_TIME.format(new Date(e.startedAt));
    const to = running ? "now" : TRACKER_TIME.format(new Date(e.endedAt ?? e.startedAt));
    const label = document.createElement("span");
    label.className = "l";
    label.textContent = entryLabel(e);
    const when = document.createElement("span");
    when.className = "w";
    when.textContent = [`${from}–${to}`, e.note].filter(Boolean).join(" · ");
    main.append(label, when);
    main.disabled = e.billed;
    main.title = e.billed ? "On an invoice: it can't be changed." : "Edit";
    main.addEventListener("click", () => openHourlyEdit(e));
    const mins = document.createElement("span");
    mins.className = "t-mins";
    mins.textContent = fmtMinutes(e.minutes);
    row.append(main, mins);
    if (e.billed) {
      const tag = document.createElement("span");
      tag.className = "t-tag";
      tag.textContent = "Invoiced";
      row.appendChild(tag);
    } else if (!running) {
      // Carry on: a new timer on the same project and note, from now.
      const go = document.createElement("button");
      go.type = "button";
      go.className = "t-go";
      go.title = `Carry on: ${entryLabel(e)}`;
      go.setAttribute("aria-label", `Carry on with ${entryLabel(e)}`);
      go.appendChild(svg(PLAY));
      go.addEventListener("click", () => {
        hourlyError(null);
        invoke("mini_hourly_continue", { entryId: e.id }).catch((error) => hourlyError(String(error)));
      });
      row.appendChild(go);
    } else {
      const tag = document.createElement("span");
      tag.className = "t-tag";
      tag.textContent = "Running";
      row.appendChild(tag);
    }
    list.appendChild(row);
  }
}

let hourlyEdit: TodayEntry | null = null;

/** The edit form's times as moments on the entry's own day (an end before
 * the start runs past midnight). */
function editStretch(): { from: Date; to: Date | null } | null {
  if (!hourlyEdit) return null;
  const day = localDay(new Date(hourlyEdit.startedAt));
  const from = new Date(`${day}T${el<HTMLInputElement>("hourly-edit-from").value}`);
  if (Number.isNaN(from.getTime())) return null;
  if (hourlyEdit.endedAt === null) return { from, to: null };
  const to = new Date(`${day}T${el<HTMLInputElement>("hourly-edit-to").value}`);
  if (Number.isNaN(to.getTime())) return null;
  if (to <= from) to.setDate(to.getDate() + 1);
  return { from, to };
}

function renderEditLength() {
  const s = editStretch();
  el("hourly-edit-length").textContent =
    s === null ? "" : s.to === null ? "Still running" : fmtMinutes((s.to.getTime() - s.from.getTime()) / 60000);
}

function openHourlyEdit(e: TodayEntry) {
  const h = current?.hourly;
  if (!h || e.billed) return;
  if (chat) closeChat();
  showSettings(false);
  hourlyPick = null;
  hourlyEdit = e;
  const select = el<HTMLSelectElement>("hourly-edit-project");
  select.replaceChildren();
  if (e.projectId === null) select.appendChild(new Option("Not yet filed", ""));
  // Its own project, even one no longer in the list.
  if (e.projectId && !h.projects.some((p) => p.id === e.projectId)) select.appendChild(new Option(entryLabel(e), e.projectId));
  for (const [label, billable] of [["Billed by the hour", true], ["Quoted", false]] as const) {
    const rows = h.projects.filter((p) => p.billable === billable);
    if (rows.length === 0) continue;
    const group = document.createElement("optgroup");
    group.label = label;
    for (const p of rows) group.appendChild(new Option(projectLabel(p), p.id));
    select.appendChild(group);
  }
  select.value = e.projectId ?? "";
  refreshPicker("hourly-edit-project");
  const pad = (d: Date) => `${pad2(d.getHours())}:${pad2(d.getMinutes())}`;
  el<HTMLInputElement>("hourly-edit-from").value = pad(new Date(e.startedAt));
  const toInput = el<HTMLInputElement>("hourly-edit-to");
  toInput.value = e.endedAt ? pad(new Date(e.endedAt)) : "";
  toInput.disabled = e.endedAt === null;
  toInput.placeholder = e.endedAt === null ? "Running" : "";
  el<HTMLInputElement>("hourly-edit-note").value = e.note ?? "";
  el<HTMLInputElement>("hourly-edit-for-client").checked = !!e.noteForClient;
  el<HTMLButtonElement>("hourly-edit-delete").hidden = e.endedAt === null;
  el("hourly-edit-error").hidden = true;
  document.querySelectorAll<HTMLButtonElement>("#hourly-edit-view .h-choices button").forEach((b) => (b.disabled = false));
  renderEditLength();
  if (current) renderMode(current);
}

function closeHourlyEdit() {
  hourlyEdit = null;
  if (current) renderMode(current);
}

function editError(text: string) {
  el("hourly-edit-error").textContent = text;
  el("hourly-edit-error").hidden = false;
  document.querySelectorAll<HTMLButtonElement>("#hourly-edit-view .h-choices button").forEach((b) => (b.disabled = false));
}

function saveHourlyEdit() {
  const e = hourlyEdit;
  const s = editStretch();
  if (!e) return;
  if (!s) return editError("Fill in the times.");
  document.querySelectorAll<HTMLButtonElement>("#hourly-edit-view .h-choices button").forEach((b) => (b.disabled = true));
  el("hourly-edit-error").hidden = true;
  const project = el<HTMLSelectElement>("hourly-edit-project").value;
  // Saved when the tracker says so (__cbbMiniHourlyAfter closes the form).
  // Only times that were changed (the inputs are to the minute; the entry
  // keeps its seconds otherwise).
  const pad = (d: Date) => `${pad2(d.getHours())}:${pad2(d.getMinutes())}`;
  const fromChanged = el<HTMLInputElement>("hourly-edit-from").value !== pad(new Date(e.startedAt));
  const toChanged = e.endedAt !== null && el<HTMLInputElement>("hourly-edit-to").value !== pad(new Date(e.endedAt));
  invoke("mini_hourly_edit", {
    entryId: e.id,
    startedAt: fromChanged ? s.from.toISOString() : null,
    endedAt: toChanged && s.to ? s.to.toISOString() : null,
    note: el<HTMLInputElement>("hourly-edit-note").value,
    projectId: project || null,
    noteForClient: el<HTMLInputElement>("hourly-edit-for-client").checked,
  }).catch((error) => editError(String(error)));
}

function deleteHourlyEdit() {
  const e = hourlyEdit;
  if (!e) return;
  const button = el<HTMLButtonElement>("hourly-edit-delete");
  // A second tap confirms: nothing is deleted on one stray click.
  if (button.dataset.confirm !== "1") {
    button.dataset.confirm = "1";
    button.textContent = "Tap again to delete";
    window.setTimeout(() => {
      button.dataset.confirm = "";
      button.textContent = "Delete";
    }, 4000);
    return;
  }
  button.dataset.confirm = "";
  button.textContent = "Delete";
  document.querySelectorAll<HTMLButtonElement>("#hourly-edit-view .h-choices button").forEach((b) => (b.disabled = true));
  invoke("mini_hourly_delete", { entryId: e.id }).catch((error) => editError(String(error)));
}

// ── "Suggest time from my work" (detector.rs) ──

/** "Change apps…" has the whole list open. */
let detectorAppsOpen = false;

/** The privacy explainer: "ask" before switching on, "read" from the link. */
let detectorPrivacy: "ask" | "read" | null = null;

function showDetectorPrivacy(mode: "ask" | "read" | null) {
  detectorPrivacy = mode;
  renderDetector(current?.hourly?.detector ?? null);
  if (mode) el("detector-privacy").scrollIntoView({ block: "nearest" });
}

function renderDetector(d: Detector | null) {
  el("detector-settings").hidden = d === null;
  el("timer-settings").hidden = d === null;
  const card = el("hourly-suggest");
  const s = d?.suggestion ?? null;
  card.hidden = s === null;
  if (d === null) return;
  el("detector-row").setAttribute("aria-checked", String(d.on));
  el("detector-body").hidden = !d.on || detectorPrivacy !== null;
  // Switching it on always goes through what it sees and what it doesn't.
  if (d.on && detectorPrivacy === "ask") detectorPrivacy = null;
  el("detector-privacy").hidden = detectorPrivacy === null;
  el("detector-agree").hidden = detectorPrivacy !== "ask";
  el("detector-privacy-close").textContent = detectorPrivacy === "ask" ? "Not now" : "Close";
  const apps = el("detector-apps");
  apps.replaceChildren();
  // Just the picked apps, until "Change apps…" (or while none is picked)
  // opens the whole list, by kind: Design, Video, Audio, 3D, Code & AI,
  // Browser, Documents, Yours.
  const anyPicked = d.apps.some((a) => a.on);
  const showAll = detectorAppsOpen || !anyPicked;
  el("detector-change-apps").hidden = !anyPicked;
  el("detector-change-apps").textContent = detectorAppsOpen ? "Done" : "Change apps…";
  el("detector-change-apps").setAttribute("aria-expanded", String(detectorAppsOpen));
  el("detector-add-app").hidden = !showAll || !d.titlesSupported;
  let kind = "";
  for (const a of d.apps) {
    if (!showAll && !a.on) continue;
    if (!showAll) {
      const chip = document.createElement("span");
      chip.className = "h-chip";
      chip.setAttribute("aria-pressed", "true");
      chip.textContent = a.label;
      apps.appendChild(chip);
      continue;
    }
    if (a.kind !== kind) {
      kind = a.kind;
      const head = document.createElement("div");
      head.className = "d-kind";
      head.style.width = "100%";
      head.textContent = kind === "Browser" ? "Browser (only pages that name a project)" : kind;
      apps.appendChild(head);
    }
    const chip = document.createElement("button");
    chip.type = "button";
    chip.className = "h-chip";
    chip.textContent = a.label;
    chip.setAttribute("aria-pressed", String(a.on));
    chip.title = a.kind === "Yours" ? "Tap to remove" : "";
    chip.addEventListener("click", () => void invoke("mini_detector_app", { id: a.id }));
    apps.appendChild(chip);
  }
  apps.hidden = !d.titlesSupported;
  if (!d.titlesSupported || !showAll) el("detector-open").hidden = true;
  renderPermission(
    d.on ? d.permission ?? null : null,
    d.on && d.titlesSupported && d.titlesAllowed && d.apps.some((a) => a.on),
  );
  renderAutoCard();
  const folders = el("detector-folders");
  folders.replaceChildren();
  for (const f of d.folders) {
    const row = document.createElement("div");
    row.className = "d-folder";
    const text = document.createElement("span");
    text.className = "f";
    text.textContent = f.folder;
    const project = document.createElement("span");
    project.className = "p";
    project.textContent = f.project;
    text.appendChild(project);
    const remove = document.createElement("button");
    remove.type = "button";
    remove.textContent = "✕";
    remove.title = "Unlink";
    remove.setAttribute("aria-label", `Unlink ${f.folder}`);
    remove.addEventListener("click", () => void invoke("mini_detector_unlink", { key: f.key }));
    row.append(text, remove);
    folders.appendChild(row);
  }
  if (s) {
    const from = new Date(s.from);
    const to = new Date(s.to);
    const span = `${TRACKER_TIME.format(from)}–${TRACKER_TIME.format(to)} (${fmtMinutes((s.to - s.from) / 60000)})`;
    el("hourly-suggest-text").textContent = s.unrecognised
      ? `Worked in ${s.label}, ${span}. Which project was it?`
      : `Worked on ${s.label}, ${span}? Log it${s.billable ? " as billable time" : ""}?`;
    el("hourly-suggest-log").hidden = !!s.unrecognised;
    el("hourly-suggest-other").textContent = s.unrecognised ? "Pick the project…" : "Other…";
    el("hourly-suggest-more").textContent = d.more > 0 ? `${d.more} more to go through after this one.` : "";
    el("hourly-suggest-more").hidden = d.more === 0;
    card.querySelectorAll("button").forEach((b) => (b.disabled = false));
  }
}

/** macOS's Screen Recording permission: what to do, by where it's stuck. */
const SETTINGS_STEPS =
  "In System Settings, remove CoolerBox Tracker with −, add it again with + (from Applications), switch it on, then restart.";
function renderPermission(state: "ask" | "restart" | "stuck" | null, ok = false) {
  el("detector-permission").hidden = state === null;
  el("detector-perm-ok").hidden = state !== null || !ok;
  if (state === null) return;
  el("detector-permission-text").textContent = {
    ask:
      `To read window names, macOS needs the permission it calls "Screen Recording". The app never records or captures your screen. ` +
      `Already switched on there, and still seeing this? ${SETTINGS_STEPS}`,
    restart:
      `Once Screen Recording is switched on for CoolerBox Tracker in System Settings, restart the app to finish. ` +
      `If it was on already: ${SETTINGS_STEPS}`,
    stuck:
      "macOS is still hiding window names from CoolerBox Tracker, even though Screen Recording looks switched on. " +
      `Test builds and their updates need it given again. ${SETTINGS_STEPS} Or let the app ask macOS again. Linked folders keep working meanwhile.`,
  }[state];
  el("detector-permission-btn").hidden = state !== "ask";
  el("detector-reset-btn").hidden = state !== "stuck";
}

/**
 * The quick panel's "noticing" card: what Suggest time from my work is
 * counting right now, while no timer runs, with Start the timer from 10:45
 * (or Pick the project… for unrecognised work) and Not this. A suggestion
 * waiting for an answer takes its place. Refreshed each second for the
 * minutes.
 */
function renderAutoCard() {
  const h = current?.hourly ?? null;
  const d = h?.detector ?? null;
  const card = el("hourly-auto");
  const n = d?.now ?? null;
  const counting = !!(n && n.counting && n.since !== null);
  const warn = !!d?.permission;
  const show = !!d && d.on && !h?.running && !d.suggestion && (counting || warn);
  card.hidden = !show;
  if (!show || !d) return;
  el("hourly-auto-warn").hidden = !warn;
  el("hourly-auto-acts").hidden = !counting;
  el("hourly-auto-from").hidden = !counting;
  const text = el("hourly-auto-text");
  text.replaceChildren();
  if (!counting || !n || n.since === null) {
    text.textContent = "Suggest time from my work is on.";
    return;
  }
  const since = TRACKER_TIME.format(new Date(n.since));
  const mins = fmtMinutes(Math.max(0, Math.round((Date.now() - n.since) / 60000)));
  const unknown = (n.counting ?? "").startsWith("something in ");
  const b = document.createElement("b");
  if (unknown) {
    b.textContent = (n.counting ?? "").replace("something in ", "");
    text.append("Working in ", b, `, since ${since} (${mins}). No project recognised yet.`);
  } else {
    b.textContent = n.counting ?? "";
    text.append("Working on ", b, `, since ${since} (${mins})`);
  }
  el("hourly-auto-from").textContent =
    n.kind === "app" && n.picked ? `From ${n.app}${n.title ? ` · ${n.title}` : ""}` : n.kind === "away" ? "You're away from the computer." : "";
  el("hourly-auto-from").hidden = !el("hourly-auto-from").textContent;
  el<HTMLButtonElement>("hourly-auto-log").hidden = unknown || !n.canLog;
  el("hourly-auto-log").textContent = `Start timer from ${since}`;
  el<HTMLButtonElement>("hourly-auto-pick").hidden = !unknown;
}

function answerSuggestion(choice: "log" | "skip", projectId?: string) {
  const s = current?.hourly?.detector?.suggestion;
  if (!s) return;
  el("hourly-suggest").querySelectorAll("button").forEach((b) => (b.disabled = true));
  invoke("mini_detector_answer", { id: s.id, choice, projectId: projectId ?? null }).catch((error) => {
    el("hourly-suggest").querySelectorAll("button").forEach((b) => (b.disabled = false));
    hourlyError(String(error));
  });
}
const STAR = "M12 3.5l2.6 5.3 5.8.8-4.2 4.1 1 5.8-5.2-2.7-5.2 2.7 1-5.8-4.2-4.1 5.8-.8z";

// ── "Add time…": a stretch logged after the fact ──

let hourlyAdd = false;

function pad2(n: number): string {
  return String(n).padStart(2, "0");
}

/** Local yyyy-mm-dd and hh:mm, five-minute steps. */
function localDay(d: Date): string {
  return `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}`;
}
function localTime(d: Date): string {
  return `${pad2(d.getHours())}:${pad2(d.getMinutes() - (d.getMinutes() % 5))}`;
}

/** The form's stretch as moments, or null while it doesn't read right. */
function addStretch(): { from: Date; to: Date } | null {
  const day = el<HTMLInputElement>("hourly-add-day").value;
  const from = new Date(`${day}T${el<HTMLInputElement>("hourly-add-from").value}`);
  const to = new Date(`${day}T${el<HTMLInputElement>("hourly-add-to").value}`);
  if (Number.isNaN(from.getTime()) || Number.isNaN(to.getTime())) return null;
  // "22:00 to 01:00" runs past midnight.
  if (to <= from) to.setDate(to.getDate() + 1);
  return { from, to };
}

function renderAddLength() {
  const s = addStretch();
  el("hourly-add-length").textContent = s ? `${fmtMinutes((s.to.getTime() - s.from.getTime()) / 60000)}` : "";
}

function openHourlyAdd() {
  const h = current?.hourly;
  if (!h) return;
  if (chat) closeChat();
  showSettings(false);
  hourlyPick = null;
  hourlyAdd = true;
  const select = el<HTMLSelectElement>("hourly-add-project");
  select.replaceChildren();
  for (const [label, billable] of [["Billed by the hour", true], ["Quoted", false]] as const) {
    const rows = h.projects.filter((p) => p.billable === billable);
    if (rows.length === 0) continue;
    const group = document.createElement("optgroup");
    group.label = label;
    for (const p of rows) group.appendChild(new Option(projectLabel(p), p.id));
    select.appendChild(group);
  }
  // The last project timed, an hour ago until now.
  select.value = h.running?.projectId ?? h.projects[0]?.id ?? "";
  refreshPicker("hourly-add-project");
  // The day is the start's: just after midnight, the hour before began
  // yesterday (and ends today, after it).
  const now = new Date();
  const hourAgo = new Date(now.getTime() - 3600_000);
  el<HTMLInputElement>("hourly-add-day").value = localDay(hourAgo);
  el<HTMLInputElement>("hourly-add-from").value = localTime(hourAgo);
  el<HTMLInputElement>("hourly-add-to").value = localTime(now);
  el<HTMLInputElement>("hourly-add-note").value = "";
  el("hourly-add-error").hidden = true;
  el<HTMLButtonElement>("hourly-add-save").disabled = false;
  renderAddLength();
  if (current) renderMode(current);
  select.focus();
}

function closeHourlyAdd() {
  hourlyAdd = false;
  if (current) renderMode(current);
}

function addError(text: string) {
  el("hourly-add-error").textContent = text;
  el("hourly-add-error").hidden = false;
  el<HTMLButtonElement>("hourly-add-save").disabled = false;
}

function saveHourlyAdd() {
  const s = addStretch();
  if (!s) return addError("Pick a day and the times.");
  el<HTMLButtonElement>("hourly-add-save").disabled = true;
  el("hourly-add-error").hidden = true;
  const note = el<HTMLInputElement>("hourly-add-note").value.trim();
  // Saved when the tracker says so (__cbbMiniHourlyAfter closes the form).
  invoke("mini_hourly_add", {
    projectId: el<HTMLSelectElement>("hourly-add-project").value,
    startedAt: s.from.toISOString(),
    endedAt: s.to.toISOString(),
    note: note || null,
  }).catch((error) => addError(String(error)));
}

/** The running card's retainer line, live: called each push and each second. */
function renderRunningRetainer(r: HourlyRunning) {
  const left = r.projectId === null ? null : liveRetainer(r.retainerMinutesLeft, r.billable, r.clientName);
  const text = retainerLine(left);
  el("hourly-retainer").textContent = text ?? "";
  el("hourly-retainer").hidden = !text;
  el("hourly-retainer").classList.toggle("over", (left ?? 0) < 0);
}

let hourlyInfoTimer = 0;

/** A line about what happened (the timer stopped while they were idle). */
function hourlyInfo(text: string | null) {
  const box = el("hourly-info");
  window.clearTimeout(hourlyInfoTimer);
  box.textContent = text ?? "";
  box.hidden = !text;
  if (text) hourlyInfoTimer = window.setTimeout(() => hourlyInfo(null), 30000);
}

function renderHourly(h: Hourly | null) {
  el("hourly").hidden = h === null;
  el("hourly-start").hidden = h === null;
  renderDetector(h?.detector ?? null);
  renderToday(h?.today ?? null);
  // Edited away meanwhile (deleted, on the website): nothing to edit.
  if (hourlyEdit && !h?.today?.entries.some((e) => e.id === hourlyEdit?.id)) closeHourlyEdit();
  el("idle-settings").hidden = h === null;
  if (h === null) {
    if (hourlyPick) closeHourlyPick();
    if (hourlyIdle) closeHourlyIdle();
    return;
  }
  document.querySelectorAll<HTMLButtonElement>("#idle-choice .seg").forEach((seg) => {
    seg.setAttribute("aria-checked", String(Number(seg.dataset.idle) === h.idleMinutes));
  });
  const running = h.running;
  el("hourly-running").hidden = running === null;
  // Nothing left to stop: the wrap question goes.
  if (running === null) el("hourly-wrap").hidden = true;
  if (running) {
    const unfiled = running.projectId === null;
    el("hourly-project").textContent = runningLabel(running, h.projects);
    renderRunningRetainer(running);
    el("hourly-project").classList.toggle("unfiled", unfiled);
    const kind = el("hourly-kind");
    kind.hidden = unfiled;
    kind.textContent = running.billable ? "Billable" : "Tracked only";
    kind.classList.toggle("tracked", !running.billable);
    kind.title = running.billable ? "An hourly project: this time is billed" : "A quoted project: this time is tracked, not billed";
    el("hourly-stop").title = unfiled ? "Stop, then pick the project on Hours" : "Stop the timer";
    el("hourly-card").title = unfiled
      ? "Not billable until it's filed under a project. Click for My Hours."
      : `${el("hourly-project").textContent ?? ""}. Click for My Hours.`;
    el("hourly-note").textContent = running.note ?? "";
    el("hourly-note").hidden = !running.note;
  }
  const others = h.projects.filter((p) => p.id !== running?.projectId);
  el("hourly-label").textContent = running ? "Switch to" : "Time a project";
  el("hourly-label").title = `${running ? "Stop" : "Start your last project"} from anywhere: ${h.shortcut}`;
  el("hourly-add-open").hidden = h.projects.length === 0;
  el("hourly-label").hidden = running !== null && others.length === 0;
  // With nothing running, "Just start" is always there, projects or not.
  el("hourly-note-input").hidden = running !== null && others.length === 0;
  const quick = el("hourly-quick");
  quick.replaceChildren();
  for (const p of h.quick) {
    const chip = document.createElement("button");
    chip.type = "button";
    chip.className = "h-chip";
    chip.title = `${running ? "Switch to" : "Start"} ${projectTitle(p)}`;
    const name = document.createElement("span");
    name.textContent = projectLabel(p);
    const pinned = (h.pinned ?? []).includes(p.id);
    if (pinned) chip.classList.add("pinned");
    chip.append(svg(pinned ? STAR : PLAY), name);
    if (p.id === h.booked) {
      const tag = document.createElement("span");
      tag.className = "h-booked";
      tag.textContent = "Booked today";
      chip.appendChild(tag);
      chip.title = `You're booked on this today (My Schedule). ${chip.title}`;
    }
    chip.addEventListener("click", () => hourlyTap("start", p.id));
    quick.appendChild(chip);
  }
  if (!running) {
    const later = document.createElement("button");
    later.type = "button";
    later.className = "h-chip later";
    later.title = "Start the clock now and pick the project when you stop";
    const text = document.createElement("span");
    text.textContent = "Just start, file it later";
    later.append(svg(PLAY), text);
    later.addEventListener("click", () => hourlyTap("start"));
    quick.appendChild(later);
  }
  if (others.length > h.quick.length) {
    const other = document.createElement("button");
    other.type = "button";
    other.className = "h-chip other";
    other.textContent = "Other…";
    other.addEventListener("click", () => openHourlyPick("start"));
    quick.appendChild(other);
  }
  // Filed meanwhile (on the website, say), or put off: nothing to ask.
  if (hourlyPick === "file" && !h.filing) closeHourlyPick();
  // Still waiting for its project (the view was left): ask again.
  if (h.filing && hourlyPick === null && !hourlyIdle && !hourlyAdd && !mood && !whatsNew && !h.asking) openHourlyPick("file");
  if (hourlyPick) renderHourlyPick();
  // A question whose timer stopped meanwhile (on the website, say) is moot.
  if (hourlyIdle && !h.asking) closeHourlyIdle();
  restHourlyButtons();
}

function renderHourlyPick() {
  const h = current?.hourly;
  const list = el("hourly-pick-list");
  list.replaceChildren();
  if (!h) return;
  const words = el<HTMLInputElement>("hourly-filter").value.trim().toLowerCase().split(/\s+/).filter(Boolean);
  const fits = (p: HourlyProject) => {
    const text = `${p.projectNumber} ${p.name} ${p.clientName ?? ""}`.toLowerCase();
    return words.every((w) => text.includes(w));
  };
  let shown = 0;
  for (const [label, billable] of [["Billed by the hour", true], ["Quoted", false]] as const) {
    const rows = h.projects.filter((p) => p.billable === billable && fits(p));
    if (rows.length === 0) continue;
    const head = document.createElement("div");
    head.className = "section";
    head.textContent = label;
    list.appendChild(head);
    for (const p of rows) {
      const row = document.createElement("button");
      row.type = "button";
      const filing = hourlyPick === "file";
      const choosing = hourlyPick !== "start";
      const on = !choosing && p.id === h.running?.projectId;
      row.className = on ? "h-row on" : "h-row";
      const t = document.createElement("span");
      t.className = "t";
      t.textContent = projectLabel(p);
      const sub = document.createElement("span");
      sub.className = "s";
      sub.textContent = retainerText(p) ?? "";
      row.append(t);
      if (sub.textContent) row.append(sub);
      row.addEventListener("click", () => {
        if (filing) {
          fileHourly("file", p.id);
          return;
        }
        if (hourlyPick === "now") {
          invoke("mini_detector_log_now", { projectId: p.id })
            .catch((error) => hourlyError(String(error)))
            .finally(() => closeHourlyPick());
          return;
        }
        if (hourlyPick === "suggest") {
          answerSuggestion("log", p.id);
          closeHourlyPick();
          return;
        }
        if (hourlyPick === "link") {
          invoke("mini_detector_link_project", { projectId: p.id })
            .catch((error) => hourlyError(String(error)))
            .finally(() => closeHourlyPick());
          return;
        }
        if (!on) hourlyTap("start", p.id);
        closeHourlyPick();
      });
      const line = document.createElement("div");
      line.className = "h-line";
      line.appendChild(row);
      if (!choosing) {
        // Pin a favourite to the front of the one-tap starts.
        const pinned = (h.pinned ?? []).includes(p.id);
        const pin = document.createElement("button");
        pin.type = "button";
        pin.className = "h-pin";
        pin.setAttribute("aria-pressed", String(pinned));
        pin.setAttribute("aria-label", `${pinned ? "Unpin" : "Pin"} ${projectLabel(p)}`);
        pin.title = pinned ? "Unpin" : "Pin to the top";
        pin.appendChild(svg(STAR));
        pin.addEventListener("click", () => void invoke("mini_hourly_pin", { projectId: p.id, pinned: !pinned }));
        line.appendChild(pin);
      }
      list.appendChild(line);
      shown += 1;
    }
  }
  if (hourlyPick === "link" && words.length === 0) {
    // A folder full of client and project folders: any of them.
    const row = document.createElement("button");
    row.type = "button";
    row.className = "h-row h-new";
    const t = document.createElement("span");
    t.className = "t";
    t.textContent = "Any project, from its folders' names";
    const sub = document.createElement("span");
    sub.className = "s";
    sub.textContent = "For your Work folder or a drive: it tells the project from the client and project folders inside.";
    row.append(t, sub);
    row.addEventListener("click", () => {
      invoke("mini_detector_link_project", { projectId: "*" })
        .catch((error) => hourlyError(String(error)))
        .finally(() => closeHourlyPick());
    });
    const line = document.createElement("div");
    line.className = "h-line";
    line.appendChild(row);
    list.prepend(line);
    shown += 1;
  }
  if ((h.clients ?? []).length > 0) {
    const row = document.createElement("button");
    row.type = "button";
    row.className = "h-row h-new";
    const t = document.createElement("span");
    t.className = "t";
    t.textContent = "+ New project…";
    row.append(t);
    row.addEventListener("click", () => openHourlyNew(el<HTMLInputElement>("hourly-filter").value.trim()));
    const line = document.createElement("div");
    line.className = "h-line";
    line.appendChild(row);
    list.appendChild(line);
  } else if (shown === 0) {
    const empty = document.createElement("div");
    empty.id = "hourly-pick-empty";
    empty.textContent = words.length ? "No project matches." : "No projects to time yet.";
    list.appendChild(empty);
  }
  restHourlyButtons();
}

// ── "New project…" (POST /api/me/projects) ──

let hourlyNewBillable = true;

function showHourlyNew(open: boolean) {
  el("hourly-new").hidden = !open;
  el("hourly-filter").hidden = open;
  el("hourly-pick-list").hidden = open;
  if (hourlyPick === "file") el("hourly-pick-foot").hidden = open;
}

function openHourlyNew(name: string) {
  const h = current?.hourly;
  if (!h) return;
  const select = el<HTMLSelectElement>("hourly-new-client");
  select.replaceChildren(new Option("New client…", "__new"));
  for (const c of h.clients ?? []) select.appendChild(new Option(c.name, c.id));
  select.value = "__new";
  refreshPicker("hourly-new-client");
  el<HTMLInputElement>("hourly-new-name").value = name;
  el<HTMLInputElement>("hourly-new-client-name").value = "";
  el("hourly-new-client-name-field").hidden = false;
  setNewBillable(true);
  el("hourly-new-error").hidden = true;
  el("hourly-new-save").textContent = {
    start: "Create and start the timer",
    file: "Create and file the time",
    suggest: "Create and log the time",
    link: "Create and link the folders",
    now: "Create and start the timer",
  }[hourlyPick ?? "start"];
  showHourlyNew(true);
  el("hourly-new-name").focus();
}

function setNewBillable(billable: boolean) {
  hourlyNewBillable = billable;
  document.querySelectorAll<HTMLButtonElement>("#hourly-new-billing .seg").forEach((b) => {
    b.setAttribute("aria-checked", String((b.dataset.billable === "1") === billable));
  });
}

function newError(text: string) {
  el("hourly-new-error").textContent = text;
  el("hourly-new-error").hidden = false;
  document.querySelectorAll<HTMLButtonElement>("#hourly-new button").forEach((b) => (b.disabled = false));
}

function saveHourlyNew() {
  const client = el<HTMLSelectElement>("hourly-new-client").value;
  const mode = hourlyPick ?? "start";
  document.querySelectorAll<HTMLButtonElement>("#hourly-new button").forEach((b) => (b.disabled = true));
  el("hourly-new-error").hidden = true;
  invoke<HourlyProject>("mini_hourly_new_project", {
    name: el<HTMLInputElement>("hourly-new-name").value,
    clientId: client === "__new" ? null : client,
    newClientName: client === "__new" ? el<HTMLInputElement>("hourly-new-client-name").value : null,
    billable: hourlyNewBillable,
  })
    .then((p) => {
      document.querySelectorAll<HTMLButtonElement>("#hourly-new button").forEach((b) => (b.disabled = false));
      showHourlyNew(false);
      // Then what the list was open for.
      if (mode === "file") return fileHourly("file", p.id);
      if (mode === "suggest") {
        answerSuggestion("log", p.id);
        return closeHourlyPick();
      }
      if (mode === "now") {
        invoke("mini_detector_log_now", { projectId: p.id })
          .catch((error) => hourlyError(String(error)))
          .finally(() => closeHourlyPick());
        return;
      }
      if (mode === "link") {
        invoke("mini_detector_link_project", { projectId: p.id })
          .catch((error) => hourlyError(String(error)))
          .finally(() => closeHourlyPick());
        return;
      }
      hourlyTap("start", p.id);
      closeHourlyPick();
    })
    .catch((error) => newError(String(error)));
}

/**
 * "Other…" (`start`), or "Which project was it for?" (`file`) right after an
 * unfiled timer ended, with what happened (`hint`).
 */
function openHourlyPick(mode: "start" | "file" | "suggest" | "link" | "now" = "start", hint: string | null = null) {
  if (mode !== "start") {
    if (chat) closeChat();
    if (mode === "file") showSettings(false);
  }
  if (mode === "link") el("settings").hidden = true;
  hourlyPick = mode;
  const filing = mode === "file";
  el("hourly-pick-title").textContent = {
    start: "Time a project",
    file: "Which project was it for?",
    suggest: "Which project was it?",
    link: "Link to…",
    now: "Which project is it?",
  }[mode];
  el("hourly-pick-hint").textContent =
    hint ?? (filing ? "The time you just stopped isn't filed under a project yet." : mode === "link" ? linkFolder : "");
  el("hourly-pick-hint").hidden = !el("hourly-pick-hint").textContent;
  el("hourly-pick-foot").hidden = !filing;
  el("hourly-pick-back").hidden = filing;
  el("hourly-pick-error").hidden = true;
  setPickBusy(false);
  el<HTMLInputElement>("hourly-filter").value = "";
  showHourlyNew(false);
  renderHourlyPick();
  if (current) renderMode(current);
  el("hourly-filter").focus();
}

function closeHourlyPick() {
  const was = hourlyPick;
  hourlyPick = null;
  if (current) renderMode(current);
  // Linking a folder came from Settings: back there.
  if (was === "link") showSettings(true);
}

function setPickBusy(busy: boolean) {
  document.querySelectorAll<HTMLButtonElement>("#hourly-pick-foot button").forEach((b) => (b.disabled = busy));
  if (hourlyPick === "file") {
    document.querySelectorAll<HTMLButtonElement>("#hourly-pick-list .h-row").forEach((b) => (b.disabled = busy));
  }
}

/** File the stretch under a project, make a new project for it on the
 * website, or leave it for later. */
function fileHourly(choice: "file" | "new" | "later", projectId?: string) {
  setPickBusy(true);
  el("hourly-pick-error").hidden = true;
  invoke("mini_hourly_file", { choice, projectId: projectId ?? null })
    .then(() => {
      // Filed when the tracker says so (the next push); the others are done.
      if (choice !== "file") closeHourlyPick();
    })
    .catch((error) => {
      setPickBusy(false);
      el("hourly-pick-error").textContent = String(error);
      el("hourly-pick-error").hidden = false;
    });
}

function openHourlyIdle(ask: HourlyIdleAsk) {
  if (chat) closeChat();
  showSettings(false);
  hourlyPick = null;
  hourlyIdle = ask;
  const from = new Date(ask.from);
  const to = new Date(ask.to);
  const left = TRACKER_TIME.format(from);
  const away = fmtSpan((ask.to - ask.from) / 60000);
  const timer = ask.unfiled
    ? "Your timer (not yet filed)"
    : `Your timer on ${ask.project} (${ask.billable ? "billable" : "tracked only"})`;
  el("hourly-idle-text").textContent =
    `${timer} kept running while you were idle for ${away}, from ${left} to ${TRACKER_TIME.format(to)}. ` +
    `Dropping the idle time ends it at ${left}.`;
  el("hourly-idle-error").hidden = true;
  document.querySelectorAll<HTMLButtonElement>(".h-choices button").forEach((b) => (b.disabled = false));
  if (current) renderMode(current);
}

function closeHourlyIdle() {
  hourlyIdle = null;
  if (current) renderMode(current);
}

function answerHourlyIdle(choice: "keep" | "resume" | "stop") {
  if (!hourlyIdle) return;
  document.querySelectorAll<HTMLButtonElement>(".h-choices button").forEach((b) => (b.disabled = true));
  // The view goes when the tracker has the answer (the next push says
  // nothing is being asked); a refusal shows here and can be answered again.
  invoke("mini_hourly_idle", { choice })
    .then(() => {
      if (choice === "keep") closeHourlyIdle();
    })
    .catch((error) => idleError(String(error)));
}

function idleError(text: string) {
  el("hourly-idle-error").textContent = text;
  el("hourly-idle-error").hidden = false;
  document.querySelectorAll<HTMLButtonElement>(".h-choices button").forEach((b) => (b.disabled = false));
}

declare global {
  interface Window {
    __cbbMiniHourlyError?: (text: string) => void;
    __cbbMiniHourlyIdle?: (ask: HourlyIdleAsk) => void;
    __cbbMiniHourlyAfter?: (after: { file: boolean; notice: string | null }) => void;
    __cbbMiniHourlyAdd?: () => void;
    __cbbMiniHourlyWrap?: (ask: { project: string }) => void;
    __cbbMiniDetectorLink?: (ask: { folder: string }) => void;
  }
}
window.__cbbMiniHourlyError = (text) => {
  if (hourlyAdd) return addError(text);
  if (hourlyEdit) return editError(text);
  if (hourlyIdle) return idleError(text);
  hourlyTapped = null;
  restHourlyButtons();
  hourlyError(text);
};
window.__cbbMiniHourlyIdle = (ask) => openHourlyIdle(ask);
// "Add time…" from the tray or Dock menu.
window.__cbbMiniHourlyAdd = () => openHourlyAdd();
// A folder was chosen to link: which project is it for?
window.__cbbMiniDetectorLink = (ask) => {
  linkFolder = ask.folder;
  openHourlyPick("link");
};
// Wrapped on the time sheet with the timer running: stop it too?
window.__cbbMiniHourlyWrap = (ask) => {
  el("hourly-wrap-text").textContent = `You've wrapped. Stop the timer on ${ask.project} too?`;
  el("hourly-wrap").hidden = false;
};
// An unfiled timer ended (ask which project it was for), and/or the timer
// stopped while they were idle (say so).
window.__cbbMiniHourlyAfter = (after) => {
  if (hourlyIdle) closeHourlyIdle();
  if (hourlyAdd) closeHourlyAdd();
  if (hourlyEdit) closeHourlyEdit();
  if (after.file) openHourlyPick("file", after.notice);
  else hourlyInfo(after.notice);
};

function showSettings(open: boolean) {
  if (!el("compact-view").hidden || mood || chat || whatsNew || hourlyIdle) return;
  // Opening Settings leaves the timer's views; closing it (as on blur)
  // keeps a filing question or a half-typed Add time.
  if (open && hourlyAdd) closeHourlyAdd();
  if (open && hourlyEdit) closeHourlyEdit();
  if (open && hourlyPick) closeHourlyPick();
  if (!open && (hourlyAdd || hourlyPick || hourlyEdit)) {
    el("settings").hidden = true;
    return;
  }
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
  click("new-window", () => menu("new-window"));
  click("update", () => menu("update"));
  click("whats-new-row", () => { showSettings(false); void invoke("mini_whats_new"); });
  click("whats-new-done", closeWhatsNew);
  click("update-row", () => menu("update"));
  click("diagnostics-row", () => menu("diagnostics"));
  click("autostart-row", () => menu("autostart"));
  click("close-keep", () => menu("close-keep"));
  click("close-quit", () => menu("close-quit"));
  click("quit", () => menu("quit"));
  click("quit-row", () => menu("quit"));
  click("open-settings", () => showSettings(true));
  click("quiet-chip", () => showSettings(true));
  click("banners-tracker", () => menu("banners-tracker"));
  click("banners-walkie", () => menu("banners-walkie"));
  click("banners-channel1", () => menu("banners-channel1"));
  click("auto-offline", () => menu("auto-offline"));
  click("hourly-stop", () => hourlyTap("stop"));
  click("hourly-add-open", openHourlyAdd);
  click("hourly-edit-cancel", closeHourlyEdit);
  click("hourly-edit-delete", deleteHourlyEdit);
  el("hourly-edit-view").addEventListener("submit", (e) => {
    e.preventDefault();
    saveHourlyEdit();
  });
  for (const id of ["hourly-edit-from", "hourly-edit-to"]) {
    el(id).addEventListener("input", renderEditLength);
  }
  click("hourly-suggest-log", () => answerSuggestion("log"));
  click("hourly-suggest-skip", () => answerSuggestion("skip"));
  click("hourly-suggest-other", () => openHourlyPick("suggest"));
  click("hourly-new-cancel", () => showHourlyNew(false));
  el("hourly-new").addEventListener("submit", (e) => {
    e.preventDefault();
    saveHourlyNew();
  });
  document.querySelectorAll<HTMLButtonElement>("#hourly-new-billing .seg").forEach((b) =>
    b.addEventListener("click", () => setNewBillable(b.dataset.billable === "1")),
  );
  el("hourly-new-client").addEventListener("change", () => {
    const fresh = el<HTMLSelectElement>("hourly-new-client").value === "__new";
    el("hourly-new-client-name-field").hidden = !fresh;
    if (fresh) el("hourly-new-client-name").focus();
  });
  click("detector-add-app", () => {
    const box = el("detector-open");
    if (!box.hidden) {
      box.hidden = true;
      return;
    }
    invoke<{ id: string; label: string }[]>("mini_detector_open_apps")
      .then((open) => {
        box.replaceChildren();
        const hint = document.createElement("p");
        hint.className = "d-hint";
        hint.textContent = open.length
          ? "Apps open now. Tap one to add it. Not here? Open it, then try again."
          : "No other apps are open. Open the one you want, then try again.";
        box.append(hint);
        for (const a of open) {
          const chip = document.createElement("button");
          chip.type = "button";
          chip.className = "h-chip";
          chip.textContent = `+ ${a.label}`;
          chip.addEventListener("click", () => {
            box.hidden = true;
            void invoke("mini_detector_app", { id: a.id });
          });
          box.append(chip);
        }
        box.hidden = false;
      })
      .catch((error) => hourlyError(String(error)));
  });
  click("detector-restart-btn", () => void invoke("mini_detector_restart"));
  click("detector-change-apps", () => {
    detectorAppsOpen = !detectorAppsOpen;
    renderDetector(current?.hourly?.detector ?? null);
  });
  click("detector-open-settings", () => void invoke("mini_detector_open_settings"));
  click("detector-reset-btn", () => {
    invoke("mini_detector_reset_permission").catch((error) => {
      el("detector-permission-text").textContent = String(error);
    });
  });
  click("hourly-auto-log", () => {
    el<HTMLButtonElement>("hourly-auto-log").disabled = true;
    invoke("mini_detector_log_now", { projectId: null })
      .catch((error) => hourlyError(String(error)))
      .finally(() => (el<HTMLButtonElement>("hourly-auto-log").disabled = false));
  });
  click("hourly-auto-pick", () => openHourlyPick("now"));
  click("hourly-auto-drop", () => void invoke("mini_detector_drop_now"));
  click("hourly-auto-fix", () => {
    showSettings(true);
    el("detector-permission").scrollIntoView({ block: "center" });
  });
  click("detector-row", () => {
    // On: off straight away. Off: first say what it sees and what it doesn't.
    if (current?.hourly?.detector?.on) {
      detectorPrivacy = null;
      void invoke("mini_detector_toggle");
    } else showDetectorPrivacy(detectorPrivacy === "ask" ? null : "ask");
  });
  click("detector-agree", () => {
    detectorPrivacy = null;
    void invoke("mini_detector_toggle");
  });
  click("detector-privacy-close", () => showDetectorPrivacy(null));
  click("detector-privacy-open", () => showDetectorPrivacy("read"));
  click("detector-link", () => void invoke("mini_detector_link").catch((error) => hourlyError(String(error))));
  click("detector-permission-btn", () => void invoke("mini_detector_permission"));
  click("hourly-wrap-stop", () => {
    el("hourly-wrap").hidden = true;
    hourlyTap("stop");
  });
  click("hourly-wrap-keep", () => (el("hourly-wrap").hidden = true));
  click("hourly-add-cancel", closeHourlyAdd);
  el("hourly-add-view").addEventListener("submit", (e) => {
    e.preventDefault();
    saveHourlyAdd();
  });
  for (const id of ["hourly-add-day", "hourly-add-from", "hourly-add-to"]) {
    el(id).addEventListener("input", renderAddLength);
  }
  click("hourly-card", () => void invoke("mini_open", { href: "/hours" }));
  click("hourly-pick-back", closeHourlyPick);
  click("hourly-idle-resume", () => answerHourlyIdle("resume"));
  // Made right here when they can start projects; otherwise on the website.
  click("hourly-file-new", () => ((current?.hourly?.clients ?? []).length > 0 ? openHourlyNew("") : fileHourly("new")));
  click("hourly-file-later", () => fileHourly("later"));
  document.querySelectorAll<HTMLButtonElement>("#idle-choice .seg").forEach((seg) => {
    seg.addEventListener("click", () => menu(`idle-${seg.dataset.idle ?? "10"}`));
  });
  click("hourly-idle-stop", () => answerHourlyIdle("stop"));
  click("hourly-idle-keep", () => answerHourlyIdle("keep"));
  const filter = el<HTMLInputElement>("hourly-filter");
  filter.addEventListener("input", renderHourlyPick);
  filter.addEventListener("keydown", (e) => {
    // Enter starts the first match.
    if (e.key !== "Enter" || e.isComposing) return;
    e.preventDefault();
    el("hourly-pick-list").querySelector<HTMLButtonElement>(".h-row:not(.on)")?.click();
  });
  el("hourly-note-input").addEventListener("keydown", (e) => {
    // Enter starts the first one-tap project with this note.
    if (e.key !== "Enter" || e.isComposing) return;
    e.preventDefault();
    el("hourly-quick").querySelector<HTMLButtonElement>(".h-chip:not(.other)")?.click();
  });
  click("next-place", () => {
    const url = current?.next_call?.map_url;
    if (url) void invoke("mini_open_map", { url });
  });
  click("next-sheet", () => {
    const href = current?.next_call?.sheet_href;
    if (href) void invoke("mini_open", { href });
  });
  document.querySelectorAll<HTMLButtonElement>("#dnd-choice .seg").forEach((seg) => {
    seg.addEventListener("click", () => setDnd(seg.dataset.dnd ?? "off"));
  });
  click("mood-save", () => {
    if (!mood) return;
    if (mood.stage === "talk") { sendMood({ intent: "talk", yes: true }, "talk-yes"); return; }
    if (mood.score === null) return;
    const note = mood.ownWords ? el<HTMLTextAreaElement>("mood-note").value.trim() : "";
    sendMood({
      intent: "answer", moment: mood.moment, score: mood.score,
      keyword: mood.ownWords ? null : mood.word, cause: mood.cause, note: note || null,
    }, "answer");
  });
  click("mood-skip", () => {
    if (!mood) return;
    if (mood.stage === "talk") sendMood({ intent: "talk", yes: false }, "talk-no");
    else sendMood({ intent: "skip", moment: mood.moment }, "skip");
  });
  click("close-settings", () => showSettings(false));
  click("pin", () => {
    const pinned = el("pin").getAttribute("aria-pressed") !== "true";
    renderPin(pinned);
    void invoke("mini_pin", { pinned });
  });
  click("shrink", () => void invoke("mini_compact", { compact: true }));
  click("grow", () => void invoke("mini_compact", { compact: false }));
  click("c-pin", () => {
    const pinned = el("c-pin").getAttribute("aria-pressed") !== "true";
    renderPin(pinned);
    void invoke("mini_pin", { pinned });
  });
  click("c-bell", () => void invoke("mini_expand_notifications"));
  // The mini timer's walkie: grow to the full panel on the newest conversation.
  click("c-walkie", () => {
    const first = current?.chats[0];
    if (!first) return;
    invoke("mini_compact", { compact: false }).finally(() => openChat(first));
  });
  click("chat-back", () => closeChat());
  click("chat-full", () => void invoke("mini_open", { href: "/walkie" }));
  el("chat-form").addEventListener("submit", (e) => {
    e.preventDefault();
    sendReply();
  });
  const input = el<HTMLTextAreaElement>("chat-input");
  input.addEventListener("input", growInput);
  // Enter sends, Shift+Enter starts a new line (as in the tracker's walkie).
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      sendReply();
    }
  });
  click("c-action", () => {
    const action = el("c-action").dataset.action;
    if (action) tapClock(action, "mini_timer");
  });
  // Unpinned, the mini timer drags from anywhere but its buttons. Pinned,
  // it stays where it was pinned: unpin to move it.
  el("compact-view").addEventListener("mousedown", (e) => {
    if (e.button !== 0 || current?.pinned || (e.target as HTMLElement).closest("button")) return;
    void getCurrentWindow().startDragging();
  });
  // Unpinned, the header drags the window (not from its buttons).
  el("head").addEventListener("mousedown", (e) => {
    if (e.button !== 0 || el("head").classList.contains("pinned")) return;
    if ((e.target as HTMLElement).closest("button")) return;
    void getCurrentWindow().startDragging();
  });
  const search = el<HTMLInputElement>("search-input");
  search.addEventListener("input", onSearchInput);
  search.addEventListener("keydown", (e) => {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      markHit(activeHit + (e.key === "ArrowDown" ? 1 : -1));
    } else if (e.key === "Enter" && !e.isComposing) {
      e.preventDefault();
      openHit(activeHit);
    } else if (e.key === "Escape" && search.value !== "") {
      e.preventDefault();
      e.stopPropagation();
      clearSearch();
    }
  });
  click("search-clear", () => { clearSearch(); search.focus(); });
  // Type to search: a letter typed on the main view goes to the search field.
  window.addEventListener("keydown", (e) => {
    if (e.key.length !== 1 || e.metaKey || e.ctrlKey || e.altKey || e.isComposing) return;
    if (el("main-view").hidden || document.activeElement instanceof HTMLInputElement || document.activeElement instanceof HTMLTextAreaElement) return;
    search.focus();
  });
  window.addEventListener("keydown", (e) => {
    if (e.key !== "Escape") return;
    if (whatsNew) { closeWhatsNew(); return; }
    // The question about time away is only closed by an answer.
    if (hourlyIdle) return;
    if (hourlyAdd) { closeHourlyAdd(); return; }
    if (hourlyEdit) { closeHourlyEdit(); return; }
    if (hourlyPick && !el("hourly-new").hidden) { showHourlyNew(false); return; }
    if (hourlyPick) {
      // "Which project was it for?" is closed by an answer, or Later.
      if (hourlyPick === "file" || el<HTMLInputElement>("hourly-filter").value !== "") return;
      closeHourlyPick();
      return;
    }
    if (mood) { if (mood.sent === null) closeMood(); return; }
    if (chat) { closeChat(); return; }
    if (!el("settings").hidden) showSettings(false);
    else void invoke("mini_hide");
  });
  // Each opening starts on the main view. A check-in left by clicking away
  // isn't answered or skipped: like closing the web clock's panel.
  window.addEventListener("blur", () => {
    if (mood && mood.sent === null && !current?.pinned) closeMood();
    if (!current?.pinned) clearSearch();
    // Like the rest of the panel, the next opening starts on the main view;
    // an unsent reply is kept for when the conversation is opened again.
    if (chat && !current?.pinned) closeChat();
    if (hourlyPick === "start" && !current?.pinned) closeHourlyPick();
    showSettings(false);
  });
});
