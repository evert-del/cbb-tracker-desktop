import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";

// Quick panel: all state is pushed from Rust via eval (window.__cbbMiniShow).
// Clicks go back through the narrow mini commands (see mini.json capability).
// The timer and today's total tick locally every second between pushes.

type Need = { label: string; body: string; href: string };
type ClockAction = { id: string; label: string };
type LabelCount = { label: string; count: number };
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

/** Badge colours for a notification label, the same for every row of it. */
const BADGES: Array<[string, string]> = [
  ["#fbe4ee", "#8f1d55"],
  ["#e9e7fd", "#3c3489"],
  ["#dff4ec", "#0f5c47"],
  ["#fdf0d9", "#7a4a06"],
  ["#e3eefb", "#0c447c"],
  ["#fbe9e3", "#7a2e14"],
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
  const timer = !onClock ? "0:00" : Number.isNaN(since) ? "—" : fmtTimer(Date.now() - since);
  el("timer").textContent = timer;
  el("c-timer").textContent = timer;
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
    chip.className = "chip";
    const [bg, fg] = badgeFor(kind.label);
    chip.style.background = bg;
    chip.style.color = fg;
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
  const compact = view.compact && view.pinned;
  el("compact-view").hidden = !compact;
  if (compact) {
    el("main-view").hidden = true;
    el("settings").hidden = true;
  } else if (el("settings").hidden && !mood && !chat) {
    el("main-view").hidden = false;
  }
  el("shrink").hidden = !view.pinned;
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
  renderActions(view);
  renderNeeds(view);
  renderSummary(view);
  renderChats(view);
  renderCompactAction(view);
  restClockButtons();
  renderMode(view);
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

function openMood(moment: "IN" | "WRAP") {
  mood = { moment, stage: "ask", score: null, word: null, ownWords: false, cause: null, sent: null };
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
    __cbbMiniMood?: (ask: { moment: "IN" | "WRAP" }) => void;
    __cbbMiniMoodResult?: (r: { ok: boolean; offerTalk?: boolean; told?: string[]; error?: string }) => void;
  }
}
window.__cbbMiniMood = (ask) => openMood(ask.moment);
window.__cbbMiniMoodResult = (r) => moodResult(r);

// ── Walkie quick chat (walkie.rs). What people say is only drawn here: never
// stored, logged or counted. Opening a conversation marks it read, so Back
// without replying clears it too.
type Line = { id: string; body: string; author: string; mine: boolean; createdAt: string; call: boolean; file: boolean };
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
      else {
        bubble.classList.add("file");
        bubble.textContent = "Sent a file";
      }
      bubble.title = new Date(line.createdAt).toLocaleString();
      row.appendChild(bubble);
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
  }
}
window.__cbbMiniWalkieResult = (r) => chatResult(r);

function showSettings(open: boolean) {
  if (!el("compact-view").hidden || mood || chat) return;
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
  click("update-row", () => menu("update"));
  click("diagnostics-row", () => menu("diagnostics"));
  click("autostart-row", () => menu("autostart"));
  click("close-keep", () => menu("close-keep"));
  click("close-quit", () => menu("close-quit"));
  click("quit", () => menu("quit"));
  click("quit-row", () => menu("quit"));
  click("open-settings", () => showSettings(true));
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
  click("c-unpin", () => void invoke("mini_pin", { pinned: false }));
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
  // The mini timer drags from anywhere but its buttons.
  el("compact-view").addEventListener("mousedown", (e) => {
    if (e.button !== 0 || (e.target as HTMLElement).closest("button")) return;
    void getCurrentWindow().startDragging();
  });
  // Pinned, the header drags the window (not from its buttons).
  el("head").addEventListener("mousedown", (e) => {
    if (e.button !== 0 || !el("head").classList.contains("pinned")) return;
    if ((e.target as HTMLElement).closest("button")) return;
    void getCurrentWindow().startDragging();
  });
  window.addEventListener("keydown", (e) => {
    if (e.key !== "Escape") return;
    if (mood) { if (mood.sent === null) closeMood(); return; }
    if (chat) { closeChat(); return; }
    if (!el("settings").hidden) showSettings(false);
    else void invoke("mini_hide");
  });
  // Each opening starts on the main view. A check-in left by clicking away
  // isn't answered or skipped: like closing the web clock's panel.
  window.addEventListener("blur", () => {
    if (mood && mood.sent === null && !current?.pinned) closeMood();
    // Like the rest of the panel, the next opening starts on the main view;
    // an unsent reply is kept for when the conversation is opened again.
    if (chat && !current?.pinned) closeChat();
    showSettings(false);
  });
});
