# RDOC SquadLink Lite — project notes

## Build hosts

### Linux / Android builds — Proxmox LXC 103
Ubuntu 24.04.4 LTS, x86_64 (container hostname `streamer`). Reached through the
Proxmox host `ve.raumdock.org` with the `claude_deploy` key, then `pct exec`:

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
`.github/workflows/build-companion.yml` (runner `windows-latest`). Cross-platform
builds (Linux multi-arch, Android, macOS, iOS) are planned as a GitHub Actions matrix.

`ve.raumdock.org` is also the deploy box: CI uses a locked forced-command key
(`DEPLOY_PULL_KEY`) that can only run the installer-pull service.

## App layout
- `apps/companion` — Tauri 2 desktop app (React + TS frontend, `src-tauri` Rust shell).
- `apps/companion-core` — headless engine: cpal + opus audio over webrtc-rs P2P mesh.
- Windows-only code is gated `#[cfg(windows)]`; Linux fallbacks exist. Global PTT
  (Raw Input) is Windows-only; the in-app PTT button works everywhere.
- `tauri.conf.json` bundle target is `"all"`: each host builds its natives —
  Windows → `nsis`+`msi`, Linux → `deb`+`rpm`+`appimage`, macOS → `dmg`+`app`.
  Linux AppImage bundles webkit2gtk/GTK/libsoup internally (via
  linuxdeploy-plugin-gtk) → runs on rolling distros (Arch) with no system webkit.
  `bundle.linux.deb.depends` declares deb runtime deps. Mobile still needs its own
  targets.
- Linux runtime: `main()` sets `WEBKIT_DISABLE_DMABUF_RENDERER=1` +
  `WEBKIT_DISABLE_COMPOSITING_MODE=1` (if unset) — webkitgtk 2.42+ DMABUF renderer
  gives a blank window on many GPU/driver combos.

Git remote: `https://github.com/cccdemon/RDOC-SquadLinkLite.git`
