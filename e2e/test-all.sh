#!/usr/bin/env bash
# The whole test suite, one command. Runs the same steps locally (inside the
# subraum-test Docker image) and on a CI runner (`--native`, toolchains already
# present — this is what .github/workflows/test.yml and .gitlab-ci.yml call).
#
#   bash e2e/test-all.sh                # everything, inside Docker
#   bash e2e/test-all.sh --native       # everything, on this machine
#   ONLY=rust bash e2e/test-all.sh      # one stage: rust | tauri | frontend | streamdeck
#
# Stages
#   rust       cargo clippy -D warnings + cargo test for the workspace
#              (crates/protocol, server/init, apps/companion-core)
#   tauri      the same for apps/companion/src-tauri (out of the workspace,
#              needs webkit2gtk)
#   frontend   tsc + vite build, then vitest (apps/companion)
#   streamdeck node unit tests + the fake-deck harness (streamdeck-plugin)
#
# Docker mode bind-mounts the repo and keeps the cargo target in named volumes,
# so a second run is incremental. Node projects are copied into the container
# first: a node_modules installed on Linux must never land in the Windows
# checkout (platform-specific binaries), and vice versa.
set -euo pipefail
export MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*'

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ONLY="${ONLY:-all}"
IMG="${IMG:-subraum-test}"

want() { [[ "$ONLY" == all || "$ONLY" == "$1" ]]; }

# ── native mode: run the stages right here ───────────────────────────────────
if [[ "${1:-}" == "--native" ]]; then
  cd "$ROOT"
  fail=0
  stage() { echo; echo "──── $1 ────"; }

  if want rust; then
    stage "rust: clippy"
    cargo clippy --workspace --all-targets -- -D warnings || fail=1
    stage "rust: test"
    cargo test --workspace || fail=1
  fi

  if want tauri; then
    # tauri-build refuses to run when `frontendDist` (../dist) does not exist.
    # The shell's tests do not need the real bundle, so a placeholder is enough
    # on a runner that never ran `pnpm build`. dist/ is gitignored.
    if [[ ! -f apps/companion/dist/index.html ]]; then
      mkdir -p apps/companion/dist
      printf '<!doctype html><title>subraum test placeholder</title>\n' > apps/companion/dist/index.html
    fi
    stage "tauri: clippy"
    (cd apps/companion/src-tauri && cargo clippy --all-targets -- -D warnings) || fail=1
    stage "tauri: test"
    (cd apps/companion/src-tauri && cargo test) || fail=1
  fi

  if want frontend; then
    stage "frontend: install"
    (cd apps/companion && pnpm install --frozen-lockfile) || fail=1
    stage "frontend: tsc + vite build"
    (cd apps/companion && pnpm build) || fail=1
    stage "frontend: vitest"
    (cd apps/companion && pnpm test) || fail=1
  fi

  if want streamdeck; then
    stage "streamdeck: install"
    (cd streamdeck-plugin/org.raumdock.subraum.sdPlugin && npm ci --omit=dev) || fail=1
    stage "streamdeck: unit tests"
    (cd streamdeck-plugin && npm test) || fail=1
    stage "streamdeck: harness"
    (cd streamdeck-plugin && node test-harness.mjs) || fail=1
  fi

  echo
  if [[ $fail -ne 0 ]]; then echo "RESULT: FAILED"; exit 1; fi
  echo "RESULT: all stages passed"
  exit 0
fi

# ── docker mode: build the runner image once, then run --native inside it ────
DOCKER="${DOCKER:-docker}"
if ! command -v "$DOCKER" >/dev/null; then
  export PATH="$PATH:/c/Program Files/Docker/Docker/resources/bin"
  DOCKER="docker.exe"
fi

if ! "$DOCKER" image inspect "$IMG" >/dev/null 2>&1; then
  echo "building $IMG (toolchains + sysdeps, no source) …"
  "$DOCKER" build -t "$IMG" -f "$ROOT/e2e/Dockerfile.test" "$ROOT/e2e"
fi

# One container per stage. The repo is mounted read-only and copied to a
# writable /run-copy first: cargo, pnpm and npm all want to write next to the
# source (Cargo.lock, node_modules, dist), and a Linux node_modules must never
# land in the Windows checkout. The cargo target is ALWAYS mounted at /target
# (the image's CARGO_TARGET_DIR): build scripts cache absolute paths into
# their output, so the src-tauri volume must not reappear under another name.
fail=0
for s in rust tauri frontend streamdeck; do
  want "$s" || continue
  vol=subraum-target; [[ "$s" == tauri ]] && vol=subraum-target-tauri
  "$DOCKER" run --rm \
    -v "$ROOT:/work:ro" \
    -v "$vol:/target" \
    -v subraum-node-cache:/root/.cache \
    -e ONLY="$s" \
    -w /work "$IMG" bash -euo pipefail -c '
      mkdir -p /run-copy
      tar -C /work --exclude=node_modules --exclude=dist --exclude=target \
          --exclude=.git --exclude=test-build --exclude=screenshots --exclude=store-art \
          -cf - . | tar -C /run-copy -xf -
      cd /run-copy && bash e2e/test-all.sh --native
    ' || fail=1
done
exit $fail
