# Konzept — Natives Video TX/RX im Rust-Core (Tauri-Client)

> Status: **Konzept** (2026-06-08). Noch nicht implementiert. Architektur-Entscheid:
> **Video komplett im Rust-Core, eine PeerConnection pro Peer** (Audio+Video
> zusammen). Ersetzt die frühere „technische Grenze ⛔"-Zeile aus
> [VIDEO-FILESHARING-CONCEPT.md](VIDEO-FILESHARING-CONCEPT.md) §0/§2.1.
> Voice/Chat/Files bleiben unverändert. P2P-first/WebRTC bleibt.

## 0. Ziel & Leitplanken

- Nativer Client kann Video **senden und empfangen**, im selben Mesh wie Audio.
- **Eine** PeerConnection pro Peer (Audio-m-line bestehend + neue Video-m-line) →
  saubere Mesh, symmetrische Interop mit dem Browser-Participant (auch 1 PC).
- Rust-Audio-Core (cpal/opus/Mixer/nnnoiseless) + globales PTT bleiben.
- Video optional, Default Kamera **aus**, kein Fake-Video.
- **Effekte (Blur/Removal) nativ = out of scope** in diesem Pfad (Frames liegen
  vor-Encode in Rust; Segmentierung in Rust unverhältnismäßig). Effekte bleiben
  Stärke des Browser-Participant. Sauber dokumentiert, nicht in der App versprochen.

## 1. Pipeline-Überblick

```
SENDEN:
  Kamera ──nokhwa──▶ Rohframe(NV12/RGB) ──convert──▶ I420
        ──openh264-encode──▶ H264-NAL ──▶ webrtc-rs TrackLocalStaticSample(H264)
        ──(encode-once fan-out, SRTP/Peer)──▶ alle PeerConnections

EMPFANGEN:
  PeerConnection on_track(video) ──read_rtp──▶ H264-Depacketizer(Annex-B)
        ──Tauri Channel(bytes)──▶ WebView WebCodecs VideoDecoder
        ──▶ VideoFrame ──▶ <canvas> pro Peer

SELF-PREVIEW:
  lokale H264-NALs ──gleicher Tauri-Channel──▶ WebCodecs ──▶ eigenes <canvas>
```

Kernidee wie beim Audio-Spike (`spikes/track-fanout`): **ein** shared Video-Track
an N PeerConnections, einmal encoden, webrtc-rs macht RTP-Rewrite + SRTP pro Peer.

## 2. Capture (Rust)

- Crate **`nokhwa`** (Windows: MediaFoundation/MSMF-Backend). Liefert Frames
  (NV12/YUYV/RGB), Auflösung/FPS konfigurierbar.
- Eigener **std-Thread** (wie Audio, außerhalb tokio — `audio.rs`-Muster).
- Device-Enumeration für Kamera-Auswahl (analog `list_audio_devices`).
- Default **480p@30** (Mesh-Bandbreite, §8). 720p optional.
- Lizenz prüfen (nokhwa = Apache-2.0/MIT) + im Doc festhalten.

## 3. Farbkonvertierung

- Kamera-Format → **I420 (YUV420p)** für den Encoder.
- nokhwa kann zu RGB dekodieren; RGB→I420 bzw. NV12→I420 per `yuv`/
  `dcv-color-primitives` oder kleiner eigener Konverter.
- Auf dem Capture-Thread, vor Encode.

## 4. Encode (Rust) — H264 via openh264

- Crate **`openh264`** (Cisco OpenH264). **Begründung:** liefert/linkt ein
  **prebuilt** Binary → baut auf **Windows-MSVC ohne autotools/libtoolize**.
  Das umgeht genau die C-Build-Wand, an der `webrtc-audio-processing` (APM) und
  libvpx-Vendoring auf MSVC scheitern (siehe Projekt-Historie, Phase 4 APM-Spike
  failed). Patent: Ciscos OpenH264-Binary ist royalty-frei.
- Encode I420 → **H264 Annex-B NAL-Units**. Bitrate/Keyframe-Intervall
  konfigurierbar; periodische IDR (z. B. alle 2 s) für späten Joiner/Resync.
- **Encode-once:** ein `TrackLocalStaticSample` mit H264-Capability, an alle PCs
  gebunden; `write_sample()` einmal → webrtc-rs packetisiert (H264-RTP) + SRTP/Peer.
- Alternative VP8 (`vpx-encode`) bewusst **verworfen**: libvpx-C-Build auf MSVC =
  Risiko; kein reifer pure-Rust-VP8-Encoder. H264 ist der pragmatische Weg.

## 5. webrtc-rs Integration

- MediaEngine: **H264 registrieren** (`register_codec`, `RTCRtpCodecCapability`
  `video/H264`, passende `sdp_fmtp_line` z. B. `packetization-mode=1;profile-level-id=42e01f`).
