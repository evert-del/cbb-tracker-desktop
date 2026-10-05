import { invoke } from "@tauri-apps/api/core";

// Mini panel: all state is pushed from Rust via eval (window.__cbbMiniShow).
// Clicks go back through the narrow mini commands (see mini.json capability).
// The elapsed timer ticks locally every second from the poll's ISO moment.

type Need = { label: string; body: string; href: string };
type View = {
  status: string;
  dot: string;
  actionLabel: string;
  action: string | null;
  since_iso: string;
  unread: number;
  needs: Need[];
};

declare global {
  interface Window {
    __cbbMiniShow?: (view: View) => void;
    __cbbMiniSinceIso?: string;
  }
}

function el<T extends HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (!node) throw new Error(`missing #${id}`);
  return node as T;
}

function fmtElapsed(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  const mm = h > 0 ? String(m).padStart(2, "0") : String(m);
  const ss = String(sec).padStart(2, "0");
  return h > 0 ? `${h}:${mm}:${ss}` : `${mm}:${ss}`;
}

function tick() {
  const iso = window.__cbbMiniSinceIso;
  const out = el("mini-elapsed");
  if (!iso) {
    out.textContent = "";
    return;
  }
  const t = Date.parse(iso);
  if (Number.isNaN(t)) {
    out.textContent = "";
    return;
  }
  out.textContent = fmtElapsed(Date.now() - t);
}

window.__cbbMiniShow = (view) => {
  el("mini-status").textContent = view.status;
  el("mini-dot").style.background = view.dot;
  window.__cbbMiniSinceIso = view.since_iso || "";
  tick();

  const action = el<HTMLButtonElement>("mini-action");
  action.textContent = view.actionLabel;
  action.disabled = view.action === null;
  action.dataset.action = view.action ?? "";

  const list = el("mini-needs");
  list.innerHTML = "";
  if (view.needs.length === 0) {
    const empty = document.createElement("div");
    empty.id = "mini-empty";
    empty.textContent = "Nothing waiting on you.";
    list.appendChild(empty);
  }
  for (const need of view.needs.slice(0, 3)) {
    const row = document.createElement("button");
    row.className = "need";
    const pill = document.createElement("span");
    pill.className = "pill";
    pill.textContent = need.label;
    const txt = document.createElement("span");
    txt.className = "txt";
    txt.textContent = need.body;
    const go = document.createElement("span");
    go.className = "go";
    go.textContent = "›";
    row.append(pill, txt, go);
    row.addEventListener("click", () => {
      void invoke("mini_open", { href: need.href });
    });
    list.appendChild(row);
  }

  const more = el("mini-more");
  const rest = view.unread - Math.min(view.unread, view.needs.length);
  more.textContent = view.unread === 0 ? "You're all caught up." : rest > 0 ? `+${rest} more` : "";
};

window.addEventListener("DOMContentLoaded", () => {
  window.setInterval(tick, 1000);
  el<HTMLButtonElement>("mini-action").addEventListener("click", () => {
    const action = el<HTMLButtonElement>("mini-action").dataset.action;
    if (action) void invoke("mini_action", { action });
  });
  el<HTMLButtonElement>("mini-expand").addEventListener("click", () => {
    void invoke("mini_expand");
  });
  el<HTMLButtonElement>("mini-close").addEventListener("click", () => {
    void invoke("mini_hide");
  });

  // Manual drag: press anywhere that isn't a button and move. (Tauri's own
  // drag region stays as a backup.) Coordinates are CSS pixels; Rust scales
  // them to the physical window.
  let dragging = false;
  let queued: { x: number; y: number } | null = null;
  const card = el("mini-card");
  card.addEventListener("mousedown", (e) => {
    if (e.button !== 0) return;
    const t = e.target as HTMLElement | null;
    if (t && t.closest && t.closest("button")) return;
    dragging = true;
    void invoke("mini_drag_start", { x: e.screenX, y: e.screenY });
  });
  window.addEventListener("mousemove", (e) => {
    if (!dragging) return;
    queued = { x: e.screenX, y: e.screenY };
    window.requestAnimationFrame(() => {
      if (queued) {
        const at = queued;
        queued = null;
        void invoke("mini_drag_move", { x: at.x, y: at.y });
      }
    });
  });
  window.addEventListener("mouseup", () => {
    if (!dragging) return;
    dragging = false;
    void invoke("mini_drag_end");
  });
});
