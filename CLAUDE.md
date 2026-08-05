# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

# subraum — project notes

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
pnpm dev                         # vite only — webview in a browser, no Tauri IPC
pnpm build                       # tsc typecheck + vite build (frontend only)
pnpm tauri icon src/subraum.png  # regenerate app icons (source: subraum-icon.svg)
```

There is **no frontend test runner and no linter** — `pnpm build` (i.e. `tsc`) is the
only static check the UI gets. Don't go looking for `pnpm test` / `pnpm lint`.

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

Tests live in `#[cfg(test)]` modules inside `lib.rs`, `mesh.rs`, `crypto.rs`,
`signaling.rs` (companion-core) and `crates/protocol/src/lib.rs`. There is no
separate test dir. On `main`, `server/init` + `src-tauri` have no tests yet —
the pushed branch `fix/keyless-guidance` adds them (auth/turn/sessions + IPC
guards) plus a 3-node room-key simulation; drop this sentence when it merges.

## Build hosts

### Linux / Android builds — Proxmox LXC 103
Local Windows `cargo` does not work at all — verify **every** Rust change on LXC
103. Two separate breakages: aws-lc-sys fails its C compile, and the MSVC linker
can't find `msvcrt.lib`, which kills even a pure-Rust crate's build scripts
(`cargo test -p protocol` → `LINK : fatal error LNK1104`). Don't burn a cycle
trying locally first. Ubuntu 24.04.4 LTS, x86_64 (container hostname `streamer`).
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
  `target/release/subraum` in ~1m10s (2026-06-17).
- Checkouts live in `/root`: `RDOC-SquadLinkLite` (git clone, stale) and
  `depupd` (a plain copy, no `.git`, carrying the dep-update tree). Point
  `CARGO_TARGET_DIR` at a shared `target/` to reuse a warm cache across them.
- **The container is disk-tight and shares the box with production** —
  `subraum-init`, `rdoc-suite-fleetplanner`, `cc-postgres` and friends run here.
  Rust `target/` dirs filled it to 100% once (2026-07-30), which would have
  broken the release's installer pull. Check `df -h /` before a big build and
  `rm -rf /root/*/target` when it gets tight; those are pure build caches.
- There is **no direct file transfer** — `pct exec` is the only channel, and it
  forwards stdin, so pipe the file in rather than reaching for scp:

```sh
base64 -w0 path/to/file.rs > /tmp/b64.txt
ssh -i ~/.ssh/claude_deploy root@ve.raumdock.org \
  "pct exec 103 -- bash -lc 'base64 -d > /root/depupd/path/to/file.rs'" < /tmp/b64.txt
```

### Per-platform GitHub Actions
Every platform has its own workflow; all of them attach bundles to the GitHub
Release on a `subraum-v*` tag.

| Workflow | Runner(s) | Output |
| --- | --- | --- |
| `build-companion.yml` | `windows-latest` | NSIS + MSI, unsigned MSIX for the Store; on a tag also cuts the prerelease and tells the server to pull the installer |
| `build-companion-linux.yml` | `ubuntu-22.04` amd64 + arm64 matrix | deb/rpm/AppImage per arch |
| `build-companion-flatpak.yml` | `ubuntu-22.04`, three jobs | `deb` compiles natively, `flatpak` unpacks it into `/app` in the GNOME 47 SDK → `.flatpak` for SteamOS / Steam Deck / immutable distros, `release` (tag-only) attaches the bundle from a plain runner — the SDK container has no `gh`, uploading from inside it fails with "command not found" |
| `build-companion-macos.yml` | `macos-14` (arm64) + `macos-13` (Intel) | dmg + app per arch, **unsigned and un-notarized** (Gatekeeper blocks first launch). `macos-13` is deprecated and starved — the Intel job can sit queued for hours; don't block a release on it |
| `build-companion-android.yml` | `ubuntu-22.04` | debug-signed sideload APK; `workflow_dispatch` **only** — `gen/android` is not committed, `tauri android init` runs fresh in CI |

