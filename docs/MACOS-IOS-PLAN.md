# macOS + iOS/iPadOS build plan — subraum

Status: **plan only**. Neither target can be built on the Windows dev box or the
Linux LXC builder — Apple's toolchain (Xcode) runs only on macOS. Both are built
via GitHub Actions `macos-14` runners (Apple Silicon, Xcode preinstalled).

## 0. Prerequisites (one-time, Apple account side)
- Apple Developer Program membership ($99/yr) — required to sign + notarize
  (macOS) and to install on real devices / ship to the App Store (iOS).
- Certificates & identifiers created in the Apple Developer portal:
  - macOS: "Developer ID Application" cert (for notarized direct distribution)
    **and/or** "Apple Distribution" (Mac App Store).
  - iOS: "Apple Distribution" cert + an App ID + provisioning profile.
- App-specific password or an App Store Connect API key for `notarytool` /
  `altool` uploads.
- Identifier note: the current `org.raumdock.subraum` is fine for Apple
  bundle IDs (hyphens allowed), unlike Android. Keep it consistent.

## 1. Code readiness
The shared Rust core already builds cross-platform:
- `companion-core`: cpal (CoreAudio on Apple), pure-Rust `webrtc`/opus, no
  Windows-only deps outside `#[cfg(windows)]`.
- `main.rs`: Windows Raw Input PTT is gated `#[cfg(windows)]`; `open_url` has the
  Windows branch + a `#[cfg(not(windows))]` `xdg-open` branch.
  - **iOS/macOS gap:** the `xdg-open` fallback is Linux-only in spirit. Add a
    macOS/iOS branch using `open` (macOS) / Tauri's `opener` plugin, or just use
    the `tauri-plugin-opener` everywhere and drop the manual `open_url`.
- Global PTT: there is no global hotkey on iOS (sandbox forbids it). The in-app
  PTT button is the path. On macOS a global hotkey needs Accessibility
  permission — out of scope for v1; in-app button works.

## 2. macOS (desktop)
Native — closest to the existing Windows/Linux desktop build.

1. `pnpm tauri build` on `macos-14` produces `.app` + `.dmg`.
2. Bundle target: pass `--bundles app,dmg` (override the Windows `nsis,msi`).
3. Signing + notarization (Tauri reads these env vars):
   - `APPLE_CERTIFICATE` (base64 .p12), `APPLE_CERTIFICATE_PASSWORD`
   - `APPLE_SIGNING_IDENTITY` (e.g. "Developer ID Application: Raumdock (TEAMID)")
   - `APPLE_ID`, `APPLE_PASSWORD` (app-specific), `APPLE_TEAM_ID`
   Tauri then signs and notarizes the `.dmg` automatically.
4. Entitlements: microphone access needs `NSMicrophoneUsageDescription` in the
   bundle `Info.plist` (set via `tauri.conf.json` → `bundle.macOS.*` /
   an `Info.plist` fragment). Hardened runtime is required for notarization;
   add the `com.apple.security.device.audio-input` entitlement.
5. Deep link: register the `subraum://` scheme via `CFBundleURLTypes` (Tauri's
   deep-link plugin already wires `on_open_url` for macOS warm relaunch).
6. Universal binary: build `aarch64-apple-darwin` + `x86_64-apple-darwin` and
   `lipo` them, or ship two DMGs. Easiest: `--target universal-apple-darwin`.
7. Output a SHA-256 `SHA256SUMS-macos.txt` like the other workflows.

## 3. iOS / iPadOS
Mobile — same Tauri mobile pipeline as Android, but Xcode-side.

1. `pnpm tauri ios init` generates `src-tauri/gen/apple` (Xcode project). Like
   Android, init-in-CI is viable; or commit the scaffold once.
2. `pnpm tauri ios build` → `.ipa`. Requires:
   - Signing: `APPLE_DEVELOPMENT_TEAM`, a provisioning profile, distribution cert
     installed into a temporary keychain in CI (`apple-actions/import-codesign-certs`).
3. Mandatory `Info.plist` keys (App Store will reject without them):
   - `NSMicrophoneUsageDescription` — "subraum uses the microphone for voice chat."
   - Background audio: add `audio` to `UIBackgroundModes` so the mic/voice keeps
     running when the screen locks (voice app expectation).
4. Deep link / universal links: `CFBundleURLTypes` for `subraum://`; optional
   Associated Domains for `https://subraum.cc` universal links.
5. WebRTC on iOS: works via `webrtc-rs` (no system WebRTC), but verify ICE/UDP
   on cellular + that AAudio/CoreAudio capture starts after the mic permission
   prompt. Test on a real device — the simulator has no mic capture parity.
6. Distribution: TestFlight first (App Store Connect API key upload via
   `xcrun altool`/`notarytool`), then App Store review.
   - **Review risk:** a voice-chat app must show a privacy policy
     (`docs/PRIVACY-POLICY.md` exists) and justify mic + background audio.

## 4. CI shape (when a Mac runner is wired up)
Add `.github/workflows/build-companion-apple.yml`:
- job `macos`: runner `macos-14`, `--bundles app,dmg`, sign+notarize, checksums.
- job `ios`: runner `macos-14`, `tauri ios init` + `tauri ios build`, import certs,
  upload `.ipa` artifact (+ TestFlight on tags).
Mirror the existing Linux/Android workflows (same triggers, checksum + release steps).

## 5. Open decisions before starting Apple builds
- Apple Developer Program enrolled? (org "Raumdock" vs personal)
- Mac App Store + iOS App Store, or direct/TestFlight only for the prototype?
- ~~Universal macOS binary vs separate Intel/ARM DMGs.~~ Decided in v0.3.0: Apple Silicon only. The Intel runner image is deprecated and starved, and an unsigned Intel build had no audience left.
