# RDOC SquadLink Lite

Stand-Alone **serverless P2P-Voice-Mesh** für kleine Gruppen — ohne SFU (kein LiveKit),
ohne Account, ohne Aufnahme. Native Audio/Netz in Rust, Tauri-GUI (React). Eigenständig,
außerhalb der RDOC-Suite.

- Audio + Chat laufen **direkt Peer-zu-Peer** (WebRTC, Opus, DTLS-SRTP / DTLS-SCTP).
- Einziger zentraler Dienst: **InitConnection** — reines Signaling (SDP/ICE-Vermittlung,
  Roster, Session-PIN), **kein Media**. TURN-Relay nur optional (opt-in), Default STUN-only.
- Zielgröße: kleine Squads (Warn-Cap 12, Hard-Cap 16).

→ Architektur: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) · Änderungen: [CHANGELOG.md](CHANGELOG.md)

## Download

Windows-Installer (unsigniert, Prototyp): **https://squadlink.raumdock.org/download/**
SmartScreen beim ersten Start: „Weitere Informationen → Trotzdem ausführen".
Die App hat einen **eingebauten Update-Checker** (meldet neue Releases + Changelog).

## Funktionen

- **Session-Brokering:** Host erstellt eine Session → Link + 6-stellige PIN; Mitspieler treten
  konfigurationslos bei. Session bleibt aktiv solange Teilnehmer drin sind (max. 24 h).
- **Push-to-Talk** frei belegbar (jede Taste **oder Maustaste**, RAW-Eingabe), Toggle-Senden,
  **Selbst-Stummschaltung** (Mikro) und **Deafen** (Ton aus).
- **Audio-Aufbereitung** (Experte): Noise Gate, Kompressor, Limiter — je an/aus + konfigurierbar;
  RNNoise-Rauschunterdrückung. **Mikrofon-Selbsttest** (Eigenwiedergabe).
- **Lautstärke** gesamt + pro Teilnehmer; Audio-Geräteauswahl.
- **Session-Verschlüsselung neu** (Key-Rotation) per Knopfdruck, room-weit.
- **Robustes Signaling:** WS-Keepalive + **automatischer Reconnect** mit Backoff; ein
  Signaling-Abriss beendet die Session **nicht** (P2P läuft weiter).
- **Low-Bandwidth-Modus** (≈14 kbps Opus + DTX) für schwache Verbindungen.
- **Netzwerk-Selbsttest:** Senden / Empfangen / STUN / Signaling — je yes/no.
- **Bandbreitenanzeige** (gemessen) oder kleines **Sende-/Empfangs-Funklicht** (rot/grün).
- Verbindungstyp-Badge pro Peer (DIREKT / RELAY).
- Einstellungen in **Einfach / Experte** geteilt; Version in Footer + Startscreen.

## Bauen

GitHub Actions baut die GUI auf einem sauberen Windows-Runner —
Workflow [`.github/workflows/build-companion.yml`](.github/workflows/build-companion.yml):

- **Push/Manuell:** Artefakt **`rdoc-squadlink-lite-windows`** (NSIS-`.exe` + `.msi`).
- **Release:** Tag `squadlink-lite-v*` pushen → veröffentlichter (Pre-)Release mit Installern;
  der Server zieht den neuesten automatisch nach `…/download/`.
- App-Icon wird im CI aus `apps/companion/src/Squad_Link_Lite.png` generiert (`tauri icon`).

Lokaler Dev-Build (braucht Rust + Node + pnpm): `cd apps/companion && pnpm install && pnpm tauri dev`

## Verschlüsselung & Key-Rotation

Nichts verlässt den Rechner unverschlüsselt:

- **Audio:** WebRTC **DTLS-SRTP** (P2P, der Server sieht kein Medium)
- **Chat:** WebRTC DataChannel über **DTLS-SCTP**
- **Signaling:** **TLS / wss** zum InitConnection-Server

**Keys sind pro Session ephemer** — jeder DTLS-Handshake handelt frische SRTP-Keys aus; keine
gemeinsamen, langlebigen Keys zwischen Sessions/Peer-Paaren.