Android/Flatpak/macOS builds are all unsigned — no Apple account, no Play keystore,
no Flatpak GPG key yet.

`ve.raumdock.org` is also the deploy box: CI uses a locked forced-command key
(`DEPLOY_PULL_KEY`) that can only run the installer-pull service.
`deploy/pull-installer.sh` mirrors **only** `exe|msi|deb|rpm|AppImage|apk` into
the `/get` download page — the Flatpak and macOS dmg exist solely as GitHub
Release assets and never appear on `subraum.cc/get`, even though README and the
Store copy advertise the Flatpak there.

The Windows workflow gates the build on `pnpm audit --audit-level moderate` — a
**blocking** step, so any moderate transitive advisory fails CI before anything is
built (fix via `pnpm.overrides` in `apps/companion/package.json`; `esbuild` and
`postcss` are pinned there today). `cargo audit` runs right after but is
`|| true` — advisory only.

### Cutting a release
Push a `subraum-v*` tag. Before tagging:

1. Bump the version in **both** `apps/companion/package.json` and
   `apps/companion/src-tauri/tauri.conf.json`.
2. Add the release section to `CHANGELOG.md` (German, user-facing; older releases
   are tagged `squadlink-lite-v*`).

`apps/companion/msix/AppxManifest.xml` carries a hardcoded `Identity/@Version`
that is **deliberately stale** (0.1.35.0 while the app is 0.2.0) — the MSIX job
rewrites it from `tauri.conf.json` as `"$ver.0"` before packing. Don't hand-sync
it and don't "fix" the mismatch.

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
    Two consequences of "authority = smallest id" that bit once already:
    ids are random, so a **later** joiner can become authority — it must learn
    the live generation via `CtrlMsg::RoomGen` before minting, or it starts at
    generation 1 and every member rejects that key as stale (silence both ways).
    And the key only ever travels the pairwise session **with the authority**, so
    an unreachable authority (STUN-only, strict NAT) leaves a peer keyless on
    otherwise-healthy links — it is heard (raw fallback) but hears nothing.
    Encode-once rules out serving that peer plaintext, so the engine warns
    instead (`KEYLESS_WARN_AFTER`).
  - `signaling.rs` — WS client to InitConnection, keepalive, reconnect with backoff.
  - `serverless.rs` — the no-server 1:1 mode: base64 copy-paste SDP exchange,
    non-trickle ICE, STUN only (no TURN, since there is no cred server).
  - `selfcheck.rs` — network diagnostic with no external test peer: a local
    two-`RTCPeerConnection` DataChannel echo proves send/receive, a public STUN
    reflexive candidate proves outbound UDP works, plus a signaling reachability
    probe.
- **`apps/companion/src-tauri`** — thin Tauri v2 shell: ~30 `#[tauri::command]`
  handlers that validate input at the IPC boundary and drive the engine. Windows-only
  extras live here behind `#[cfg(windows)]` — Raw Input PTT (a `WH_KEYBOARD_LL` hook
  does *not* keep firing under a fullscreen/elevated game) and WASAPI per-session
  audio ducking. `main.rs` is a stub that calls `lib.rs::run()`; `run()` carries
  `#[cfg_attr(mobile, tauri::mobile_entry_point)]` because Tauri mobile builds the
  app as `--lib`. Desktop-only plugins must stay behind `cfg(desktop)`, or the
  Android build breaks.
- **`apps/companion/src`** — React UI. `App.tsx` is the whole app (~1.6k lines);
  `Overlay.tsx` is the separate in-game overlay window.
