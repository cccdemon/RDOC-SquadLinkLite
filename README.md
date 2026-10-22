# subraum — encrypted communication

Stand-Alone **serverless P2P-Voice-Mesh** für kleine Gruppen — ohne SFU (kein LiveKit),
ohne Account, ohne Aufnahme. Native Audio/Netz in Rust, Tauri-GUI (React). Eigenständig,
außerhalb der RDOC-Suite.

- Audio + Chat laufen **direkt Peer-zu-Peer** (WebRTC, Opus, DTLS-SRTP / DTLS-SCTP).
- Einziger zentraler Dienst: **InitConnection** — reines Signaling (SDP/ICE-Vermittlung,
  Roster, Session-PIN), **kein Media**. TURN-Relay nur optional (opt-in), Default STUN-only.
- Zielgröße: kleine Squads (Warn-Cap 12, Hard-Cap 16).

> **Früher „RDOC SquadLink Lite".** Umbenannt in **subraum**, weil bereits eine
> andere Anwendung den Namen „SquadLink" trägt — die Umbenennung schließt
> Verwechslungen aus. Gleiche App, gleiches Team. Im Microsoft Store läuft das
> Listing als **Subraum Communicator**.

→ Architektur: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) · Änderungen: [CHANGELOG.md](CHANGELOG.md)

## Download

**Jetzt Multi-Plattform** — alle Builds + Prüfsummen: **https://subraum.cc/get**

