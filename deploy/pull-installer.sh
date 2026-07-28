#!/usr/bin/env bash
# Mirror the newest published release (all platforms) from GitHub Releases into
# the public download folder served by Caddy at subraum.cc/download,
# and emit a manifest.json the signaling server renders on /get.
# Public repo → no auth/token needed. Run by a systemd timer on LXC 103.
set -euo pipefail

REPO="cccdemon/RDOC-SquadLinkLite"
DEST="/opt/RDOC-Suite/downloads/subraum"
API="https://api.github.com/repos/${REPO}"
# Robust against transient GitHub/CDN 5xx right after a release is published.
RETRY="--retry 6 --retry-delay 4 --retry-all-errors"
CURL="curl -fsSL $RETRY -H Accept:application/vnd.github+json"

mkdir -p "$DEST"

# 1) Newest release by SEMVER. The REST order (created_at) is unreliable for
#    force-pushed tags and /latest excludes prereleases (ours are all
#    prereleases), so list a page and pick the highest version ourselves.
tags="$($CURL "${API}/releases?per_page=30" \
  | grep -oE '"tag_name": *"[^"]+"' | cut -d'"' -f4 || true)"
TAG="$(printf '%s\n' "$tags" | sort -V | tail -1 || true)"
[ -n "$TAG" ] || { echo "no releases yet"; exit 0; }
echo "newest release: $TAG"

# 2) All asset URLs for THAT release only (no cross-release version mixing).
#    Retry: tauri-action creates the release a moment before the upload finishes.
urls=""
for attempt in 1 2 3 4 5 6 7 8; do
  urls="$($CURL "${API}/releases/tags/${TAG}" \
    | grep -oE '"browser_download_url": *"[^"]+"' | cut -d'"' -f4 || true)"
  printf '%s\n' "$urls" | grep -qiE '\.(exe|msi|deb|rpm|AppImage|apk)$' && break
  echo "no installer asset yet (attempt $attempt) — waiting"
  sleep 10
done
[ -n "$urls" ] || { echo "no assets on $TAG"; exit 0; }

# 3) Stage in a temp dir; only swap into DEST if everything verifies. A tampered
#    asset aborts the whole run → the public folder is never half-updated.
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

# Pull every SHA256SUMS-*.txt published with the release → combined lookup table
# of "<hex>  <filename>". CI attaches these; older releases may lack them.
sums="$STAGE/.sums"
: > "$sums"
for su in $(printf '%s\n' "$urls" | grep -iE '/SHA256SUMS[^/]*\.txt$' || true); do
  $CURL "$su" >> "$sums" 2>/dev/null || true
done

# version digits for the manifest, e.g. subraum-v0.1.25 → 0.1.25
VERSION="$(printf '%s' "$TAG" | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1 || true)"

# (platform, arch) for an asset filename. Echoes "platform arch".
classify() {
  local f; f="$(printf '%s' "$1" | tr 'A-Z' 'a-z')"
  case "$f" in
    *setup.exe|*.exe) echo "windows x64" ;;
    *.msi)            echo "windows x64" ;;
    *arm64*.deb|*aarch64*.deb)         echo "linux arm64" ;;
    *.deb)                             echo "linux amd64" ;;
    *aarch64*.rpm|*arm64*.rpm)         echo "linux arm64" ;;
    *.rpm)                             echo "linux amd64" ;;
    *aarch64*.appimage|*arm64*.appimage) echo "linux arm64" ;;
    *.appimage)                        echo "linux amd64" ;;
    *aarch64*.apk|*arm64*.apk)  echo "android arm64" ;;
    *armv7*.apk|*armeabi*.apk)  echo "android armv7" ;;
    *x86_64*.apk|*x86*.apk)     echo "android x86_64" ;;
    *.apk)                      echo "android universal" ;;
    *) echo "other -" ;;
  esac
}

entries=""  # accumulated manifest JSON objects

process_asset() {
  local url fname expected actual size pa platform arch
  url="$1"; fname="$(basename "$url")"
  $CURL -o "$STAGE/$fname" "$url"

  # Verify against the release's SHA256SUMS when available. Match by HASH
  # presence, not filename: GitHub may serve assets under a renamed form (it
  # rewrites characters it dislikes), so filenames don't always line up — the
  # content (hash) does. SHA-256 is collision-resistant, so a hash hit is a
  # sound verification.
  # Fail closed when sums exist but the hash is absent (tamper); with no
  # published sums (legacy releases) compute + warn.
  actual="$(sha256sum "$STAGE/$fname" | cut -d' ' -f1)"
  if [ -s "$sums" ]; then
    if grep -qi -- "$actual" "$sums"; then
      echo "verified $fname sha256=$actual"
    else
      echo "CHECKSUM MISMATCH for $fname (sha256 $actual not in published SHA256SUMS) — aborting, DEST untouched" >&2
      exit 1
    fi
  else
    echo "WARNING: no SHA256SUMS published for this release — mirroring with locally computed hash" >&2
  fi

  printf '%s' "$actual" > "$STAGE/$fname.sha256"
  size="$(stat -c%s "$STAGE/$fname")"
  pa="$(classify "$fname")"; platform="${pa% *}"; arch="${pa#* }"
  entries="${entries:+$entries,}$(printf '{"platform":"%s","arch":"%s","file":"%s","size":%s,"sha256":"%s"}' \
    "$platform" "$arch" "$fname" "$size" "$actual")"
}

# Mirror every installable artifact (skip the SHA256SUMS txt + signatures).
for url in $(printf '%s\n' "$urls" | grep -iE '\.(exe|msi|deb|rpm|AppImage|apk)$'); do
  process_asset "$url"
done
[ -n "$entries" ] || { echo "no installable assets on $TAG"; exit 0; }

# manifest.json the server reads to render /get.
printf '{"version":"%s","tag":"%s","generated":"%s","artifacts":[%s]}\n' \
  "$VERSION" "$TAG" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$entries" > "$STAGE/manifest.json"

# The logo and social card are compiled into the server binary now
# (/assets/logo.svg, /assets/og-image.png) and need no rescuing. The app
# screenshots cannot be: they are published by hand and the home page links
# them out of this directory, so carry them across the swap or the next release
# silently empties the gallery.
for shot in "$DEST"/shot-*.png; do
  [ -e "$shot" ] && cp -p "$shot" "$STAGE/"
done
rm -f "$STAGE/.sums"

# 4) Atomic-ish publish: replace DEST contents with the verified staging set.
if command -v rsync >/dev/null 2>&1; then
  rsync -a --delete "$STAGE"/ "$DEST"/
else
  find "$DEST" -mindepth 1 -delete
  cp -a "$STAGE"/. "$DEST"/
fi
echo "published $(printf '%s' "$entries" | grep -o '"file"' | wc -l) artifact(s) from $TAG to $DEST"
