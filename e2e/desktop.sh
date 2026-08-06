#!/usr/bin/env bash
# Linux DESKTOP client smoke test: boots the real Tauri app (webkit2gtk, GTK)
# under Xvfb in a container and probes its local control API from inside.
# Proves the .deb-equivalent binary starts on Linux, the webview mounts, and
# the Stream-Deck-facing control surface comes up — no display, no audio
# hardware, no interaction required.
#
#   bash e2e/desktop.sh          # from the repo root
set -euo pipefail
export MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*'

DOCKER="${DOCKER:-docker}"
if ! command -v "$DOCKER" >/dev/null; then
  export PATH="$PATH:/c/Program Files/Docker/Docker/resources/bin"
  DOCKER="docker.exe"
fi

cleanup() { "$DOCKER" rm -f sb-desktop >/dev/null 2>&1 || true; }
trap cleanup EXIT
cleanup

echo "== build (desktop stage) =="
"$DOCKER" build -q --target desktop -t subraum-e2e-desktop -f e2e/Dockerfile . >/dev/null

echo "== boot the GUI under Xvfb =="
# webkit wants decent /dev/shm; the default 64M starves it.
"$DOCKER" run -d --name sb-desktop --shm-size=512m subraum-e2e-desktop >/dev/null

echo "== wait for the control API discovery file =="
for i in $(seq 1 45); do
  if "$DOCKER" exec sb-desktop test -f /root/.config/org.raumdock.subraum/control.json 2>/dev/null; then
    break
  fi
  if [ "$i" = 45 ]; then
    echo "FAIL: control.json never appeared — app did not boot. Log:"
    "$DOCKER" logs --tail 30 sb-desktop 2>&1
    exit 1
  fi
  sleep 2
done

echo "== probe: auth + webview state =="
if ! "$DOCKER" exec sb-desktop python3 /usr/local/bin/probe-control.py; then
  echo "FAIL — app log:"
  "$DOCKER" logs --tail 30 sb-desktop 2>&1
  exit 1
fi

echo "== clean shutdown =="
"$DOCKER" stop -t 10 sb-desktop >/dev/null
echo "PASS: Linux desktop client boots, webview mounts, control API healthy"
