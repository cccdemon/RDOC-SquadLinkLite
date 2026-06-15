# Konzept — Browser Participant für RDOC SquadLink Lite

> Status: **Konzept** (2026-06-08). Noch nicht implementiert. Tauri-App und
> Rust-Core bleiben unberührt; `crates/protocol/src/lib.rs` ist Source-of-Truth.

Lightweight Browser-Teilnahme (Voice + Chat) als **zweiter, separater** Client
neben der nativen Tauri-App. Die Tauri-App bleibt der native Client mit globalem
PTT; der Browser-Client ist die konfigurationslose Web-Teilnahme.

## 1. Verdict: kompatibel

Der native Client serialisiert Signaling so, dass Browser-WebRTC es **nativ**
versteht. Belege aus dem Ist-Code:

| Punkt | Rust-Stand (Beweis) | Browser-Seite | Match |
|---|---|---|---|
| Wire-Tag | `t`, kebab-case (`crates/protocol/src/lib.rs:37`) | gleiche JSON-Tags | ✅ |
| Offer/Answer | `sdp` = roher SDP-String (`apps/companion-core/src/mesh.rs:212,223`) | `setRemoteDescription({type,sdp})` | ✅ |
| **ICE** | `candidate` = `JSON.stringify(RTCIceCandidateInit)` (`mesh.rs:132-134`) | `JSON.stringify(cand.toJSON())` bzw. `new RTCIceCandidate(JSON.parse(s))` | ✅ camelCase passt (`candidate,sdpMid,sdpMLineIndex,usernameFragment`) |
| **Glare** | kleinere `user_id` offert, lexikografisch (`mesh.rs:198`, `my_id < peer`) | gleiches `<` auf ASCII-IDs | ✅ wenn user_id ASCII |
| DataChannel | Offerer erzeugt Label `"chat"`, in-band/nicht-negotiated (`mesh.rs:207`); Answerer `on_data_channel` | Offerer `createDataChannel("chat")`, Answerer `ondatachannel` | ✅ |
| Chat-Payload | `ChatMsg{text, ts:u64-secs}` JSON über DC (`protocol/lib.rs:22`) | `JSON.stringify({text, ts:Math.floor(Date.now()/1000)})` | ✅ |
| Audio | 1× Opus-m-line, sendrecv (`mesh.rs:118`) | `addTrack(mic)` Opus | ✅ (Quality-Caveats §10) |
| TURN | `ServerMsg::Turn{urls,username,credential,ttl}` nach Roster (`server/init/src/main.rs:474`) | `RTCIceServer{urls,username,credential}` | ✅ |
| STUN-Default | `stun:stun.l.google.com:19302` (`mesh.rs:97`) | gleicher Default | ✅ |
| Transport | Front-Door `wss://squadlink.raumdock.org/ws` = echtes LE-Cert via Caddy (`deploy/docker-compose.proxy.yml`) | Browser-WSS, **kein Pin** | ✅ |

**Einziger echter Bruch:** Der Self-signed-Pin-Pfad (Heim-VM `:8080`,
`CERT_SHA256`, `signaling.rs`) ist **Rust-only** — der Browser kann ein
self-signed Cert nicht via SHA-256 pinnen. Der Browser MUSS über die CA-valide
Front-Door (oder localhost `ws` im Dev) gehen. Kein Code-Fix nötig, reine
Deploy-Wahl. Siehe §10.

## 2. Neuer Client — Struktur

Separater Vite+TS-Client, statisch baubar, getrennt von der Tauri-`apps/companion`:

```
apps/web-participant/
  index.html              # strikte CSP <meta>, ein #app mount
  package.json            # vite, typescript, optional vitest
  tsconfig.json
  vite.config.ts          # base: '/web/', build.outDir dist
  src/
    main.ts               # bootstrap, URL ?code= prefill
    signaling.ts          # WS, ClientMsg/ServerMsg typed (1:1 zu protocol/lib.rs)
    types.ts              # TS-Mirror von protocol (handgepflegt, klein)
    mesh.ts               # RTCPeerConnection pro Peer, glare, ICE-buffer, DC, track
    audio.ts              # getUserMedia, PTT (track.enabled), remote <audio> sinks
    ui.ts                 # textContent-only DOM, Zustände, Roster, Chat
    session.ts            # POST /session/:code/join {pin} → {room,token,ws}
  README.md               # lokaler Start + Interop-Checkliste
```

