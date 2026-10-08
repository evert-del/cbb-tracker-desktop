# CoolerBox Tracker — Desktop Shell (Tauri v2)

Thin desktop wrapper around the hosted CoolerBox Production Tracker
(`https://tracker.coolerboxbrothers.com`) for **Windows, macOS and Linux**.
Built with [Tauri v2](https://v2.tauri.app/start/) — a small Rust shell whose
main window loads the production web app as its URL.

## Hard boundaries (do not cross)

- **This repo never touches the web app.** No code is shared, vendored or
  submodule-linked from `CoolerBox-Production-Tracker`. Website deploys land
  here automatically on next load — no desktop rebuild needed.
- **No database work.** No migrations, no RPC, no RLS, no Supabase keys.
  The shell talks to the already-deployed backend over HTTPS/WSS exactly
  like a browser. `SUPABASE_SERVICE_ROLE_KEY`, R2/GCS credentials and mail
  secrets must never appear in this repo.
- **Online-only by default, offline as explicit opt-in:** the "Saved for
  offline" library window (`tracker://offline`) keeps only PDFs the user
  explicitly saves — paste a call-sheet/quota/invoice Download link, and the
  shell downloads it through the main window's own session into app-data
  (25 MB per file, 500 MB total). Saved copies are read-only snapshots with
  a saved-at time, opened in the system PDF viewer. Re-save to refresh;
  nothing syncs back. No service worker, no local database.

## Prerequisites (per https://v2.tauri.app/start/prerequisites/)

- macOS: Xcode (or `xcode-select --install` for desktop-only) + `rustup`
- Windows: Microsoft C++ Build Tools + Evergreen WebView2 + `rustup` (MSVC)
- Linux: `libwebkit2gtk-4.1-dev build-essential curl wget file libxdo-dev
  libssl-dev libayatana-appindicator3-dev librsvg2-dev` + `rustup`
- Node.js LTS for the Tauri CLI

## Develop

```bash
npm install
npm run tauri dev   # opens the shell pointed at production
```

The main window URL (`APP_ORIGIN` in `src-tauri/src/lib.rs`), bundle
identifier (`com.coolerbox.tracker`) and deep-link scheme (`tracker://`)
live in `src-tauri/`. Capabilities (least-privilege plugin scopes) live in
`src-tauri/capabilities/`.

URL routing (`src-tauri/src/lib.rs`, unit-tested): the tracker host,
Supabase Auth and the OAuth/billing sign-in pages stay in the WebView so the
session lands in the app's own cookie jar; anything else opens in the
**system browser**. Inbound `tracker://` links (`auth`, `view`, `quote`,
`invoice`, `invite`, `cb`, `r`, `offline`) load the matching page or window.
Google may refuse embedded WebViews — password/magic-link in the same window
is the v1 fallback.

Time sheet (`src-tauri/src/idle.rs`, unit-tested): the shell reads only how
long the computer has had no keyboard or mouse input (Windows
GetLastInputInfo, macOS CGEventSource, GNOME's idle monitor over D-Bus on
Linux, else KDE Plasma's org.freedesktop.ScreenSaver; a sleep counts too). When someone is back after 5 minutes or more it
evals a `cbb:away` event ({from, to} in ms) into the tracker page, which asks
them keep / break / wrapped only if they are called in. Nothing else is read,
stored or sent. `window.cbbDesktopApp.idleSupported` says whether input idle
can be read here.

System tray: the time sheet's clock first (`src-tauri/src/clock.rs`, unit-
tested): "Time sheet: Called in since 08:02", then Call in / Break / Back from
break / Wrap, enabled as they make sense. It asks the page to fetch and post
the tracker's own `/api/time-sheet/clock` with its session (no IPC, like the
notifications), and tells the page (`cbb:clock-changed`) so its clock catches
up. Calling in before the notice is read opens the tracker's clock instead.
Then Show Tracker / Saved for offline / Quit; left-click focuses
plus OS toasts on offline save/fail. Live message/approval toasts are
parked: the shell cannot see page state, and screen-scraping the remote DOM
is off the table without a web-side hook (out of scope for this repo).