- **`server/init`** — InitConnection: axum + tokio-tungstenite, in-memory rooms, no
  DB. Also serves the multilingual marketing site (`i18n.rs`) and the `/j/:code`
  share landing.
  - `auth.rs` — room join token = `HMAC-SHA256(ROOM_AUTH_SECRET, room)` hex. The
    binary's `mint` subcommand produces invite tokens. Fail-closed (see gotchas).
  - `turn.rs` — ephemeral coturn `use-auth-secret` REST credentials
    (`username = "<expiry>:<user_id>"`, `credential = base64(HMAC-SHA1(secret, username))`).
    coturn validates the HMAC itself, so there is still no account store. Enabled
    only when both `TURN_SECRET` and `TURN_URLS` are set.
  - `tls.rs` — self-signed cert generated on first run and **persisted**, so the
    SHA-256 fingerprint clients pin as `CERT_SHA256` stays stable across restarts.
  - `sessions.rs` — in-memory room/roster state.
  - `i18n.rs` — every string of the server-rendered site, hardcoded for **5
    languages** (EN/DE/IT/ES/FR; `?lang=` > `Accept-Language` > English). It is
    the biggest file in the server (~1k lines) — adding one page string means
    adding five `match` arms, not one.
  - `assets/` — brand assets compiled into the binary (`include_str!`/
    `include_bytes!`) and served at `/assets/logo.svg` + `/assets/og-image.png`,
    so a deploy needs no files copied onto the host. `og-image.png` is rendered
    from `og-image.svg` (scrapers don't render SVG) — regenerate with
    `rsvg-convert -w 1200 -h 630 og-image.svg -o og-image.png` and the
    JetBrains Mono font installed.

`apps/web-participant` is a concept only (`CONCEPT.md`, no code). `deploy/` holds
the docker-compose stacks, `turnserver.conf`, and `pull-installer.sh` (the target
of the CI forced-command deploy key). Per-topic docs live in `docs/`: shipped
platforms (`LINUX.md`, `MACOS-IOS-PLAN.md`, `MSIX-PACKAGING.md`,
`STORE-SUBMISSION.md`, `STORE-LISTING.md`, `PRIVACY-POLICY.md`,
`FLEETPLANNER-DEEPLINK.md`) and **unbuilt concepts** —
`TURN-RELAY-FALLBACK-CONCEPT.md`, `NATIVE-VIDEO-CONCEPT.md`,
`VIDEO-FILESHARING-CONCEPT.md` describe designs with no code behind them, so
don't read them as documentation of what exists.

Encode-once fan-out is the load-bearing performance assumption: one
`TrackLocalStaticRTP` is attached to all peer connections and a single `write_rtp()`
fans out to every peer, with webrtc-rs rewriting PT/SSRC and doing SRTP per peer.
Opus encodes once per 20 ms frame regardless of peer count. Verified in
`spikes/track-fanout/`.

## Conventions and gotchas

- Windows-only code is gated `#[cfg(windows)]` and **must** have a Linux fallback —
  global PTT is Windows-only, but the in-app PTT button works everywhere.
- Language split: code, comments, doc-comments and commit messages are **English**;
  user-facing prose (`README.md`, `CHANGELOG.md`, `docs/STORE-LISTING.md`, the UI
  strings, the site copy in `i18n.rs`) is **German** first. Match whichever side of
  the line you're editing.
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
- **TURN is not deployed.** Production `subraum-init` runs without
  `TURN_SECRET`/`TURN_URLS` and no coturn exists, so `ServerMsg::Turn` is never
  sent and the client's "TURN-Relay-Fallback" toggle does nothing (verified
  2026-07-30). Never write user-facing text that recommends enabling the relay —
  the 0.2.1 keyless warning made that mistake. `deploy/turnserver.conf` and the
  compose stack are ready but unused; `turn.rs` is live code behind a dead env
  var.
- License is PolyForm Noncommercial 1.0.0, not MIT — despite `license = "MIT"` in
  the workspace `Cargo.toml`.

## Naming

The product was renamed from **RDOC SquadLink Lite** to **subraum** (tagline:
"encrypted communication", domain `subraum.cc`). What that does and does not
cover:

