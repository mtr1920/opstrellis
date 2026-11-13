#!/usr/bin/env python3
"""Writes latest.json, the manifest the in-app updater reads from the latest GitHub release.

Usage: latest_json.py <tag> <dir>, where <dir> holds the release assets and their Tauri `.sig` files.
Refuses (exit 1) a missing asset or a signature that isn't Tauri's for this version.
       latest_json.py --check <version> <asset>... only checks each asset and its `.sig` (every build).
"""
import base64
import binascii
from datetime import datetime, timezone
import json
from pathlib import Path
import re
import sys

REPO = 'mtr1920/opstrellis'
# The updater's platform key, and the release asset that platform installs.
ASSETS = {
    'linux-x86_64': 'opstrellis-linux-x86_64.AppImage',
    'darwin-aarch64': 'opstrellis-macos-aarch64.app.tar.gz',
}


def fail(message):
    raise SystemExit('latest.json: ' + message)


def signature(sig, version):
    """The .sig file's contents, after checking it is a minisign box signed for `version`."""
    if not sig.is_file():
        fail(sig.name + ' is missing')
    text = sig.read_text().strip()
    try:
        lines = base64.b64decode(text, validate=True).decode().splitlines()
    except (binascii.Error, UnicodeDecodeError):
        fail(sig.name + " isn't a Tauri signature")
    if len(lines) != 4 or not lines[0].startswith('untrusted comment:') or not lines[2].startswith('trusted comment: '):
        fail(sig.name + " isn't a Tauri signature")
    # The updater rejects a package signed for another version than the manifest announces.
    if 'version:' + version not in lines[2][len('trusted comment: '):].split('\t'):
        fail(sig.name + ' was not signed for version ' + version)
    return text


def signed_asset(asset, version):
    """The signature of `asset`, which must exist and be signed for `version`."""
    if not asset.is_file() or asset.stat().st_size == 0:
        fail(asset.name + ' is missing')
    return signature(asset.with_name(asset.name + '.sig'), version)


def manifest(tag, folder, now):
    if not re.fullmatch(r'v\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?', tag):
        fail('unexpected tag ' + repr(tag))
    version = tag[1:]
    platforms = {}
    for key, asset in ASSETS.items():
        platforms[key] = {
            'signature': signed_asset(folder / asset, version),
            'url': f'https://github.com/{REPO}/releases/download/{tag}/{asset}',
        }
    return {'version': version, 'pub_date': now.astimezone(timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ'), 'platforms': platforms}


def main(argv):
    if len(argv) >= 3 and argv[0] == '--check':
        for asset in argv[2:]:
            signed_asset(Path(asset), argv[1])
        return
    if len(argv) != 2:
        fail('usage: latest_json.py <tag> <dir>')
    folder = Path(argv[1])
    data = manifest(argv[0], folder, datetime.now(timezone.utc))
    (folder / 'latest.json').write_text(json.dumps(data, indent=2) + '\n')


if __name__ == '__main__':
    main(sys.argv[1:])