Keine Tauri-Berührung, keine Rust-Core-Änderung. `protocol/lib.rs` bleibt
Source-of-Truth; `types.ts` spiegelt es manuell (klein genug, kein Codegen).

## 3. Join-Flow (Browser)

1. `?code=abc123` aus URL vorausgefüllt; PIN-Feld separat (PIN nie im Link).
2. Mic **zuerst**: `getUserMedia({audio:true})` — vor dem Offer, sonst fehlt die
   Audio-m-line.
3. `POST {ws-origin}/session/{code}/join` Body `{pin}` → `{room, token, ws}`.
   Fehlerfälle: 404 not_found / 403 bad_pin / 429 locked → klare UI-Meldung.
4. WS zu `ws` öffnen → `ClientMsg::Join{room,user_id,name,token}`.
5. `Roster` → für jeden Peer PC anlegen; wenn `myId < peerId` → offer.
   `PeerJoined` analog. `PeerLeft` → PC schließen.

`user_id`: 8 Zeichen base36, random, in `sessionStorage` (kurz + stabil pro Tab,
ASCII → Glare-sicher). Token/PIN **nur in-memory**, nie persistiert.

## 4. Mesh-Interop-Regeln (exakt einhalten)

- **Glare deterministisch** → kein "perfect negotiation"/Rollback nötig. Nur die
  kleinere ID offert; die größere wartet auf den Offer und sendet nie ein eigenes.
- **ICE buffern**: Remote-ICE erst nach `setRemoteDescription` per
  `addIceCandidate`; vorher in Array puffern (spiegelt Rust `pending_ice`/`flush_ice`).
- **End-of-candidates NICHT senden**: Rust sendet kein null-Candidate
  (`mesh.rs:131 if let Some`). Browser: `if(!event.candidate) return;`.
- **DataChannel**: nur der Offerer `createDataChannel("chat")` (default,
  `negotiated:false`). Answerer rein über `ondatachannel`. Genau ein "chat".
- **Audio**: `addTrack` vor `createOffer`. Remote: `pc.ontrack` → MediaStream an
  ein verstecktes `<audio autoplay>` pro Peer.
- **TURN-Race**: `Turn` kommt nach `Roster`. Ein gemeinsames
  `iceServers=[stun]`-Array halten; bei `Turn` ergänzen und auf bereits erzeugte
  PCs `pc.setConfiguration({iceServers})` anwenden. Bei sehr frühem Roster die
  PC-Erzeugung um einen Microtask defern (`await Promise.resolve()` nach Join,
  dann Roster verarbeiten).

## 5. Server-Integration — minimal

- `apps/web-participant/dist` statisch über Caddy unter `/web/` ausliefern
  (neuer `handle_path /web*`-Block neben `/download`). Vite `base:'/web/'`.
- Landing `landing()` in `server/init/src/main.rs:216-221` um einen Button
  erweitern: "**Im Browser teilnehmen**" → `{base}/web/?code={code}` (Code
  vorausgefüllt, PIN bleibt manuell). Download-Pfad bleibt. Eine Zeile HTML,
  `textContent`-sicher (Code ist `[a-z0-9]`).
- Kein neuer Endpoint nötig — `/session/:code/join` existiert. CORS ist
  `permissive` (`main.rs:125`); für Browser ok, **nicht** weiter aufweiten.

## 6. Security

- Token/PIN nur in-memory (Closure/Modulvariable), kein localStorage; user_id in
  sessionStorage (nicht sensibel).
- DOM nur `textContent`/`createElement` — **kein** `innerHTML`. Chat-Text nie als HTML.
- CSP (Meta im `index.html`, später als Caddy-Header verschärfbar):
  `default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self' https://squadlink.raumdock.org wss://squadlink.raumdock.org; media-src 'self' blob:; img-src 'self' data:`
- Validierung: Code `^[a-z0-9]{4,12}$`, PIN `^\d{6}$`, Name length-cap + trim —
  als UX, nicht als alleinige Sicherheit (Server bleibt autoritativ:
  PIN-Rate-Limit, Room-Token-HMAC).
- Keine Secrets im Repo; ws-URL kommt aus der Join-Response, nicht hardcoded.

## 7. PTT / Audio-UX (nur Tab-fokussiert)

- PTT = `micTrack.enabled = true/false` + `ClientMsg::Ptt{active}` ans Roster.
- Hold-Key (Space, frei wählbar) **nur bei Tab-Fokus** via `keydown/keyup`; plus
  Toggle-Button (Mute/Transmit).
