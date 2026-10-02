#!/usr/bin/env python3
"""Point an updater manifest at our own website instead of GitHub.

tauri-action writes latest.json to the GitHub release with download URLs on
github.com (private repo -> not reachable by installed apps). This rewrites
every platform URL to <base_url>/<file name>, so uploading the release files
next to latest.json on the website is all that is needed.

    python3 scripts/selfhost-latest-json.py latest.json \
        https://coolerboxbrothers.com/downloads/tracker/ > site/latest.json
"""
import json
import sys
from urllib.parse import unquote, urlparse

if len(sys.argv) != 3:
    sys.exit(__doc__)

manifest = json.load(open(sys.argv[1]))
base = sys.argv[2].rstrip("/") + "/"
for platform in manifest["platforms"].values():
    name = unquote(urlparse(platform["url"]).path.rsplit("/", 1)[-1])
    platform["url"] = base + name
json.dump(manifest, sys.stdout, indent=2)
print()