Usage of the app's own features (`src-tauri/src/analytics.rs`): opening the
quick panel, clock taps from the panel / mini timer / tray / shortcut, the
panel and tray shortcuts, pin / mini-timer mode and settings changes. The shell
has no analytics key and sends nothing itself: it hands each event (fixed
labels only, never content) to the tracker page as `window.cbbDesktopEvents` +
a `cbb:desktop-event`, and the page sends it with its own `track()`, under the
same consent rules, only when someone is signed in.

Linux AppImage: on every start the app keeps its own launcher in
`~/.local/share/applications/cbb-tracker-desktop.desktop` (named after the
window's id, so the dock shows the right icon), with its icons, pointing at
wherever the AppImage now is, and makes it the `tracker://` handler
(`src-tauri/src/desktop_entry.rs`). A launcher of that name it didn't write is
left alone. Keep the AppImage somewhere the user can write (e.g.
`~/Applications`), not `/opt`, or it cannot update itself.

## Release (free signing)

- Tag `desktop-vX.Y.Z` → GitHub matrix builds macOS (universal, ad-hoc-signed
  `.dmg`), Windows (unsigned NSIS `.exe` + `.msi`) and Linux (`.AppImage` +
  `.deb`) into a **draft** release. The repo is private, so only collaborators
  can see it; download the files from there.
- No paid Apple/Microsoft accounts: first-run OS warnings are expected and
  documented on the download page (Mac: Right-click → Open; Windows:
  More info → Run anyway).
- Bundle ID and updater public key are frozen from v1 so paid signing later
  is an upgrade, not a migration.

## Releasing an update (auto-update)

The website itself needs no update — it loads fresh every time. Only changes
to *this* repo (window, tray, notifications, downloads, routing) need a
shell release. Installed apps check 30 s after launch and every 6 h, ask the
user, then verify the bundle's signature against the public key in
`src-tauri/tauri.conf.json` before installing.

1. Bump `version` in `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml` and
   `package.json` (must be higher than what is installed).
2. `git tag desktop-vX.Y.Z && git push origin desktop-vX.Y.Z`; wait for the
   build, open the draft release.
3. Check the draft, then **Publish** it. `publish-downloads.yml` then copies
   the installers and update files into the tracker's R2 bucket
   (`platform/desktop/<version>/`), writes `latest.json` (its URLs rewritten
   by `scripts/tracker-downloads.py` to
   `https://tracker.coolerboxbrothers.com/download/desktop/v/<version>/…`)
   and `current.json` **last**, so nobody is offered a file that is not
   there yet. The tracker's download page
   (https://tracker.coolerboxbrothers.com/download and "Get the desktop app"
   in its sidebar) shows the new version straight away. It uploads through
   R2's S3 API with an R2 Account API token limited to `cbb-tracker-media`
   (Object Read & Write): repo secrets `R2_ACCESS_KEY_ID`,
   `R2_SECRET_ACCESS_KEY` and `CLOUDFLARE_ACCOUNT_ID`. Re-run it by hand
   with the tag if it fails.

Installed apps check the tracker's `latest.json` first, then the old
`coolerboxbrothers.com/downloads/tracker/` address (0.2.2 and earlier only
know that one, so they never update by themselves; install 0.2.3 by hand).
`scripts/selfhost-latest-json.py` is kept for that old address.

The endpoint URL is baked into every installed app — changing it later strands
installed copies on the old URL. Confirm it before the first real rollout.
Linux `.deb` installs do not self-update (AppImage does, if it sits where the user can write).

Signing key: private key + password are repo secrets
(`TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`). If the
key is lost, installed apps can never be updated again — keep an offline copy
in a password manager.

## Verified against

Web commit: `fc63a3c0` (tracker `main`, 2026-10-07; includes `/desktop/signed-in`).
One-line note per desktop release; not automation, not a sync.
