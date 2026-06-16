#!/usr/bin/env bash
# Pull the newest published installer from GitHub Releases into the public
# download folder served by Caddy at squadlink.raumdock.org/download.
# Public repo → no auth/token needed. Run by a systemd timer on LXC 103.
set -euo pipefail

REPO="cccdemon/RDOC-SquadLinkLite"
DEST="/opt/RDOC-Suite/downloads/squadlink"
# NOTE: the REST /releases order (created_at) is unreliable for force-pushed tags,
# and /releases/latest excludes prereleases (ours are all prereleases). So fetch a
# page and pick the HIGHEST semver ourselves (sort -V).
API="https://api.github.com/repos/${REPO}/releases?per_page=30"
# Robust against transient GitHub/CDN 5xx right after a release is published.
RETRY="--retry 6 --retry-delay 4 --retry-all-errors"

mkdir -p "$DEST"

# All asset URLs. Retry because tauri-action creates the release a moment before
# it finishes uploading the installer (publish→attach race).
urls=""
for attempt in 1 2 3 4 5 6 7 8; do
  urls="$(curl -fsSL $RETRY -H 'Accept: application/vnd.github+json' "$API" \
    | grep -oE '"browser_download_url": *"[^"]+"' | cut -d'"' -f4 || true)"
  printf '%s\n' "$urls" | grep -qiE 'setup\.exe$' && break
  echo "no installer asset yet (attempt $attempt) — waiting"
  sleep 10
done
[ -n "$urls" ] || { echo "no releases yet"; exit 0; }

fetch() {
  # $1 = case-insensitive filename suffix to match, $2 = output filename.
  # sort -V → pick the highest version, not whatever the API lists first.
  local url
  url="$(printf '%s\n' "$urls" | grep -iE "$1" | sort -V | tail -1 || true)"
  [ -n "$url" ] || { echo "no asset for $1"; return 0; }
  curl -fsSL $RETRY -o "$DEST/$2.tmp" "$url"

  # Integrity: CI publishes a per-asset "<asset>.sha256" sidecar (just the hex
  # digest) alongside each installer. Verify before publishing so a tampered or
  # corrupted asset is never mirrored. Fail closed if the sidecar exists but the
  # hash mismatches; skip (with a warning) only for legacy releases that predate
  # the checksum step.
  local sig_url expected actual
  sig_url="$(printf '%s\n' "$urls" | grep -iF "${url}.sha256" | head -1 || true)"
  if [ -n "$sig_url" ]; then
    expected="$(curl -fsSL $RETRY "$sig_url" | tr -d '[:space:]' | tr 'A-F' 'a-f')"
    actual="$(sha256sum "$DEST/$2.tmp" | cut -d' ' -f1)"
    if [ "$expected" != "$actual" ]; then
      rm -f "$DEST/$2.tmp"
      echo "CHECKSUM MISMATCH for $2 (expected $expected, got $actual) — refusing to publish" >&2
      return 1
    fi
    echo "verified $2 sha256=$actual"
  else
    echo "WARNING: no .sha256 sidecar for $2 — publishing unverified" >&2
  fi

  mv "$DEST/$2.tmp" "$DEST/$2"
  echo "updated $2  <-  $url"
}

fetch 'setup\.exe$'        RDOC-SquadLink-Lite-Setup.exe
fetch '_x64_en-US\.msi$|\.msi$'  RDOC-SquadLink-Lite.msi
