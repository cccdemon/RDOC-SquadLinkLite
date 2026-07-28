# Konzept — Video, Camera & Filesharing (optional)

> Status: **Konzept** (2026-06-08). Noch nicht implementiert. Erweitert RDOC
> subraum von Voice+Chat zu optionalem „Voice, Video & Filesharing".
> Bestehender Voice-only-Flow bleibt unverändert. P2P-first/WebRTC bleibt.
> Verwandt: [TURN-Relay-Konzept](TURN-RELAY-FALLBACK-CONCEPT.md),
> [Browser-Participant-Konzept](../apps/web-participant/CONCEPT.md).

## 0. Kern-Verdict (zuerst lesen)

Der **native Core ist reines Rust-Audio**: cpal + audiopus + nnnoiseless
(`apps/companion-core/src/audio.rs:1`), webrtc-rs ist **nur für Audio** verdrahtet
(eine `TrackLocalStaticSample`, `apps/companion-core/src/mesh.rs:118`), der einzige
DataChannel ist `"chat"` mit **Text**-Frames (`mesh.rs:207,299`). Es gibt **keinen**
Kamera-Capture, **keinen** Video-Encoder/Decoder und **keinen** Video-Render-Pfad
im Rust-Core.

Daraus folgt die ehrliche Aufteilung — **kein Fake-Video**:

| Feature | Browser-Participant (WebView/Web) | Nativer Tauri-Client (Rust-Core) |
|---|---|---|
| Voice TX/RX | ✅ | ✅ (bestehend) |
| Chat | ✅ | ✅ (bestehend) |
| **Video TX/RX** | ✅ `getUserMedia` + `addTrack` | ✅ **nativ in Rust** (nokhwa+openh264, 1 PC) — siehe [NATIVE-VIDEO-CONCEPT.md](NATIVE-VIDEO-CONCEPT.md) |
| **Video-Effekte** (Blur/Removal) | ✅ Canvas/WebGL/MediaPipe | ⛔ out of scope nativ (Effekte = Browser-Stärke) |
| **Filesharing** | ✅ RTCDataChannel | ✅ RTCDataChannel (Rust, machbar) |