**„Session neu verschlüsseln"** (Knopf in der Leiste / Experte-Menü) löst eine **room-weite**
Rotation aus: alle Teilnehmer handeln neu aus → neue DTLS-SRTP-Keys auf jedem Link. Die
Schlüssel-Generation steht in der Verschlüsselungs-Fußzeile.
Protokoll: `ClientMsg::Rekey` → Server-Broadcast `ServerMsg::Rekey` → `mesh.rekey()`
(glare-aware: pro Paar offert die kleinere user_id neu).

## Sicherheit / Härtung

- **Loopback-Erkennung** parst den URL-Host (kein Substring): `ws://` nur zu
  `localhost`/`127.0.0.1`/`::1`, sonst `wss://` erzwungen (`server_url_ok`, Unit-getestet).
- **Tauri-CSP** strikt (kein `null`, keine Wildcards): self + `squadlink.raumdock.org` (https/wss)
  + GitHub (Update-Check) + IPC.
- **Eingabevalidierung** Rust-seitig für alle Tauri-Commands; DSP-Werte werden an der IPC-Grenze
  normalisiert (finite + clamp).
- **InitConnection:** WS-Frame-Limit (64 KB), Längen-Caps (room/user_id/name/SDP/ICE),
  REST-Body-Limit, **bounded** per-Peer-Channels (Backpressure), **per-IP-Rate-Limits** auf
  `/session` + PIN-Join (echte IP via `ConnectInfo`, X-Forwarded-For nur vom Loopback-Proxy).
- Reflected-XSS auf `/j/:code` escaped + **CSP** auf allen Server-HTML-Seiten; **CORS-Allowlist**
  statt permissive (erweiterbar via `EXTRA_CORS_ORIGINS`).
- **Auth fail-closed:** ohne `ROOM_AUTH_SECRET` startet der Server **nicht** — außer
  `ALLOW_OPEN_AUTH=1` (nur Dev).
- **Dependencies/CI:** Vite 6 / esbuild 0.25; Install mit `--frozen-lockfile`, plus
  `pnpm audit` + `cargo audit`.

## Deployment / Konfiguration (InitConnection)

| Variable | Zweck |
|---|---|
| `ROOM_AUTH_SECRET` | **Pflicht in Prod** — HMAC-Secret für Room-Join-Tokens. Erzeugen: `openssl rand -hex 32`. |
| `ALLOW_OPEN_AUTH` | `1` erlaubt Open-Mode **ohne** Secret (nur Dev). |
| `PORT` | Listen-Port (Default 8080). |
| `TLS_DISABLE` | `1` = plain ws (nur hinter TLS-terminierendem Proxy). Bindet dann `127.0.0.1`. |
| `ALLOW_PLAIN_PUBLIC_BIND` | `1` = plain ws auf `0.0.0.0` binden. **In Docker nötig** (Host-Mapping `127.0.0.1:…` schützt). |
| `PUBLIC_BASE` / `PUBLIC_WS` | Öffentliche URLs für Share-Links + zurückgegebene ws-URL. |
| `TURN_SECRET` / `TURN_URLS` | optionale coturn-Creds (NAT-Relay-Fallback; im Client opt-in). |
| `EXTRA_CORS_ORIGINS` | zusätzliche erlaubte Origins (kommagetrennt). |

Prod läuft hinter dem RDOC-Suite-Caddy auf `squadlink.raumdock.org`
(`deploy/docker-compose.proxy.yml`); `.env` **muss** `ROOM_AUTH_SECRET` setzen (sonst
Fail-Closed-Abbruch). Der Server serviert auch die mehrsprachige Website (EN/DE/IT/ES/FR):
`/`, `/privacy`, `/legal`, `/license` und die Share-Landing `/j/:code`.

## License

© head87x & justcallmedeimos — **PolyForm Noncommercial License 1.0.0** (see [LICENSE](LICENSE)).

Free for any **non-commercial** purpose (private, community, education, research).

**Commercial use requires a separate commercial license.** Commercial use includes selling,
sublicensing, hosting as a paid service, integrating into commercial products, or using the
software in revenue-generating activities.

For commercial licensing inquiries: **commercialusage@raumdock.org**
