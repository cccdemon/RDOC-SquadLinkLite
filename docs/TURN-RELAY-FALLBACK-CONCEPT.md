# Konzept — Optionaler TURN/Relay-Fallback (App-Toggle)

> Status: **Konzept** (2026-06-08). Noch nicht implementiert. Betrifft die native
> Tauri-App (`apps/companion`) + Core (`apps/companion-core`). Server unverändert.
> Erweitert den bestehenden Prompt um Punkte 8–15.

Ziel: TURN bleibt optional, nutzergesteuert per Toggle, lokal persistiert,
Default P2P-first. Der Server darf TURN-Creds anbieten — der **Client** entscheidet,
ob er sie nutzt.

## Ist-Stand (Belege)

- `EngineConfig` ohne Relay-Flag — `apps/companion-core/src/lib.rs:64`.
- TURN wird aktuell **immer** genutzt, sobald der Server sie schickt:
  `ServerMsg::Turn(t) => { mesh.add_turn(t.urls, t.username, t.credential); }`
  — `apps/companion-core/src/lib.rs:305`.
- STUN-Default ist immer da (`stun:stun.l.google.com:19302`,
  `apps/companion-core/src/mesh.rs:97`) → P2P ohne TURN funktioniert.
- Tauri `connect`-Command — `apps/companion/src-tauri/src/main.rs:95-145`.
- Frontend `connect`-Invoke — `apps/companion/src/App.tsx:194-203`.
- Bestehendes localStorage-Pattern: `sa.form`, `sa.audio`, `sa.ptt`
  (`App.tsx:43,63,77`).
- Peer-Badge `DIREKT`/`RELAY (TURN)` existiert bereits — `App.tsx:400`,
  Quelle `mesh.rs:283-296`.

> ⚠️ **Verhaltensänderung:** Heute ist TURN faktisch *an*, wenn der Server Creds
> schickt. Neuer Default = *aus*. Praktisch betrifft das nur den Heim-VM-Deploy
> (coturn aktiv); die Front-Door (`subraum.cc`) ist STUN-only und
> schickt kein `Turn` → dort keine Änderung.

## 8. Doku: Corporate-/restriktive-Netzwerk-Nutzung

In **README.md**, **docs/ARCHITECTURE.md** (§8b TURN), **deploy/README.md**
ergänzen:

- Private/Community-Nutzung: im Normalfall P2P-first **ohne** TURN.
- Corporate-, Hotel-, Uni-, Mobilfunk-, CGNAT- oder stark restriktive Netze:
  brauchen ggf. TURN.
- TURN = Compatibility-**Relay**, **kein** SFU/Voice-Server. Leitet nur
  verschlüsselte Bytes weiter.
- TURN verbessert Erreichbarkeit, kostet aber Serverbandbreite.
- Kernsatz (EN, wörtlich übernehmen):
  > "Corporate or restrictive network use may require TURN. TURN remains optional
  > and is only used as an encrypted relay fallback when direct P2P connectivity
  > fails."

## 9. TURN-Konfiguration in der App

Anforderung:

- TURN bleibt optional.
- Nutzer aktiviert/deaktiviert per Toggle.
- Einstellung lokal persistiert.
- Button-Text: „Relay-Fallback aktivieren" / „Relay-Fallback deaktivieren".
- Default: **aus** (P2P-first).
- Aktiviert → Client darf vom Server angebotene TURN-Creds nutzen.
- Deaktiviert → Client ignoriert `ServerMsg::Turn`, nutzt nur host/srflx (STUN).
- P2P ohne TURN funktioniert weiter (STUN-Default bleibt unangetastet).

Betroffene Dateien (geplant):

| Datei | Änderung |
|---|---|
| `apps/companion-core/src/lib.rs` | `EngineConfig.relay_enabled: bool`; Turn-Gate |
| `apps/companion/src-tauri/src/main.rs` | `connect`-Param `relay_enabled`, in EngineConfig durchreichen |
| `apps/companion/src/App.tsx` | Toggle-UI, localStorage, `relayEnabled` im invoke |
| `apps/companion-core/src/mesh.rs` | keine Pflichtänderung (`add_turn` bleibt) |

## 10. Persistente App-Einstellung

- Key passend zum Pattern: `localStorage.setItem("sa.relayFallback", "true"|"false")`.
- Laden beim App-Start via `useState`-Initializer (wie `sa.audio` `App.tsx:63`):
  ```ts
  const [relayFallback, setRelayFallback] = useState<boolean>(() => {
    try { return localStorage.getItem("sa.relayFallback") === "true"; }
    catch { return false; }
  });
  ```
- Schreiben bei jeder Umschaltung.
- **Keine** Tokens/PINs/Session-Secrets persistieren (unverändert).

## 11. UI/UX für den Toggle

- Platz: Audio-/Settings- oder ein Connection-/Network-Bereich der Connect-Card
  (`App.tsx:287-345`, vor dem Verbinden sichtbar).