- Sendeseite: shared `TrackLocalStaticSample` (H264) bei **PC-Aufbau** hinzufügen
  (analog Audio `mesh.rs:118`). **Renegotiation-frei:** Track existiert immer;
  Kamera-aus = einfach **keine** Samples schreiben (stiller Video-Track, keine
  SDP-Änderung). Kamera-an = Samples fließen. Toggle ohne Reneg.
- Empfangsseite: `on_track` filtert `kind==video` → `read_rtp` → **H264-Depacketizer**
  (`webrtc::rtp::codecs::h264::H264Packet`) reassembliert NALs (Annex-B/AVCC) →
  an die Render-Bridge (§6).
- Audio-`on_track` bleibt exakt wie heute (kind==audio → Opus-Decode-Pipeline).
  Demux per Track-`kind`, kein Protokoll-Eingriff am Media-Pfad.

## 6. Render-Bridge → WebView (WebCodecs)

- **Kein Rust-H264-Decoder** nötig (dodge zweite C-Abhängigkeit). Stattdessen
  encoded NALs an die WebView, dort dekodieren.
- Transport Rust→WebView: **Tauri v2 `Channel<Vec<u8>>`** (streamt Bytes effizient,
  kein base64/IPC-JSON-Overhead). Pro empfangenem Frame ein Chunk inkl. kleinem
  Header `{peer_id, is_keyframe, ts}` + NAL-Bytes.
- WebView: **WebCodecs `VideoDecoder`** (in WebView2/Chromium vorhanden) pro Peer.
  `decode(EncodedVideoChunk)` → `VideoFrame` → `canvas.drawImage` / `<canvas>` im
  Remote-Grid. Wartet auf Keyframe vor erstem Decode.
- Bandbreite IPC: H264 480p30 ≈ 0,5–1 Mbit/s ⇒ ~60–125 KB/s pro Stream — unkritisch
  über den Channel.

## 7. Self-Preview & Kamera-Exklusivität

- **Wichtig:** Die Kamera gehört dem Rust-Prozess (nokhwa). Die WebView kann
  **nicht** parallel `getUserMedia` auf dieselbe Kamera → Self-Preview kommt **aus
  Rust**: lokale encoded NALs über denselben Tauri-Channel an WebCodecs → eigenes
  `<canvas>`. Kein zweiter Kamera-Zugriff.

## 8. Mesh-Bandbreite (neuer Druck durch Video)

- Full-Mesh: jeder Peer empfängt N−1 Videoströme. Video ist um Größenordnungen
  teurer als Opus. Cap warn@12/hard@16 bleibt, aber **mit Video realistisch
  niedriger**.
- Maßnahmen: Default **480p@30**, niedrige Bitrate-Cap, optional FPS-Drosselung;
  ggf. „nur X Kameras gleichzeitig aktiv"-Soft-Limit. Keine Simulcast/SVC in
  webrtc-rs → bewusst einfache, niedrige Profile.
- Doku-Hinweis: Video erhöht TURN-Bandbreitenkosten deutlich, wenn Relay aktiv.

## 9. Signaling/State (minimal, backward-compat-bewusst)

- `Video { active }` in `ClientMsg`/`ServerMsg` (analog `Ptt`, `protocol/lib.rs:52`;
  Relay wie `Ptt` in `server/init/src/main.rs:490`). Teilt dem Roster mit, ob die
  Kamera eines Peers an ist (steuert Grid-Slot/Badge; das Bild selbst kommt über die
  immer vorhandene Video-m-line).
- Backward-Compat: getaggte serde-Enums → alter Client kann neuen `video`-Tag nicht
  parsen. Alle Clients aus einem Crate, koordiniert deployen; im Doc vermerkt.
  Browser-TS-Mirror führt `video` mit.
- Media-Pfad braucht **keine** Protokolländerung (Demux per Track-`kind`).

## 10. UI (`apps/companion/src/App.tsx`, `style.css`)

