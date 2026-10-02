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
- **Online-only by default, offline as explicit opt-in (v1 plan):** only
  call-sheet PDFs and task/schedule snapshots the user explicitly saves are
  available offline, labelled with their saved-at time, read-only.
  Revalidated on reconnect; stale saves are marked "re-save".

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

The main window URL, bundle identifier (`com.coolerbox.tracker`) and
deep-link scheme (`tracker://`) live in `src-tauri/tauri.conf.json`.
Capabilities (least-privilege plugin scopes) live in
`src-tauri/capabilities/`.

Auth, OAuth (Google/Xero/Frame.io), Paystack and email token links
(`/view|/quote|/invoice|/invite`, `/cb/*`) open in the **system browser**
and finish via `tracker://auth/callback` — Google blocks embedded WebViews.

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
