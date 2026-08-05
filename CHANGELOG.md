# Changelog

All notable changes to subraum (formerly RDOC SquadLink Lite).
Tags: `subraum-v*` (older releases: `squadlink-lite-v*`).

## Unreleased

### Added
- **Stream-Deck-Integration**: eigenes Elgato-Plugin (`subraum.streamDeckPlugin`,
  Stream Deck ≥ 6.4) mit Live-Status auf den Tasten — Push-to-Talk halten
  (leuchtet beim Senden), Mikro stumm, Deafen, Funkkanal-Direktwahl und
  vor/zurück, Lautstärke ±5 %, Session neu verschlüsseln, Session verlassen.
  Null Konfiguration: Plugin findet die laufende App selbst. Unterbau ist eine
  **lokale Steuer-Schnittstelle** (WebSocket, nur 127.0.0.1, Token rotiert bei
  jedem Start), die auch Bitfocus Companion oder eigene Skripte nutzen können —
  Protokoll in `docs/STREAMDECK.md`. Mikro-stumm gilt auch für Deck-PTT.
- **Funk-Effekt** (Experte, standardmäßig aus): färbt alle **eingehenden**
  Stimmen wie ein Funkgerät — Bandpass plus leichte Sättigung. Vier Regler:
  **FX Tiefen abschneiden** (50–1000 Hz), **FX Höhen abschneiden**
  (1,2–12 kHz), **Saturation** und **FX Destruction** (Dezimierung +
  Quantisierung, das „kaputtes Handfunkgerät"-Kratzen). Wirkt nur lokal beim
  Hörer: Gesendetes bleibt unverändert. Der Mikrofon-Selbsttest läuft durch den
  Effekt mit — so lässt er sich ohne Gegenstelle probehören. Einstellungen
  bleiben gespeichert.

### Fixed
- **Schließen schließt jetzt wirklich.** Das X des Hauptfensters beendet die
  App vollständig — vorher blieb das (unsichtbare oder sichtbare)
  In-Game-Overlay-Fenster übrig und hielt den Prozess samt Audio am Leben.
- **Irreführender Hinweis entfernt.** Die Warnung „Sprach-Schlüssel nicht
  erhalten" empfahl, das TURN-Relay zu aktivieren — das es serverseitig gar
  nicht gibt. Jetzt empfiehlt sie, was wirklich hilft: „Session neu
  verschlüsseln" (baut jede Verbindung neu auf und verteilt den Schlüssel
  erneut), sonst Session-Neubeitritt des Schlüssel-Verwalters (der Teilnehmer
  mit dem ★ in der Verschlüsselungs-Zeile).

## v0.2.1 — 2026-07-30

### Fixed
- **Teilnehmer hörten sich teilweise gar nicht.** Betrifft alle Versionen ab
  v0.1.35 (Einführung der PQC-Sprachverschlüsselung) — wer von v0.1.34 oder
  älter direkt auf v0.2.0 gewechselt ist, hat es beim Umstieg bemerkt.
  Der Raum-Schlüssel wird vom Teilnehmer mit der kleinsten ID verwaltet, und
  diese IDs sind zufällig — wer später beitritt, kann also der neue Verwalter
  werden. Er begann dann bei **Generation 1**, die alle anderen als veraltet
  verwarfen, weil sie längst höher standen. Folge: dieser eine Teilnehmer
  versiegelte mit einem Schlüssel, den niemand hatte, und konnte umgekehrt
  niemanden öffnen — **in beide Richtungen stumm**, bis er den Raum verließ,
  während der restliche Raum normal weiterlief.
  Jetzt melden die Mitglieder dem Verwalter die laufende Generation (versiegelt
  über die paarweise PQC-Session) und er prägt darüber. Ältere Clients ignorieren
  diese Meldung folgenlos, gemischte Räume funktionieren also weiter.