- **🪟 Windows** — signiert im Microsoft Store, sonst unsigniert (SmartScreen: „Weitere Informationen → Trotzdem ausführen").
- **🐧 Linux** — `.deb` / `.rpm` / `.AppImage` (amd64 + arm64) sowie **Flatpak für SteamOS / Steam Deck** und gängige Gaming-Distributionen (Bazzite, ChimeraOS, Nobara, Garuda …). Installation + Voraussetzungen: [docs/LINUX.md](docs/LINUX.md).
- **🍎 macOS** — Apple Silicon (unsigniert: per Rechtsklick → Öffnen starten).
- **🤖 Android** — APK zum Sideloaden.
- **📱 iOS / iPadOS** — technisch fertig, läuft bereits auf dem Gerät; die **App-Store-Veröffentlichung scheitert noch an der Apple-Entwickler-Lizenz** (99 $/Jahr). Unterstütze den Stream, um das möglich zu machen: **[twitch.tv/JustCallMeDeimos](https://twitch.tv/JustCallMeDeimos)**.

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
- **Stream-Deck-Plugin** mit Live-Status auf den Tasten (PTT, Mikro, Deafen,
  Kanäle, Lautstärke, Session) + offene lokale Steuer-Schnittstelle für
  Companion & Co. — siehe [docs/STREAMDECK.md](docs/STREAMDECK.md).
- Einstellungen in **Einfach / Experte** geteilt; Version in Footer + Startscreen.

## Bauen

GitHub Actions baut die GUI auf einem sauberen Windows-Runner —
Workflow [`.github/workflows/build-companion.yml`](.github/workflows/build-companion.yml):

- **Push/Manuell:** Artefakt **`subraum-windows`** (NSIS-`.exe` + `.msi`).
- **Release:** Tag `subraum-v*` pushen → veröffentlichter (Pre-)Release mit Installern;
  der Server zieht den neuesten automatisch nach `…/download/`.
- App-Icon wird im CI aus `apps/companion/src/subraum.png` generiert (`tauri icon`).

Lokaler Dev-Build (braucht Rust + Node + pnpm): `cd apps/companion && pnpm install && pnpm tauri dev`

## Testen

Die komplette Testsuite läuft mit einem Befehl — lokal in Docker oder auf
einem CI-Runner, gleiche Schritte, gleiche Definition
([`e2e/test-all.sh`](e2e/test-all.sh)):

```sh
bash e2e/test-all.sh                # alles, in Docker (baut das Runner-Image beim ersten Mal)
bash e2e/test-all.sh --native       # alles, direkt auf dieser Maschine (Toolchains vorhanden)
ONLY=rust bash e2e/test-all.sh      # eine Stufe: rust | tauri | frontend | streamdeck
```

| Stufe | Was läuft |
| --- | --- |
| `rust` | `cargo clippy -D warnings` + `cargo test` für `crates/protocol`, `server/init`, `apps/companion-core` |
| `tauri` | dasselbe für `apps/companion/src-tauri` (eigener Workspace, braucht webkit2gtk) |
| `frontend` | `tsc` + `vite build`, dann `vitest` (`apps/companion`) |
| `streamdeck` | `node --test` + der Fake-Deck-Harness (`streamdeck-plugin`) |

CI: [`.github/workflows/test.yml`](.github/workflows/test.yml) (GitHub) und
[`.gitlab-ci.yml`](.gitlab-ci.yml) (GitLab-Runner) rufen genau diese Stufen auf.
Der echte Drei-Client-Mesh-Test mit Server (`bash e2e/run.sh`) und der
Desktop-Boot-Test unter Xvfb (`bash e2e/desktop.sh`) bleiben separate
Docker-Läufe.

## Kompatibilität

**Es gibt keine Kompatibilität zwischen subraum-Versionen.** Das Protokoll
trägt eine Versionsnummer (`protocol::PROTOCOL_VERSION`); der Server weist
jede andere Nummer beim Beitritt ab, und ein Mitspieler mit abweichender
Version wird in der Session weder gehört noch kann er chatten. Alle Teilnehmer
aktualisieren gemeinsam. Share-Links (`subraum://`, `/j/<code>`) bleiben
dagegen gültig.

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

**Fingerabdruck pro Teilnehmer:** Für jeden Peer zeigt die Oberfläche einen Fingerabdruck des
PQC-Handshakes. Wer ihn mit dem Gegenüber über einen anderen Kanal vergleicht (Signal-„Safety
Number"-Stil), erkennt einen Mann-in-der-Mitte, auch wenn der Server feindlich wäre.

## Sicherheit / Härtung

- **Loopback-Erkennung** parst den URL-Host (kein Substring): `ws://` nur zu
  `localhost`/`127.0.0.1`/`::1`, sonst `wss://` erzwungen (`server_url_ok`, Unit-getestet).
- **Tauri-CSP** strikt (kein `null`, keine Wildcards): self + `subraum.cc` (https/wss)
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
| `ALLOW_OPEN_AUTH` | `1`/`true` erlaubt Open-Mode **ohne** Secret (nur Dev). |
| `PORT` | Listen-Port (Default 8080). |
| `TLS_DISABLE` | `1`/`true` = plain ws (nur hinter TLS-terminierendem Proxy). Bindet dann `127.0.0.1`. Andere Werte (auch `0`) lassen TLS an. |
| `ALLOW_PLAIN_PUBLIC_BIND` | `1` = plain ws auf `0.0.0.0` binden. **In Docker nötig** (Host-Mapping `127.0.0.1:…` schützt). |
| `PUBLIC_BASE` / `PUBLIC_WS` | Öffentliche URLs für Share-Links + zurückgegebene ws-URL. |
| `TURN_SECRET` / `TURN_URLS` | optionale coturn-Creds (NAT-Relay-Fallback; im Client opt-in). |
| `EXTRA_CORS_ORIGINS` | zusätzliche erlaubte Origins (kommagetrennt). |
| `TLS_CERT` / `TLS_KEY` | Pfade für das selbstsignierte Zertifikat + Key (Default `init-cert.pem` / `init-key.pem` im Arbeitsverzeichnis). Werden beim ersten Start erzeugt und **behalten**, damit der gepinnte Fingerprint stabil bleibt. |
| `TLS_SAN` | zusätzliche SANs für dieses Zertifikat (kommagetrennt; `localhost` und `127.0.0.1` sind immer drin). |
| `DOWNLOADS_DIR` | Installer-Spiegel für `/get` (Default `/srv/downloads/subraum`; gefüllt von `deploy/pull-installer.sh`). |
| `RUST_LOG` | Log-Filter (Default `init_connection=info,tower_http=info`). |

Prod läuft hinter dem RDOC-Suite-Caddy auf `subraum.cc`
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
