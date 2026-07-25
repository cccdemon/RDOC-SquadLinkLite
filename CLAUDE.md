# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

# RDOC SquadLink Lite — project notes

Serverless P2P voice mesh for small squads (warn-cap 12, hard-cap 16). No SFU, no
media server, no accounts, no recording. Audio/chat go **directly peer-to-peer**
(WebRTC, Opus, DTLS-SRTP/SCTP); the only central service is **InitConnection**,
which does signaling only (SDP/ICE relay, roster, session PIN) and never sees media.

Full design rationale: `docs/ARCHITECTURE.md`. User-facing docs: `README.md`.

## Commands

Frontend + Tauri app (from `apps/companion`, pnpm — **not** npm):

```sh
pnpm install --frozen-lockfile   # CI uses this; matches the lockfile
pnpm tauri dev                   # run the desktop app (needs Rust + Node)
pnpm build                       # tsc typecheck + vite build (frontend only)
pnpm tauri icon src/Squad_Link_Lite.png   # regenerate app icons
```

Rust workspace (from repo root — members: `crates/protocol`, `server/init`,
`apps/companion-core`):

```sh
cargo test                       # all workspace tests
cargo test -p companion-core     # one crate
cargo test -p companion-core crypto::   # one module's tests
cargo test -p protocol -- --nocapture   # single test: append its name
cargo build --release            # headless engine binary
```

`apps/companion/src-tauri` is **excluded from the workspace** — build/test it from
its own directory (`cd apps/companion/src-tauri && cargo check`). `spikes/*` are
standalone throwaway crates, also excluded.

Tests live in `#[cfg(test)]` modules inside `mesh.rs`, `crypto.rs`, `signaling.rs`
(companion-core) and `crates/protocol/src/lib.rs`. There is no separate test dir.

## Build hosts

### Linux / Android builds — Proxmox LXC 103
Local Windows `cargo` is unreliable here (aws-lc-sys C compile fails) — verify Rust
changes on LXC 103. Ubuntu 24.04.4 LTS, x86_64 (container hostname `streamer`).
Reached through the Proxmox host `ve.raumdock.org` with the `claude_deploy` key,
then `pct exec`:

```sh
ssh -i ~/.ssh/claude_deploy root@ve.raumdock.org 'pct exec 103 -- bash -lc "<cmd>"'
```

- `pct exec 103` runs as **root** in the container (can `apt install`).
- Has internet; node 18 / npm / pkg-config / git preinstalled.
- Missing on first use: `rustc`/`cargo`, `pnpm`, Tauri sysdeps
  (`webkit2gtk-4.1`, GTK3, ALSA `libasound2-dev`, `libopus-dev`+`clang` for
  `audiopus_sys` — else "Failed to autogen Opus").
- Verified Linux x86_64 build: `cargo build --release` → 27 MB
  `target/release/rdoc-squadlink-lite` in ~1m10s (2026-06-17).

### Windows builds — GitHub Actions
`.github/workflows/build-companion.yml` (runner `windows-latest`) builds the Tauri
app, packs an unsigned MSIX for the Store, and on a `squadlink-lite-v*` tag cuts a
prerelease + tells the server to pull the installer. Linux and Android have their
own workflows (`build-companion-linux.yml`, `build-companion-android.yml`).

`ve.raumdock.org` is also the deploy box: CI uses a locked forced-command key
(`DEPLOY_PULL_KEY`) that can only run the installer-pull service.

Releases are cut by pushing a tag — bump the version in **both**
`apps/companion/package.json` and `apps/companion/src-tauri/tauri.conf.json`
(the MSIX job reads the version out of `tauri.conf.json`).

## Architecture

Three Rust layers plus a React UI. The split matters: **all audio and networking
live in Rust**, never in the webview (this is what fixed OBS capture, device
selection and mic gain versus a webview-based client).

- **`crates/protocol`** — shared signaling message enums (`ClientMsg`/`ServerMsg`).
  Single source of truth for the wire format; both the server and the client depend
  on it, so a change here is a protocol change on both sides at once.