### Added
- **Hinweis, wenn der Sprach-Schlüssel ausbleibt.** Der Schlüssel läuft
  ausschließlich über die direkte Verbindung zum Verwalter. Ist ausgerechnet
  dieser eine Teilnehmer nicht erreichbar (ohne Relay hinter striktem NAT),
  bekommt man keinen Schlüssel, obwohl alle anderen Verbindungen stehen — man
  wird gehört, hört aber selbst nichts. Statt still taub zu bleiben, sagt die App
  das nach 8 Sekunden und empfiehlt, das Relay (TURN) einzuschalten.

## v0.2.0 — 2026-07-28 — Umbenennung: RDOC SquadLink Lite → subraum

- **Warum umbenannt**: es existiert bereits eine andere Anwendung namens
  „SquadLink". Um Verwechslungen auszuschließen, heißt die App jetzt **subraum**.
  Gleiche App, gleiches Team, gleiche Daten — nur der Name ist neu.
- **Neuer Name + neue Domain**: die App heißt **subraum** ("encrypted
  communication"), die Website läuft auf **https://subraum.cc**. Alle
  In-App-URLs, die CSP und die Session-Share-Links zeigen auf die neue Domain.
- **Neue Paket-Identität**: Tauri-Identifier `org.raumdock.subraum`, Flatpak-App-ID
  `org.raumdock.Subraum`, Binary/Crate `subraum`. Wer über den **direkten
  Installer** (NSIS/MSI) installiert hat, bekommt dadurch kein In-Place-Update —
  die neue Version landet neben der alten, die alte kann danach deinstalliert
  werden.
- **Microsoft Store: normales Update.** Die Store-Identität bleibt unverändert
  (`raumdock.org.RDOC-SquadLinkLite`, Store-ID 9N9NR49QFBF4) — das Listing wurde
  nur in **„Subraum Communicator"** umbenannt. Store-Installationen aktualisieren
  also ganz normal auf subraum.
- **Deep-Link-Schema** ist jetzt `subraum://`. **`squadlink://` funktioniert
  weiter** — beide Schemas werden registriert und geparst, identisches Format.
  Bereits verteilte Fleetplanner-Direktlinks bleiben also gültig.
- **Alte Share-Links** (`squadlink.raumdock.org/j/<code>`) funktionieren weiter:
  der alte Host bleibt in der CSP erlaubt, und serverseitig leitet er die Seiten
  auf subraum.cc um, während `/ws` weiterhin echt proxied wird (bestehende
  Installationen haben diese WS-URL fest eingebaut).
- **Release-Tags** heißen jetzt `subraum-v*`.
- **Neues Logo für die Website.** Das Zeichen ist die Topologie selbst: eine
  Oberflächenlinie, darunter vier Peers mit allen sechs Verbindungen, ein Knoten
  durchstößt die Linie. Keine Mitte — es gibt keinen Server im Gespräch. Logo und
  Social-Preview sind in die Server-Binary einkompiliert (`/assets/logo.svg`,
  `/assets/og-image.png`) und müssen nicht mehr von Hand auf den Host kopiert
  werden; `pull-installer.sh` muss sie entsprechend nicht mehr über den
  Verzeichnistausch retten.
- Unverändert: das Wire-Protokoll und das KDF-Label der PQC-Session
  (`rdoc-squadlink-pqc-v1`) — eine Änderung dort wäre ein Protokollbruch und
  würde alte gegen neue Clients inkompatibel machen.

## Angekündigt — 2026-07-26 — Multi-Plattform-Rollout

- **🐧 Linux**, **🍎 macOS** und **🤖 Android** kommen zu **🪟 Windows** dazu —
  Linux inkl. **Flatpak für SteamOS / Steam Deck** und die gängigen
  Gaming-Distributionen (Bazzite, ChimeraOS, Nobara, Garuda …).
- **📱 iOS / iPadOS** ist technisch fertig und läuft bereits auf dem Gerät. Die
  Veröffentlichung im App Store scheitert aktuell an der Apple-Entwickler-Lizenz
  (99 $/Jahr). **Support the stream to make this possible —
  [twitch.tv/JustCallMeDeimos](https://twitch.tv/JustCallMeDeimos).**

## v0.1.36 — 2026-07-26

### Fixed
- **Gelöschte Funkkanäle verschwinden jetzt bei allen** — löscht ein Teilnehmer
  einen (leeren) Kanal, wird er über die Mesh entfernt und *tombstoned*, sodass
  ein späterer Verzeichnis-Abgleich ihn nicht wieder auferstehen lässt. Vorher
  blieb ein gelöschter Kanal bei den anderen Clients bestehen. Ein Kanal, auf
  dem noch jemand ist, wird nicht gelöscht; wer den Kanal neu betritt, hebt das
  Tombstone auf (Neuanlage).

## v0.1.35 — 2026-07-25

### Added
- **Post-Quantum-Sprachverschlüsselung** — die Stimme wird jetzt zusätzlich
  quantensicher verschlüsselt (nicht mehr nur DTLS-SRTP). Ein hybrider
  ML-KEM-768 + X25519 Handshake läuft pro Peer über den (bereits
  DTLS-authentifizierten) DataChannel; Chat und der **Raum-Schlüssel** reisen
  darin versiegelt. Die Sprache selbst wird unter *einem* raumweiten Schlüssel
  versiegelt — einmal versiegelt, an alle gefächert (das Encode-once-Prinzip
  bleibt). So schützt die App gegen „jetzt mitschneiden, später mit einem
  Quantencomputer entschlüsseln".
- **Sichtbarer Voice-Krypto-Status** — die Encryption-Zeile zeigt
  „🛡️ Voice quantensicher #<Generation>" (★ = dieser Client verwaltet den
  Schlüssel) bzw. „aushandeln…", bis der Raum-Schlüssel verteilt ist.

### Security
- Der Raum-Schlüssel wird vom Teilnehmer mit der kleinsten ID erzeugt und
  **bei jedem Beitritt und Verlassen rotiert** (frische Epoche für neue
  Mitglieder, Forward Secrecy gegenüber verlassenden). „Session neu
  verschlüsseln" rotiert jetzt auch den Sprach-Schlüssel. Eine kurze
  Übergangszeit beim Rotieren verhindert Audio-Aussetzer.

## v0.1.34 — 2026-07-24

### Added
- **Funkkanäle werden an alle durchgereicht** — geteiltes Kanal-Verzeichnis
  über die Mesh: ein von irgendeinem Teilnehmer erstellter Kanal wird für alle
  wählbar, auch für später beitretende und für leere Kanäle (Ankündigung beim
  DataChannel-Open + bei jeder Erweiterung, union-merge, transitiv). Vorher sah
  man nur den Kanal, auf den ein Peer gerade getunt war.

### Fixed
- Headless-Engine (`companion-core` bin) kompiliert wieder — fehlender
  `UiEvent::Channel`-Match-Arm (nur die Tauri-App wird von CI gebaut, daher lange
  unbemerkt).
- CI-Build-Gate: postcss auf `^8.5.18` gepinnt (behebt high-Advisory im
  `pnpm audit`).

## v0.1.33 — 2026-07-10

### Added
- **Kanäle bleiben die ganze Session bestehen** — ein erstellter Kanal
  verschwindet nicht mehr, wenn niemand mehr drauf ist; er bleibt wählbar.
- **Kanäle löschbar** — × auf leeren Kanälen (nicht dem aktuellen).
- **Cycle-Hotkeys** — globale Vor/Zurück-Tasten (RAW, auch im Vollbild-Game)
  zum Durchschalten der Session-Kanäle.
- **Kanal-Overlay** — kleines, klick-durchlässiges, immer-oben Fenster über dem
  Spiel; zeigt den aktuellen Kanal, blinkt beim Wechsel. Position (6 Presets),
  Größe (S/M/L) und Ein/Aus konfigurierbar. Kein Game-Prozess-Eingriff.

### Performance
- Overlay wird **lazy** erzeugt (nur wenn aktiv) und bei Aus vollständig
  geschlossen → kein RAM/CPU wenn ungenutzt.
- Channel-Hotkey-Matching wird pro Tastenevent übersprungen, solange keine
  Cycle-Taste belegt ist.

## v0.1.32 — 2026-07-10

### Added
- **Funk-Klick-Lautstärke.** Der lokale Funk-Klick (Earcon) ist jetzt regelbar
  (0–200 %, Schieberegler in den Einstellungen neben dem An/Aus-Schalter). Wird
  gespeichert und bei jedem Verbinden angewendet.

## v0.1.31 — 2026-07-09

### Added
- **Streamermodus.** Toggle in beiden Session-Boxen blendet Link + PIN unkenntlich
  (geblurt), damit sie beim Streamen nicht mitgelesen werden. Kopier-Buttons kopieren
  weiterhin die echten Werte. Einstellung wird gespeichert.
- **Testmode.** Netzwerk-Selbsttest auf dem Startbildschirm mit Gesamturteil (Senden /
  Empfangen / STUN / Signaling) — prüft vor der Session, ob die Grundfunktion läuft.
- **Kanäle (Frequenzen).** Benannte Kanäle im Mesh mit clientseitigem Umschalten; man
  hört nur Peers auf demselben Kanal. Kein Server-Code (reines P2P-Overlay).

## v0.1.27 — 2026-06-11

### Added
- **Push-to-talk now works while a game is focused.** The global PTT listener moved from a
  `WH_KEYBOARD_LL` hook (rdev) to the Windows **Raw Input** API (`RIDEV_INPUTSINK`), which is
  delivered straight off the device stack and keeps firing under a fullscreen / elevated game
  (e.g. Star Citizen) where the low-level hook was blocked by UIPI. Saved key/mouse bindings
  (default `F8`) are unchanged.
- **`squadlink://` deep link + Fleetplanner-Modus.** The app can be configured by a direct link
  carrying the full connection creds (`squadlink://connect?ws=…&room=…&token=…&name=…&uid=…`) — no
  code or PIN entry needed. `uid` sets the stable identity (e.g. the player's Discord name); `name`
  is the display name. Links auto-connect on cold start (drained from Rust on mount) and while
  running (single-instance forward). A hidden toggle bottom-right reveals a manual direct-link
  field for testing. Link format documented in `docs/FLEETPLANNER-DEEPLINK.md`.

### Fixed
- **Switching audio device mid-session no longer risks going silent.** The new capture/playback
  stream is now built and started **before** the old one is dropped; if the new device fails
  (unsupported format, unplugged), the current device keeps running instead of leaving you muted.

### Packaging (Microsoft Store)
- **MSIX/Store build support.** The app detects at runtime whether it runs inside an MSIX package:
  it then skips the registry-based deep-link registration (the package manifest declares the
  `squadlink` protocol) and hides the self-update prompt (the Store handles updates). Added
  `apps/companion/msix/AppxManifest.xml`, a one-command `pack.ps1` (build → assets → makeappx →
  optional self-sign), and packaging/submission docs under `docs/`.
- **MSI now writes a proper "Add/Remove Programs" entry** (Publisher "Raumdock" + app name +
  version) so Microsoft Store package validation can identify the install.
- **Open the download page via `ShellExecuteW` instead of shelling out to `cmd.exe`** — removes
  the cmd.exe reference that tripped the Store's optional "blocked executable" check (WACK).

## v0.1.26 — 2026-06-09

### Changed
- **Audio device selection now takes effect immediately** — picking a different microphone or
  output switches the live capture/playback stream without a reconnect (previously the choice was
  read only when (re)connecting, so changing it mid-session appeared to do nothing). The
  encode/mixer resamplers retune to the new device's sample rate.

### Fixed
- **Mic self-check ("Eigenwiedergabe") is now a local loopback only.** While testing, the
  processed mic is routed to your own playback and is no longer encoded/sent to peers, even
  if PTT is held — the room no longer hears your test.
- **Leaving and re-joining the same session restored two-way audio.** On a rejoin the existing
  peer (the glare offerer) renegotiated on its old, dead PeerConnection — SDP swapped but ICE
  never restarted, so no media flowed in either direction. The offerer now tears down and
  rebuilds a fresh PC (restarting ICE), mirroring the rekey handshake.

## v0.1.25 — 2026-06-08

### Fixed
- Update banner now renders the changelog as clean plain text (no raw Markdown markers).

## v0.1.24 — 2026-06-08

### Added
- Version shown in the footer (both screens) and on the start screen.

## v0.1.23 — 2026-06-08

### Fixed
- A joiner can now re-share the **exact same session** (same code + same PIN) — the in-session
  "Session teilen" box is populated from the joined code/PIN, not just for the host.

## v0.1.21 — 2026-06-08

### Added
- The netbar "Session neu verschlüsseln" button can be shown/hidden via the Experte menu.

## v0.1.20 — 2026-06-08

### Added
- **Bandwidth (kbps) display can be hidden** (Experte → Bandbreiten-Anzeige). When hidden,
  the netbar shows a small radio activity light instead: red = sending, green = receiving,
  split red/green = both at once.

## v0.1.19 — 2026-06-08

### Changed
- **Settings split into Einfach / Experte tabs.** Simple = Mikrofon, Ausgabe, Push-to-Talk,
  Mikrofon-Test. Expert = Session neu verschlüsseln, TURN-Relay-Fallback, Low-Bandwidth,
  Netzwerk-Selbsttest, Audio-Aufbereitung.

## v0.1.18 — 2026-06-08

### Fixed
- **More crackle, root cause #2**: the playback mixer used a fixed `sleep(20ms)` clock,
  which is ~31ms on Windows (timer granularity) → the playback ring drained → underrun
  crackle. The mixer is now demand-driven (keeps ~60ms buffered, independent of sleep
  precision). The mix sum is also soft-limited (tanh) so several simultaneous speakers
  can't hard-clip.

## v0.1.17 — 2026-06-08

### Added
- **Network self-check** (gear): tests the WebRTC data path via a local two-PeerConnection
  DataChannel echo and reports — Signaling server reachable, Kann senden, Kann empfangen,
  Internet/STUN (server-reflexive candidate) — each yes/no.

### Fixed
- Chat is now cleared when starting/joining a new session.

## v0.1.16 — 2026-06-08

### Changed
- Renamed the rekey button to "Session neu verschlüsseln".

## v0.1.15 — 2026-06-08

### Fixed
- **Update checker never fired**: it trusted the REST `/releases` order (`[0]`), which is
  wrong for force-pushed tags (returned v0.1.9). Now picks the highest semver itself.
- **Settings menu couldn't be closed** when the long panel overflowed the window. The
  settings are now a modal overlay with its own scroll — close via × or by clicking the
  backdrop, independent of page scroll.

### Changed
- **TURN relay fallback is now OFF by default** (opt-in), matching the serverless ethos —
  media never traverses a relay unless explicitly enabled. (Currently moot anyway: prod is
  STUN-only, no coturn deployed.)

## v0.1.14 — 2026-06-08

### Added
- **Automatic signaling reconnect** with backoff (2→4→…→30 s). On loss the UI shows
  "Signaling verloren — automatischer Reconnect läuft…" (P2P audio keeps running); the
  button is now "Jetzt wiederverbinden" for an immediate retry. Re-join keeps the mesh.

## v0.1.13 — 2026-06-08

### Fixed
- **Signaling dropping while idle**: the WebSocket had no heartbeat, so after the
  initial join/offer/ICE burst an idle connection was reaped by proxy/NAT idle
  timeouts. The client now sends a Ping every 25&nbsp;s (server auto-replies Pong),
  keeping the link open and detecting dead connections promptly. (Resume button stays
  as a fallback.)

## v0.1.12 — 2026-06-08

### Fixed
- **Settings panel could not be closed** when opened before a session: the long panel
  pushed the gear button off-screen. The panel now scrolls (max 60vh) and has a sticky
  header with an explicit × close button.

## Website i18n — 2026-06-08

- The public website (`/`, `/privacy`, `/legal`, `/license`, `/j/:code`) is now available in
  **EN / DE / IT / ES / FR** with a language switcher; language is picked from `?lang=` then
  the `Accept-Language` header (default English). Server-side only — no app release.

## v0.1.11 — 2026-06-08

### Security
- **Reflected XSS on /j/:code fixed**: the share code is now length-capped + HTML-escaped;
  added a strict CSP `<meta>` to all server-rendered pages.
- **CORS restricted** to known origins (own domain + Tauri webview + dev), extendable via
  `EXTRA_CORS_ORIGINS` — no more `CorsLayer::permissive()`.
- **Rate-limit IP**: trust `X-Forwarded-For` only when the direct peer is the loopback
  proxy; otherwise use the real socket IP (via `ConnectInfo`) — no XFF spoofing.
- **Plain-ws bind hardened**: with `TLS_DISABLE=1` the server binds `127.0.0.1` only
  unless `ALLOW_PLAIN_PUBLIC_BIND=1`.
- **DSP IPC validation**: `set_dsp` rejects NaN/inf and clamps all fields at the boundary.

### Added
- **Low-bandwidth mode** (gear): drops Opus to ~14 kbps + app-level DTX (silence sends no
  packets). Big win in a full mesh (upload ≈ bitrate × peers). Netbar shows 🐢 when active.
- **TURN relay-fallback toggle** (gear): off = direct/STUN only, never via a relay
  (`EngineConfig.relay_enabled`, default on).

## v0.1.10 — 2026-06-08

### Added
- **Update checker**: on launch the app compares the running version against the
  newest GitHub release (prereleases included) and, if newer, shows a banner with
  the **changelog** + a "Herunterladen" button (opens the download page). Dismissable.

## v0.1.9 — 2026-06-08

### Fixed
- **Occasional audio crackle**: the capture-path compressor's makeup gain could
  clip on the final clamp. Added a smooth peak **limiter** (instant attack, 50 ms
  release) after the compressor and lowered default makeup 1.8→1.4 — no more clip.

### Added
- **Configurable audio chain in the gear menu**: Noise Gate, Compressor (threshold/
  ratio/makeup) and Limiter (ceiling) — each toggleable + adjustable, persisted,
  pushed live (`DspConfig`, `set_dsp`). All on by default.
- **Mic self-check** (gear menu): local monitor playback of your own (processed) mic.
- **Disconnect / "Verlassen"** button → returns to the create/join screen. Cleanly
  stops the engine **and** the audio threads (shutdown flag) so a later reconnect
  doesn't stack duplicate capture/playback rigs.

## v0.1.8 — 2026-06-08

### Fixed
- **Signaling drop no longer ends the session.** The WS signaling link is now
  decoupled from the P2P mesh: if it drops (e.g. server restart), audio/chat keep
  running and the UI shows a "Signaling getrennt" banner instead of going
  disconnected. Engine keeps the mesh alive via an internal uplink channel.

### Added
- **"Session wiederaufnehmen"** button — reconnects signaling + re-joins the room
  without tearing down the live mesh (`reconnect_session` / `Cmd::Reconnect`,
  `UiEvent::Signaling`).
- **Self-mute mic** (🎙️) — stop sending while still hearing everyone (gates PTT).
- **Deafen / Ton aus** (🔊) — mute all output without losing the volume value.
- **Explicit toggle-transmit button** next to push-to-talk.

## v0.1.7 — 2026-06-08

### Fixed
- **Glare-aware key rotation:** `mesh.rekey()` no longer has both peers tear down
  and re-offer independently (which could race and leave an answerer stuck). Per
  pair only the smaller user_id rebuilds + re-offers; the larger side lets
  `on_offer` swap in the new PC. Added a two-mesh integration test (real ICE/DTLS
  over a mock relay) proving both links reconnect into fresh PeerConnections.

## v0.1.6 — 2026-06-08

### Added
- **On-demand session key rotation.** Button "🔑 Key rotieren" triggers a room-wide
  DTLS-SRTP re-handshake (new keys on every link). Protocol `ClientMsg::Rekey` →
  server broadcast `ServerMsg::Rekey` → `mesh.rekey()`. UI shows the current key
  generation + last-rotation time in the encryption footer (`UiEvent::Rekeyed`).

## v0.1.5 — 2026-06-08

### Security
- **Loopback detection** now parses the URL host instead of substring-matching
  (`ws://` only to `localhost`/`127.0.0.1`/`::1`); added `signaling::server_url_ok`
  + unit tests (incl. `ws://evil.example/127.0.0.1`).
- **Tauri CSP** set to a strict policy (was `null`): self + `squadlink.raumdock.org`
  (https/wss) + IPC, no wildcards.
- **Tauri command input validation** (server URL, room/user_id/name/token/cert_sha256,
  chat length, volume clamp, PTT code) with clean `Result` errors.
- **InitConnection hardening:** 64 KB WS frame cap, length caps on room/user_id/name/SDP/ICE,
  REST body limit, bounded per-peer channels (backpressure), per-IP rate limits on
  `/session` and the PIN join (on top of per-code `MAX_ATTEMPTS`).
- **Auth fail-closed:** missing `ROOM_AUTH_SECRET` aborts startup unless `ALLOW_OPEN_AUTH=1`
  (dev only). Production now runs in HMAC mode.
- **Dependencies/CI:** Vite 6 + esbuild 0.25 (override); CI uses `--frozen-lockfile` and runs
  `pnpm audit` + `cargo audit`.

## v0.1.4 — 2026-06-08

### Added
- Public web surface served by InitConnection: `/` (what-is + links to raumdock.org,
  Fleetmanager, GitHub), `/privacy`, `/legal`, `/license`.
- **PolyForm Noncommercial License 1.0.0** (`LICENSE`); authors head87x & justcallmedeimos;
  commercial-use clause + contact `commercialusage@raumdock.org`.
- App icon + in-app logo generated from `Squad_Link_Lite.png` (CI `tauri icon`).

### Changed
- Repository renamed to `cccdemon/RDOC-SquadLinkLite` (GITHUB_URL + installer pull updated).

## v0.1.3 — 2026-06-08

### Added
- **Configurable RAW push-to-talk** (any key or mouse button via `rdev`), rebind via the
  gear menu; binding persisted.
- **Live bandwidth**: real WebRTC transport-stats polling → measured up/down kbps + peer count.
- **Audio compressor** in the capture path (RNNoise noise-suppression already on by default).

### Changed
- Volume sliders are 0–100 % (100 = unity), no longer 0–200.

## v0.1.2 — 2026-06-07

### Added
- Master + per-participant output volume; audio device selection behind a gear icon.
- In-session share panel (code + link + PIN stay visible to the host).
- Encryption footer.

### Changed
- Session-only UI (removed Server/Serverless tabs); chat shows display names, not raw ids.

### Fixed
- Session persistence: a session now lives while its room has members (5-min grace after
  empty, 24 h hard cap), instead of a fixed 12 h TTL from creation.