- Optional auch während Verbindung sichtbar; Änderung wirkt erst beim **nächsten**
  Connect (Live-Umschaltung zu aufwendig — Engine baut ICE-Server bei `start`).
- Wenn verbunden + geändert: kurzer Hinweis „Wird beim nächsten Verbinden aktiv."
- Texte kurz, keine Erklärblöcke:
  - Label: „Relay-Fallback (TURN)"
  - Status aus: „Aus: direktes P2P bevorzugt"
  - Status an: „An: hilft bei Corporate/Hotel/Uni-Netzwerken"
- Peer-Badges unverändert: `DIREKT` / `RELAY (TURN)` (`App.tsx:400`).

## 12. Core-Verhalten

- `EngineConfig` um `relay_enabled: bool` erweitern (`lib.rs:64`).
- Turn-Gate (`lib.rs:305`):
  ```rust
  ServerMsg::Turn(t) => {
      if relay_enabled { mesh.add_turn(t.urls, t.username, t.credential); }
      // sonst: ignorieren → nur host/srflx via STUN
  }
  ```
  (`relay_enabled` vor dem move von `cfg` in lokale `let` kopieren.)
- Tauri `connect` reicht `relay_enabled` vom Frontend an `EngineConfig` durch
  (`main.rs:95-105` Param + `:143` Konstruktion). Tauri mappt JS `relayEnabled`
  → Rust `relay_enabled` (gleiches camel→snake-Pattern wie `userId`/`certSha256`).
- **Keine Serveränderung.** Server bietet TURN weiter optional an; Client
  entscheidet. Default `relay_enabled=false`.

## 13. Betriebsmodi (Doku)

1. **P2P-first ohne Relay** — Default. Minimaler Serveranteil, direkte
   WebRTC-Verbindungen (host/srflx via STUN). Kann in restriktiven Netzen scheitern.
2. **Optionaler Relay-Fallback** — Nutzer aktiviert TURN in der App. TURN nur
   genutzt, wenn ICE eine Relay-Route braucht. Audio bleibt DTLS-SRTP verschlüsselt
   (TURN sieht nur verschlüsselte Bytes). Hilft bei Corporate/Hotel/Uni/Mobilfunk/CGNAT.
3. **Admin/Deployment mit TURN verfügbar** — Betreiber stellt coturn bereit
   (`deploy/`, `use-auth-secret`, ephemere Creds). App-Nutzer entscheiden optional
   über Relay-Fallback. TURN **niemals** als Open Relay betreiben.

## 14. Akzeptanzkriterien (zusätzlich)

- [ ] Persistente TURN/Relay-Fallback-Einstellung (`sa.relayFallback`).
- [ ] Per Toggle/Button an- und abschaltbar.
- [ ] Default bleibt P2P-first (aus).
- [ ] TURN bleibt optional.
- [ ] Relay aus → Client ignoriert `ServerMsg::Turn`.
- [ ] Relay an → Client nutzt angebotene TURN-Creds.
- [ ] Doku nennt explizit: Corporate/restriktive Netze brauchen ggf. TURN.
- [ ] Keine Session-Tokens/PINs persistent gespeichert.
- [ ] Bestehende Tauri-App baut weiter; P2P ohne TURN funktioniert weiter.

## 15. Tests/Checks

- Core-Test `relay_enabled=false`: `ServerMsg::Turn` führt **nicht** zu
  `add_turn` (ICE-Server-Liste bleibt nur STUN). Testbar, indem die Turn-Gate-Logik
  in eine reine Funktion `should_add_turn(relay_enabled) -> bool` o.ä. faktorisiert
  wird (Mesh-Internals sind sonst async/privat).
- Core-Test `relay_enabled=true`: Turn-Gate ⇒ `add_turn` aufgerufen / ICE-Server
  enthält die TURN-URL (sofern `Mesh` einen Lese-Zugriff auf `ice_servers` bekommt
  oder die Gate-Funktion getestet wird).
- Frontend-Build: `cd apps/companion && pnpm build`.
- Rust-Tests: `cargo test -p companion-core` und `cargo test -p init-connection`.

> Build/Test laufen lokal nur als Vorschlag — Ausführung beim User
> (Tauri-Build lokal, Server-Tests durch Betreiber).

## Geänderte/neu angelegte Dateien (geplant, NICHT umgesetzt)

- **edit** `apps/companion-core/src/lib.rs` — `EngineConfig.relay_enabled`, Turn-Gate
- **edit** `apps/companion/src-tauri/src/main.rs` — `connect`-Param + Durchreichen
- **edit** `apps/companion/src/App.tsx` — Toggle, localStorage `sa.relayFallback`, invoke-Param
- **edit** `README.md`, `docs/ARCHITECTURE.md`, `deploy/README.md` — Netz-Hinweis + 3 Modi
- **optional neu** Core-Test für die Turn-Gate-Funktion
- **keine** Serveränderung (`server/init` bleibt)
