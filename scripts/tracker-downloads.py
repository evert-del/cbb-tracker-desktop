#!/usr/bin/env python3
"""Prepare a published release for the tracker's download page.

The tracker serves the desktop app from its R2 bucket, under
platform/desktop/ (tracker repo: src/lib/desktop-app.ts). Given the release's
assets in one folder, this writes into <out>:

  current.json  the manifest the tracker reads: version, date, the installer
                for each computer, and the updater's own files
  latest.json   the updater file, its URLs pointed at
                https://tracker.coolerboxbrothers.com/download/desktop/v/<version>/<file>
  uploads.txt   the files to upload to platform/desktop/<version>/, one per line

    python3 scripts/tracker-downloads.py dist 0.2.3 out
"""
import json
import os
import sys
from datetime import datetime, timezone
from urllib.parse import unquote, urlparse

BASE = "https://tracker.coolerboxbrothers.com/download/desktop"

# Which asset is the installer for each computer, by how its name ends.
INSTALLERS = {
    "windows": "_x64-setup.exe",
    "mac": "_universal.dmg",
    "appimage": "_amd64.AppImage",
    "deb": "_amd64.deb",
}


def main(dist, version, out):
    names = sorted(os.listdir(dist))
    files = {}
    for kind, ending in INSTALLERS.items():
        found = [n for n in names if n.endswith(ending) and f"_{version}_" in n]
        if len(found) != 1:
            sys.exit(f"Expected one {kind} installer ending {ending} for {version}, found {found}")
        files[kind] = {"name": found[0], "size": os.path.getsize(os.path.join(dist, found[0]))}

    latest = json.load(open(os.path.join(dist, "latest.json")))
    if latest.get("version", "").lstrip("v") != version:
        sys.exit(f"latest.json is for {latest.get('version')}, not {version}")
    updater = []
    for platform in latest["platforms"].values():
        name = unquote(urlparse(platform["url"]).path.rsplit("/", 1)[-1])
        if name not in names:
            sys.exit(f"latest.json names {name}, which is not among the release's files")
        platform["url"] = f"{BASE}/v/{version}/{name}"
        if name not in updater:
            updater.append(name)

    os.makedirs(out, exist_ok=True)
    manifest = {
        "version": version,
        "publishedAt": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "files": files,
        "updater": updater,
    }
    with open(os.path.join(out, "current.json"), "w") as f:
        json.dump(manifest, f, indent=2)
    with open(os.path.join(out, "latest.json"), "w") as f:
        json.dump(latest, f, indent=2)
    uploads = sorted({f["name"] for f in files.values()} | set(updater))
    with open(os.path.join(out, "uploads.txt"), "w") as f:
        f.write("\n".join(uploads) + "\n")
    print(json.dumps(manifest, indent=2))


if __name__ == "__main__":
    if len(sys.argv) != 4:
        sys.exit(__doc__)
    main(*sys.argv[1:])