- Zustände sichtbar: `disconnected / connecting / connected / muted / transmitting`.
- UI sagt **explizit**: "Push-to-Talk wirkt nur, wenn dieses Browser-Tab im
  Vordergrund ist." Kein globales PTT behauptet (bleibt Alleinstellungsmerkmal
  der Tauri-App via `rdev`).

## 8. TURN / STUN

- TURN-Creds aus `ServerMsg::Turn` nutzen, falls der Server sie liefert (er tut es
  nur, wenn `TurnConfig::from_env` gesetzt ist).
- Default STUN = `stun:stun.l.google.com:19302` (identisch zum Rust-Code).
- Bei `iceConnectionState=failed` klar melden + Hinweis "hinter striktem NAT, TURN
  nötig". Kein stiller Fehler.

## 9. Build/Dev

`package.json` scripts: `dev` (vite), `build` (`tsc && vite build`), `preview`,
optional `test` (vitest).

Lokaler Stack:

```
# 1) Init signaling (plain ws, loopback dev)
cd server/init && TLS_DISABLE=1 ROOM_AUTH_SECRET=dev cargo run

# 2) Browser client
cd apps/web-participant && pnpm install && pnpm dev   # http://localhost:5173/web/

# 3) (Interop) nativer Client
cd apps/companion-core && ROOM=op1 USER_ID=rustA NAME=Alice cargo run
```

Browser-Dev gegen `ws://localhost:8080/ws` (localhost-Ausnahme erlaubt unsicheren
WS + getUserMedia). Monorepo-Eingriff klein: nur neuer `apps/web-participant` +
ein Caddy-Block + ein Landing-Button.

## 10. Tests / Interop-Checkliste

TS: `tsc`-Build muss grün sein. Optional vitest: ein `ServerMsg`-Beispiel je
Variante parsen + `RTCIceCandidateInit`-Roundtrip gegen ein echtes Rust-emittiertes
Sample.

Manuelle Interop-Matrix:

- [ ] Browser joint Tauri-gehostete Session (Code+PIN)
- [ ] Tauri joint Session mit Browser-Teilnehmer
- [ ] Audio Browser → Tauri (Rust decodiert Opus-RTP)
- [ ] Audio Tauri → Browser (`<audio>` spielt)
- [ ] Chat bidirektional über "chat"-DC
- [ ] Roster + Speaking/PTT-Dots korrekt
- [ ] Peer leave/rejoin (PC-Cleanup, supersede same user_id)
- [ ] Badge DIREKT vs RELAY (TURN), sobald coturn live
- [ ] 3-Wege (1 Browser + 2 Rust) Full-Mesh

## Bekannte Limitierungen

- **PTT**: nur Tab-fokussiert (Browser-Sandbox, kein globaler Hotkey). Tauri behält
  globales `rdev`-PTT. Bewusst kommuniziert, nicht simuliert.
- **TURN**: Front-Door-Deploy ist aktuell **STUN-only**
  (`deploy/docker-compose.proxy.yml`, kein coturn). Hinter hartem NAT scheitert
  ICE, bis coturn am Front-Door (oder Managed-TURN) konfiguriert ist. Heim-VM-Deploy
  hat coturn, aber self-signed → für den Browser nicht der richtige Pfad.
- **Self-signed-Pin-Pfad** (`:8080` Heim-VM) für Browser **nicht** nutzbar — nur
  CA-valide Front-Door oder localhost.
- **Opus-Quality**: Browser kann Stereo/DTX/inband-FEC senden; Rust-`read_track` +
  audiopus-Decode funktioniert, aber Mix-Qualität bei Browser-Quelle = Hörtest
  offen. Kein Negotiation-Bruch, nur Quality.
- **Mesh-Cap** warn@12/hard@16 gilt auch für Browser-Peers (zählt voll mit). Kein
  SFU — bewusst out of scope.

## Geänderte/neu angelegte Dateien (geplant, noch NICHT umgesetzt)

- **neu** `apps/web-participant/**` (Vite+TS-Client)
- **edit** `server/init/src/main.rs` — Landing-Button "Im Browser teilnehmen"
  (`/web/?code=`), Caddy `/web`-Static via `deploy/docker-compose.proxy.yml` /
  Caddyfile
- **keine** Änderung an `crates/protocol`, `apps/companion`, `apps/companion-core`