- Kamera-Toggle in den Media-Controls („Kamera an/aus"), OS-Permission-Hinweis vor
  erstem Start, saubere Fehlermeldung wenn keine Kamera/abgelehnt — App bleibt nutzbar.
- Kamera-**Auswahl** (Device-Liste aus Rust); Geräte-ID darf gespeichert werden
  (nicht sensibel), **Kamera-Status nicht** auto-persistiert (Default aus, Privacy).
- **Remote-Video-Grid:** `<canvas>` pro Peer mit aktivem Video; sonst Avatar/Name/
  Status. Badge `DIREKT`/`RELAY (TURN)` bleibt.
- Self-Preview-Kachel.
- Keine langen Erklärtexte; Technik in `docs/`.

## 11. Tauri-Commands / Core-API

- Core (`apps/companion-core/src/lib.rs`): `EngineConfig` ggf. `camera_device:
  Option<String>`; `Engine::set_camera(on: bool)`, `list_cameras()`. Neue
  `UiEvent`/Channel für Render-Frames + `video_enabled`-Roster-Feld.
- Tauri (`src-tauri/src/main.rs`): Commands `set_camera`, `list_cameras`; der
  Frame-`Channel` wird beim `connect` an die WebView übergeben.
- `mesh.rs`: shared Video-Track bei `ensure()` hinzufügen; `on_track`-Video-Zweig +
  Depacketizer; lokale Sample-Quelle anbinden.

## 12. Risiken & Mitigation

| Risiko | Mitigation |
|---|---|
| openh264-Build auf MSVC | Crate liefert prebuilt Binary; früh auf CI (windows-latest) verifizieren (eigener Spike) |
| nokhwa MSMF-Robustheit | Spike mit Device-Enum + Fallback-Format; saubere Fehler |
| H264-Interop Browser↔Rust | beide H264, `packetization-mode=1`, gängiges Baseline-Profile; gegen Chrome testen |
| Mesh-Video-Bandbreite | 480p-Default, Bitrate-Cap, FPS-Drossel, Kamera-Soft-Limit |
| Render-Latenz Channel→WebCodecs | encoded (klein) statt raw; auf Keyframe warten; per-Peer-Decoder |
| Kamera-Exklusivität | Rust besitzt Kamera; WebView nie `getUserMedia` (Self-Preview aus Rust) |

## 13. Phasen (eigene Spikes, Build-Reihenfolge)

1. **Capture-Spike** `spikes/video-capture` — nokhwa: Device-Enum, Frame-Pull, Format.
2. **Encode-Spike** `spikes/h264-encode` — openh264 I420→H264, **MSVC/CI-Build verifizieren** (Go/No-Go-Gate).
3. **Send** — H264-Track in webrtc-rs, Browser empfängt (encode-once an N).
4. **Receive+Render** — Depacketize → Tauri-Channel → WebCodecs → canvas.
5. **Self-Preview** + Kamera-Toggle + `Video{active}` + Device-Select.
6. **Remote-Grid-UI** + Roster `video_enabled`.
7. **Mesh-Härtung** — Bandbreite/Res-Caps, N-Peer-Video, TURN-Last.

> Gate nach Phase 2: scheitert openh264 auf MSVC, vor dem Weiterbau Rücksprache
> (Alternative VP8-Vendoring oder Pfad-A-WebView-Video neu bewerten).

## 14. Akzeptanzkriterien (Ergänzung)

- [ ] Voice/Chat/Files unverändert (keine Regression).
- [ ] Nativer Client sendet Video (Kamera an) und empfängt/zeigt Remote-Video.
- [ ] Eine PeerConnection pro Peer (Audio+Video), kein zweiter Stack.
- [ ] Kamera Default aus; Toggle ohne Renegotiation; kein Videotrack-Inhalt wenn aus.
- [ ] Interop: nativer Client ↔ Browser-Participant Video beidseitig (H264).
- [ ] `video_enabled` im Roster; Remote-Grid + Self-Preview.
- [ ] Effekte nativ als nicht-verfügbar dokumentiert (nicht in App versprochen).
- [ ] Keine Server-Speicherung; keine Secrets/Tokens persistent.

## 15. Tests/Checks

- Encode-Roundtrip-Test (I420→H264→Depacketize-Reassembly, NAL-Grenzen).
- `Video{active}` serde-Roundtrip.
- Interop-Matrix manuell: nativ→Browser, Browser→nativ, Kamera an/aus ohne
  Audio-Drop, Late-Joiner sieht Bild nach nächstem Keyframe, 3-Wege mit 2 Kameras.
- `cd apps/companion && pnpm build`; `cargo test -p companion-core`.
- Spike-Builds auf CI (windows-latest) für nokhwa + openh264.

## Geänderte/neu angelegte Dateien (geplant, NICHT umgesetzt)

- **neu** `spikes/video-capture/`, `spikes/h264-encode/` (Go/No-Go vor Core-Integration)
- **neu** `apps/companion-core/src/video.rs` — capture + convert + encode + depacketize-bridge
- **edit** `apps/companion-core/src/mesh.rs` — shared Video-Track, `on_track`-Video, Render-Tap
- **edit** `apps/companion-core/src/lib.rs` — `set_camera`/`list_cameras`, Frame-Channel, `video_enabled`
- **edit** `apps/companion/src-tauri/src/main.rs` — Commands + Frame-`Channel` an WebView
- **edit** `apps/companion/src/App.tsx` + `style.css` — Kamera-Toggle, Remote-Grid (WebCodecs/canvas), Self-Preview, Device-Select
- **edit** `crates/protocol/src/lib.rs` — `Video{active}`
- **edit** `Cargo.toml` (companion-core) — `nokhwa`, `openh264`, Farb-Konverter
- **edit** `README.md`, `docs/ARCHITECTURE.md` — Video-Modi, H264-Wahl, Bandbreite, Effekt-Grenze nativ
- **keine** Server-Speicherung; `server/init` nur `Video`-Relay
