import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

type OfflineItem = {
  id: string;
  label: string;
  source_url: string;
  saved_at_millis: number;
  bytes: number;
};

const statusEl = document.querySelector("#status") as HTMLElement;
const saveMsgEl = document.querySelector("#save-msg") as HTMLElement;
const listEl = document.querySelector("#offline-list") as HTMLElement;
const urlInput = document.querySelector("#save-url") as HTMLInputElement;
const labelInput = document.querySelector("#save-label") as HTMLInputElement;

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

function formatDate(millis: number): string {
  return new Date(millis).toLocaleString();
}

async function refresh() {
  try {
    const [items, info] = await Promise.all([
      invoke<OfflineItem[]>("offline_list"),
      invoke<{ count: number; bytes: number; capBytes: number }>(
        "offline_storage_info",
      ),
    ]);
    const online = navigator.onLine ? "online" : "offline";
    statusEl.textContent =
      `${info.count} saved · ${formatBytes(info.bytes)} of ${formatBytes(info.capBytes)} · ${online}` +
      (online === "online"
        ? ""
        : " — showing saved copies only");
    listEl.innerHTML = "";
    for (const item of items) {
      const li = document.createElement("li");
      const title = document.createElement("strong");
      title.textContent = item.label || "(no label)";
      li.appendChild(title);
      li.appendChild(
        document.createTextNode(
          ` · ${formatBytes(item.bytes)} · saved ${formatDate(item.saved_at_millis)} `,
        ),
      );
      const open = document.createElement("button");
      open.textContent = "Open";
      open.addEventListener("click", async () => {
        try {
          await invoke("offline_open", { id: item.id });
        } catch (e) {
          saveMsgEl.textContent = `Could not open: ${e}`;
          await refresh();
        }
      });
      const del = document.createElement("button");
      del.textContent = "Delete";
      del.addEventListener("click", async () => {
        await invoke("offline_delete", { id: item.id });
        await refresh();
      });
      li.appendChild(open);
      li.appendChild(del);
      listEl.appendChild(li);
    }
    if (items.length === 0) {
      const li = document.createElement("li");
      li.textContent =
        "Nothing saved yet. Open a call sheet in the tracker, copy its Download link, and save it here before heading to set.";
      listEl.appendChild(li);
    }
  } catch (e) {
    statusEl.textContent = `Could not load saved copies: ${e}`;
  }
}

window.addEventListener("DOMContentLoaded", () => {
  document.querySelector("#save-form")?.addEventListener("submit", async (e) => {
    e.preventDefault();
    saveMsgEl.textContent = "Saving…";
    try {
      await invoke("offline_save_pdf", {
        url: urlInput.value.trim(),
        label: labelInput.value.trim(),
      });
      saveMsgEl.textContent =
        "Downloading… this window updates when it lands.";
      urlInput.value = "";
      labelInput.value = "";
    } catch (err) {
      saveMsgEl.textContent = `Could not save: ${err}`;
    }
  });

  listen("offline-saved", async () => {
    saveMsgEl.textContent = "Saved.";
    await refresh();
  });
  listen("offline-failed", async (event) => {
    const reason = (event.payload as { reason?: string })?.reason ?? "unknown";
    saveMsgEl.textContent = `Save failed: ${reason}`;
    await refresh();
  });
  window.addEventListener("online", refresh);
  window.addEventListener("offline", refresh);
  void refresh();
});