- **`apps/companion-core`** — the headless engine, and the interesting part of the
  codebase. Builds as both a lib (`companion_core`) and a standalone binary.
  - `lib.rs` — `Engine` + `EngineConfig`, and the `UiEvent` enum. The engine pushes
    state to a `Sink` callback rather than being polled; the Tauri layer forwards
    those events to the frontend. `ChanState` holds per-channel state.
  - `mesh.rs` — one `RTCPeerConnection` per peer, ICE, renegotiation, `rekey()`.
    Glare rule: for any peer pair, the **lexicographically smaller userId offers**;
    the larger one waits. Applies to both the initial offer and re-keying.
  - `audio.rs` — cpal capture/playback, resampling (device rate ↔ 48 kHz — mandatory,
    WASAPI shared mode forces the device mix rate), RNNoise, DSP chain, the 20 ms
    mixer clock.
  - `crypto.rs` — hybrid post-quantum session handshake (ML-KEM-768 + X25519 → HKDF →
    ChaCha20-Poly1305), layered *on top of* DTLS over the DataChannel. Two
    symmetric layers ride on it: per-peer pairwise `Session`s seal chat and ship
    the room key; a single `RoomAudio`/`RoomKey` seals **group voice** — one
    room-wide key so a frame is sealed ONCE and fanned out (preserving
    encode-once). The authority (smallest `user_id`) mints + rotates it (on
    join/leave/manual-rekey) and distributes it sealed over the pairwise
    sessions; a rekey grace defers the send-switch so no audio drops.
  - `signaling.rs` — WS client to InitConnection, keepalive, reconnect with backoff.
  - `serverless.rs` — the no-server 1:1 mode: base64 copy-paste SDP exchange,
    non-trickle ICE, STUN only (no TURN, since there is no cred server).
- **`apps/companion/src-tauri`** — thin Tauri v2 shell: ~30 `#[tauri::command]`
  handlers that validate input at the IPC boundary and drive the engine. Windows-only
  extras live here behind `#[cfg(windows)]` — Raw Input PTT (a `WH_KEYBOARD_LL` hook
  does *not* keep firing under a fullscreen/elevated game) and WASAPI per-session
  audio ducking.
- **`apps/companion/src`** — React UI. `App.tsx` is the whole app (~1.6k lines);
  `Overlay.tsx` is the separate in-game overlay window.
- **`server/init`** — InitConnection: axum + tokio-tungstenite, in-memory rooms, no
  DB. Also serves the multilingual marketing site (`i18n.rs`) and the `/j/:code`
  share landing.

Encode-once fan-out is the load-bearing performance assumption: one
`TrackLocalStaticRTP` is attached to all peer connections and a single `write_rtp()`
fans out to every peer, with webrtc-rs rewriting PT/SSRC and doing SRTP per peer.
Opus encodes once per 20 ms frame regardless of peer count. Verified in
`spikes/track-fanout/`.

## Conventions and gotchas

- Windows-only code is gated `#[cfg(windows)]` and **must** have a Linux fallback —
  global PTT is Windows-only, but the in-app PTT button works everywhere.
- Linux runtime: `main()` sets `WEBKIT_DISABLE_DMABUF_RENDERER=1` +
  `WEBKIT_DISABLE_COMPOSITING_MODE=1` if unset — webkitgtk 2.42+ DMABUF renderer
  gives a blank window on many GPU/driver combos.
- `tauri.conf.json` bundle target is `"all"`: each host builds its own natives —
  Windows → `nsis`+`msi`, Linux → `deb`+`rpm`+`appimage`, macOS → `dmg`+`app`.
  The Linux AppImage bundles webkit2gtk/GTK/libsoup internally (linuxdeploy-plugin-gtk)
  so it runs on rolling distros with no system webkit. `bundle.linux.deb.depends`
  declares deb runtime deps.
- Vite `build.target` must stay `esnext` — esbuild 0.28 errors when Tauri's default
  lower target forces it to downlevel.
- The Tauri CSP is strict (no `null`, no wildcards). Adding an outbound host means
  editing the CSP in `tauri.conf.json`.
- Loopback detection parses the URL **host**, never a substring: `ws://` is allowed
  only for `localhost`/`127.0.0.1`/`::1`, otherwise `wss://` is enforced
  (`server_url_ok`, unit-tested). Don't loosen this.
- The InitConnection server is **fail-closed**: without `ROOM_AUTH_SECRET` it refuses
  to start unless `ALLOW_OPEN_AUTH=1` (dev only). Server env vars are tabulated in
  `README.md`.
- License is PolyForm Noncommercial 1.0.0, not MIT — despite `license = "MIT"` in
  the workspace `Cargo.toml`.

Git remote: `https://github.com/cccdemon/RDOC-SquadLinkLite.git`