| Thing | Value |
| --- | --- |
| Product / `productName` / window title | `subraum` (lowercase, deliberate) |
| Domain | `subraum.cc` — replaced `squadlink.raumdock.org` |
| Tauri identifier | `org.raumdock.subraum` |
| Flatpak app-id | `org.raumdock.Subraum` (D-Bus name rules: no hyphens) |
| Microsoft Store listing | `Subraum Communicator` — `AppxManifest.xml` `Properties/DisplayName` must match the reservation exactly |
| Crate + binary | `subraum`, lib `subraum_lib` |
| Deep-link scheme | `subraum://` — `squadlink://` still registered + parsed |
| Release tag prefix | `subraum-v*` |

Backwards compatibility that must not be dropped while old builds are in the
wild:

- **Both deep-link schemes work.** `DEEPLINK_SCHEMES` in `src-tauri/src/lib.rs`,
  `schemes` in `tauri.conf.json`, two `uap:Protocol` entries in
  `AppxManifest.xml`, and `LINK_SCHEMES` in `App.tsx` — change them together.
- **`squadlink.raumdock.org` stays in the Tauri CSP.** `baseFromInput()` derives
  the session host from a pasted share link, so an old `/j/<code>` link fetches
  the old host; CSP checks the pre-redirect URL, so removing it breaks old links.
- **The old host must keep proxying `/ws`, not redirect it** — see the Caddy
  snippet in `deploy/README.md`. Installed clients have that WS URL baked in and
  tungstenite does not follow redirects.

Deliberately **not** renamed — don't "fix" these:

- `crypto.rs::KDF_LABEL = b"rdoc-squadlink-pqc-v1"` — a domain-separation label
  baked into the PQC handshake. Changing it is a protocol break: renamed clients
  could not talk to any released build.
- The GitHub repo is still `cccdemon/RDOC-SquadLinkLite` — `REPO` in `App.tsx`
  and `pull-installer.sh` (update check / installer pull) must keep that slug
  until the repo itself is renamed.
- **MSIX `Identity Name = raumdock.org.RDOC-SquadLinkLite`.** The Store assigns
  package identity; the listing was renamed in place, so identity, package family
  and Store ID (9N9NR49QFBF4) are unchanged and Store installs keep updating
  across the rename. Rewriting it to a "subraum" spelling forks the package
  family and orphans every Store install. Only `Properties/DisplayName` carries
  the new name.
- Infra under `raumdock.org`: the deploy box `ve.raumdock.org`, the publisher
  name "Raumdock", `commercialusage@raumdock.org`, the RDOC-Suite Caddy.
- Host-side deploy state (download dir, installer systemd unit, Caddy vhost) —
  see the migration checklist in `deploy/README.md`.

## Public routing

Requests reach InitConnection through three hops, none of which live in this
repo — useful when a hostname "just doesn't resolve":

```
:443 → Proxmox DNAT (85.215.253.135) → LXC 101 "proxy", nginx stream, SNI map
     → 10.10.10.99:9443 (LXC 103, rdoc-suite-caddy, ACME DNS-01 via Cloudflare)
     → 127.0.0.1:8090 (the init container)
```

- The SNI map is `/etc/nginx/stream.d/minecraft.raumdock.org.conf` on LXC 101
  (misleading filename — it holds every hostname). `subraum.cc` is already
  mapped to the same backend as `squadlink.raumdock.org`.
- Caddy issues certs over DNS-01, so its Cloudflare token needs `Zone:DNS:Edit`
  on each zone it serves — including `subraum.cc`.
- There is **no IPv6 path**: the Proxmox host has no global v6 address and
  `net.ipv6.conf.all.forwarding = 0`. The AAAA record on
  `squadlink.raumdock.org` points somewhere this infrastructure cannot serve.
  Don't add AAAA records for new hostnames here.

Git remote: `https://github.com/cccdemon/RDOC-SquadLinkLite.git`
