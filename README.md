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

System tray (Show Tracker / Saved for offline / Quit; left-click focuses)
plus OS toasts on offline save/fail. Live message/approval toasts are
parked: the shell cannot see page state, and screen-scraping the remote DOM
is off the table without a web-side hook (out of scope for this repo).

## Release (free signing)

- Tag `desktop-vX.Y.Z` → GitHub matrix builds macOS (ad-hoc-signed `.dmg`),
  Windows (unsigned NSIS `.exe`) and Linux (GPG-signed `.AppImage` + `.deb`).
- No paid Apple/Microsoft accounts: first-run OS warnings are expected and
  documented on the download page (Mac: Right-click → Open; Windows:
  More info → Run anyway). Auto-updates are still cryptographically verified
  via the updater's own free minisign keypair.
- Bundle ID and updater public key are frozen from v1 so paid signing later
  is an upgrade, not a migration.

## Verified against

Web commit: `d30a6043` (tracker `ui/design-system-wip` branch, 2026-10-02).
One-line note per desktop release; not automation, not a sync.