**Empfehlung:** Video + Effekte primär im **Browser-Participant** bauen
(natürlicher Boden: WebView hat Kamera, Canvas, WASM-Segmentierung). Der native
Client bekommt **Filesharing** voll und die **Video-Presence-Grundlage**
(`video_enabled`-Flag, Remote-Grid zeigt „kein Video" für native Peers). Nativer
Kamera-Versand = eigene spätere Phase, sauber als Grenze dokumentiert (§2.1).

## 1. Produkt-/Architekturziel

- Voice: bestehend, P2P-first, unverändert.
- Video: optional pro Nutzer, Default **aus** (Browser-Participant zuerst).
- Filesharing: P2P über DataChannel, beide Clients.
- Video-Effekte (Browser): Aus / Weichzeichnen / Hintergrund entfernen / optional
  Hintergrundbild.
- UI macht klar sichtbar: Mikrofon/Voice · Kamera/Video · Relay-Fallback · Filesharing.

Invarianten:
- Keine unrelated Refactors.
- Voice/Chat darf nicht regressieren.
- Video + Filesharing optional, pro Nutzer aktivierbar.
- TURN bleibt optionaler Relay-Fallback (eigenes Konzept).
- **Keine** server-seitige Speicherung von Medien/Dateien — Init bleibt Signaling-only.

## 2. WebRTC Video

### 2.1 Nativer Client — jetzt eingeplant (Pfad gewählt)

> **Update 2026-06-08:** Natives Video TX/RX wird gebaut, Architektur = **Video im
> Rust-Core, 1 PeerConnection** (nokhwa-Capture + openh264-Encode + WebCodecs-Render).
> Vollständiges Konzept: [NATIVE-VIDEO-CONCEPT.md](NATIVE-VIDEO-CONCEPT.md). Der
> folgende Abschnitt beschreibt die ursprüngliche Grenze als Kontext, warum es Arbeit ist.

Outbound-Video aus dem Rust-Core ist groß (darum eigenes Konzept). Gründe/Bausteine:
- **Kein Encoder:** webrtc-rs bündelt keinen VP8/VP9/H264-Encoder; man müsste
  libvpx/openh264-Bindings + RTP-Packetizer ergänzen.
- **Kein Capture:** keine Kamera-Quelle im Core (Audio ist cpal; Video bräuchte
  z. B. `nokhwa`).
- **Kein Render:** der Stream lebt in webrtc-rs (Rust), **nicht** in der Tauri-WebView
  — Remote-Frames müssten erst in die WebView gepiped werden. Auch das fehlt.

Konsequenz für den nativen Client in diesem Schritt:
- Protokoll/State/UI-Grundlage für Video (`video_enabled`-Flag, Remote-Grid-Slots).
- **Kein** Videotrack wird gesendet, wenn keiner existiert (trivially erfüllt).
- Remote-Video von Browser-Peers: native UI zeigt „Video aktiv (im Browser ansehen)"
  bzw. Avatar/Status — kein gefälschtes Bild.

### 2.2 Browser-Participant — echtes Video

- `getUserMedia({video:true})`, Track zu bestehenden PeerConnections.
- **Renegotiation vermeiden:** beim PC-Aufbau einen **Video-Transceiver vorab**
  anlegen (`addTransceiver('video', {direction:'sendrecv'})`); Kamera-an =
  `sender.replaceTrack(videoTrack)`, Kamera-aus = `replaceTrack(null)` + Track
  stoppen. So kein SDP-Renegotiation-Tanz im Mesh nötig (der aktuelle Mesh macht
  primär ein Offer/Answer; `mesh.rs:217` kennt Re-Offer, aber Track-Add-Reneg in
  webrtc-rs ist fragil → Transceiver-Preallocation ist der sichere Weg).
- Default: Kamera aus → kein Videotrack aktiv.
- Roster zeigt `video_enabled` pro Peer (§8).
- Remote-Video in der UI als Grid (`<video>` pro aktivem Peer).

## 3. Kamera-Aktivierung (UI)

- Button/Icon: „Kamera an" / „Kamera aus".
- Vor erstem Aktivieren klar: OS-Kamerazugriff nötig.
- Permission verweigert / keine Kamera → saubere Fehlermeldung, App bleibt nutzbar.
- Kamera-Auswahl (`enumerateDevices`) falls vertretbar.
- **Default bei App-Start: aus** — Kamera-Status nicht auto-aktiv persistieren
  (Datenschutz). Geräte-**Auswahl** darf gespeichert werden (Label/ID, nicht sensibel),
  Analog zu `sa.audio`.

## 4. Video Background Effects (Browser, pragmatisch + optional)

Modi: „Aus" / „Weichzeichnen" / „Hintergrund entfernen" / optional „Hintergrund ersetzen".

- Effekte dürfen Voice/Chat **nicht** blockieren (eigener Verarbeitungspfad,
  Worker/OffscreenCanvas wo möglich).
- Schwache Hardware → Nutzer kann Effekte abschalten.
- **Feature-Flag/Fallback:** UI bietet nur **verfügbare** Modi an. Nicht verfügbare
  Modi nicht irreführend anzeigen.
- **Nur lokale Verarbeitung.** Keine Frames an externe APIs. Keine
  Cloud-Background-Removal-Dienste.
- Pipeline: `getUserMedia` → Segmentierung → Canvas-Compositing →
  `canvas.captureStream()` → der so erzeugte Track ersetzt via `replaceTrack` den
  Kamera-Track. Effekt-Wechsel = Pipeline-Reconfigure, kein Reconnect.
- Library-Wahl (zu dokumentieren, Lizenz + Bundle-Größe prüfen):
  - MVP **Background Blur** zuerst (am robustesten).
  - Background Removal via lokale WASM/JS-Segmentierung (z. B. MediaPipe Selfie
    Segmentation / vergleichbar) als nächster Schritt, nur wenn sauber integrierbar.
  - Lizenz/Größe in `docs/` festhalten; bei Unklarheit Modus nicht anbieten.

## 5. Filesharing (P2P über DataChannel)

- Direkt Peer→Peer, **Server speichert nichts**, Init nur Signaling.
- Optional + nutzerinitiiert. Empfänger **muss** annehmen/ablehnen — **kein**
  Auto-Download.
- Fortschritt pro Transfer: Dateiname · Größe · Sender/Empfänger · Fortschritt ·
  Status (`wartet | läuft | fertig | abgelehnt | fehlgeschlagen`).
- Abbrechen mindestens senderseitig (idealerweise beidseitig via `file_cancel`).
- **Chunking** 16–64 KiB (Vorschlag 16 KiB für breite Kompatibilität).
- **Backpressure:** `bufferedAmount` + `bufferedAmountLowThreshold` (Browser);
  Rust: `data_channel.buffered_amount()` + `on_buffered_amount_low` +
  `set_buffered_amount_low_threshold`. Senden pausieren über Schwelle, bei
  `low`-Event fortsetzen.
- Max Dateigröße initial 100 MB (konfigurierbar). Dateinamenlänge begrenzen.
- **Nur Dateiname, keine Pfade.** Dateiname sanitizen. Keine Auto-Ausführung/Öffnung.

### 5.1 Transport-Kanal

Eigener DataChannel **`file`** pro Peer (zusätzlich zum `chat`), vom Offerer
erzeugt — analog `chat` (`mesh.rs:207`). Begründung: Chunk-Binärframes sauber von
Chat-Text trennen, eigene Backpressure-Schwelle.
- **Control** = JSON über **Text**-Frames (§6).
- **Chunks** = **Binär**-Frames: fixer Header `[16B transfer_id][4B BE seq]` +
  Payload. Empfänger routet per `transfer_id`, ordnet per `seq`. (Binärframes haben
  kein JSON-Envelope — daher der feste Header.)

## 6. Filesharing-Protokoll (über `file`-DataChannel, server-blind)

JSON-Control-Messages (Text-Frames), Tag-Feld konsistent zum Signaling-Stil (`t`,
kebab-case):

```
file-offer    { transfer_id, name, size, mime, from }
file-accept   { transfer_id }
file-reject   { transfer_id, reason }
file-complete { transfer_id }            # Sender signalisiert: alle Chunks raus
file-cancel   { transfer_id }
```
`file-chunk` = **Binär**-Frame (Header s. §5.1), **nicht** JSON (bevorzugt binary).
Fallback base64-über-Text nur, falls ein Endpoint keine Binärframes kann — beide
Clients hier können binär, also binär als Default.

- Kompatibel zwischen Rust/Tauri-Client und Browser-Participant: identisches
  Header-Layout + identische Control-Tags. TS-Mirror + Rust-`serde`-Structs aus
  einer gemeinsamen Spec (in `crates/protocol` als reine Typen ablegbar, auch wenn
  der Transport der DataChannel ist).

## 7. Security Filesharing

- Max file size (Default 100 MB), max parallele Transfers, max eingehende pending
  Offers — alle hart begrenzt, sonst Reject.
- Filename sanitize: Pfadtrenner (`/ \\`), `..`, Steuerzeichen, NUL entfernen;
  Länge cappen (z. B. 255); leerer Name → ablehnen. **Keine** relativen/absoluten
  Pfade übertragen — nur Basename.
- Schreiben **nur** an den vom Nutzer gewählten Speicherort (Tauri Save-Dialog,
  wenn nativ verfügbar) — Empfänger bestätigt Ort.
- Keine Auto-Vorschau für potenziell gefährliche Typen. Keine Auto-Öffnung.
- Kein `innerHTML` für Datei-Metadaten — nur `textContent`.
- Transfer-Metadaten validieren (size ≥ 0 ≤ max, mime/Name-Form). Ungültiger/
  fehlender Chunk, Größenüberschreitung, seq-Lücke → Transfer abbrechen
  (`file-cancel` + UI „fehlgeschlagen").

## 8. Protokoll-/State-Erweiterungen

### Signaling (`crates/protocol/src/lib.rs`)
- Neue Variante **`Video { active }`** in `ClientMsg`/`ServerMsg`, exakt analog zum
  bestehenden `Ptt { active }` (`protocol/lib.rs:52`) — Server relayed sie wie
  `Ptt` (`server/init/src/main.rs:490`). Damit kennt der Roster `video_enabled`
  ohne Inferenz.
- **Backward-Compat-Hinweis:** `ClientMsg`/`ServerMsg` sind intern-getaggte
  serde-Enums → ein **alter** Client kann den neuen `video`-Tag nicht parsen
  (`from_str` schlägt fehl, Server schickt `Error{bad_json}`). Da alle Clients aus
  demselben Crate stammen und gemeinsam deployen, ist das koordinierbar; im Doc
  vermerken. Der Browser-TS-Mirror muss `video` mitführen.
- Filesharing braucht **keine** Signaling-Änderung (läuft über DataChannel).

### Core-State / UiEvent (`apps/companion-core/src/lib.rs`, `mesh.rs`)
- Participant-State `video_enabled: bool` (Default false). `camera_label` nur wenn
  nicht privacy-sensibel — im Zweifel weglassen.
- Neue `UiEvent`/`MeshEvent`-Varianten für Filesharing: `FileOffer`, `FileProgress`,
  `FileComplete`, `FileError/Cancel` (Felder: transfer_id, name, size, peer, status,
  bytes_done). Bestehende Events (`Roster/Chat/Badge/Status`) unverändert.
- `Engine`-Methoden ergänzen: `set_video(bool)` (native: nur State+`ClientMsg::Video`),
  `send_file(peer, path)`, `accept_file(transfer_id, save_path)`,
  `reject_file(transfer_id)`, `cancel_file(transfer_id)`.

## 9. UI (`apps/companion/src/App.tsx`, `style.css`)

- **Kompakte Media-Controls** (eine Leiste): Mic/PTT (bestehend) · Kamera-Toggle ·
  Video-Effekt-Modus (nur Browser/where-available) · Relay-Fallback-Toggle (optional,
  aus TURN-Konzept).
- **Remote-Video-Grid:** Peers mit aktivem Video als `<video>`; ohne Video →
  Avatar/Name/Status. Badge `DIREKT`/`RELAY (TURN)` bleibt (`App.tsx:400`).
- **Filesharing-Panel:** Datei wählen · Empfänger wählen (einzelner Peer; „alle"
  nur wenn sinnvoll = n parallele Transfers) · eingehende Angebote (Annehmen/Ablehnen)
  · Transferliste mit Fortschritt + Status.
- Nicht überladen, keine langen Erklärtexte in der App — Technik gehört in `docs/`.
- Nur `textContent`/Komponenten, kein `innerHTML` (auch für Datei-/Chat-Inhalte).

## 10. Browser-Participant (siehe eigenes Konzept)

- Video **dort zuerst** über `getUserMedia` (natürlicher Boden).
- Background Blur/Removal im Browser deutlich einfacher (Canvas/WASM).
- Filesharing über RTCDataChannel **bit-kompatibel** zum nativen Client halten
  (gleiches Header-Layout + Control-Tags, §5.1/§6).
- **Globales PTT im Browser weiterhin nicht versprechen** — nur Tab-fokussiertes
  PTT (Browser-Sandbox). Bleibt Alleinstellungsmerkmal der nativen App.

## 11. Phasen-Vorschlag (Build-Reihenfolge)

1. **Filesharing nativ + Browser** (DataChannel `file`, Protokoll §6, Backpressure,
   Security §7) — größter Nutzen, beide Clients, kein Video-Pfad nötig.
2. **Video im Browser-Participant** (Transceiver-Preallocation, Kamera-Toggle,
   Remote-Grid, `Video{active}`-Signaling).
3. **Background Blur** (Browser, Canvas).
4. **Background Removal/Replace** (Browser, lokale Segmentierung) — nur wenn
   Lizenz/Größe/Qualität passen.
5. **Nativer Video-Pfad** — separates, großes Vorhaben (Capture+Encoder+Render);
   eigener Spike, erst wenn gewünscht.

## 12. Akzeptanzkriterien

- [ ] Voice/Chat unverändert funktionsfähig (keine Regression).
- [ ] Video optional, Default aus; ohne Kamera kein Videotrack.
- [ ] Browser: Kamera an/aus, Remote-Video sichtbar, `video_enabled` im Roster.
- [ ] Nativer Client: keine Fake-Video-Frames; Grenze dokumentiert; Video-Presence
      sichtbar.
- [ ] Filesharing P2P, Server speichert nichts; Annehmen/Ablehnen Pflicht; Fortschritt
      + Abbrechen.
- [ ] Filename sanitized, Pfade nie übertragen, kein Auto-Open, Größen-/Anzahl-Limits.
- [ ] TURN bleibt optionaler Fallback (unberührt).
- [ ] Keine Secrets im Repo; keine Tokens/PINs persistent.

## 13. Tests/Checks

- Core: Filename-Sanitizer (Unit), Chunk-Header-Parser (Roundtrip), Transfer-State-
  Machine (offer→accept→chunks→complete; reject; cancel; bad-chunk→abort).
- Protokoll: `Video{active}` serde-Roundtrip; `file-*` Control-Roundtrip TS↔Rust
  gegen Beispielmessages.
- Frontend-Build: `cd apps/companion && pnpm build`.
- Rust-Tests: `cargo test -p companion-core`, `cargo test -p init-connection`.
- Manuelle Interop-Matrix: Browser↔Browser, Browser↔nativ (Filesharing beidseitig,
  Video Browser→Browser, Reject, Cancel, Backpressure bei großer Datei, schwache HW
  Effekt-aus).

> Build/Test nur als Vorschlag — Ausführung beim User (Tauri-Build lokal,
> Server-Tests durch Betreiber).

## Geänderte/neu angelegte Dateien (geplant, NICHT umgesetzt)

- **edit** `crates/protocol/src/lib.rs` — `Video{active}`; optional `file-*`-Typen als reine Structs
- **edit** `apps/companion-core/src/lib.rs` — Video-State, File-Engine-Methoden, UiEvents
- **edit** `apps/companion-core/src/mesh.rs` — `file`-DataChannel, Chunk-IO, Backpressure
- **neu** `apps/companion-core/src/file_transfer.rs` — Transfer-State-Machine + Sanitizer
- **edit** `apps/companion/src-tauri/src/main.rs` — Commands `set_video`, `send_file`, `accept/reject/cancel_file`, Save-Dialog
- **edit** `apps/companion/src/App.tsx` + `style.css` — Media-Controls, Remote-Grid, File-Panel
- **edit** `apps/web-participant/**` — Video (getUserMedia + Transceiver), Effekte (Canvas/WASM), Filesharing-DC
- **edit** `README.md`, `docs/ARCHITECTURE.md` — Modi, Grenzen, Library-/Lizenz-Notiz
- **keine** Server-Speicherung; `server/init` nur `Video`-Relay (wie `Ptt`)
