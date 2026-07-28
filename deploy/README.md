# Deploy — InitConnection + coturn

Server side of subraum. **Untested scaffold** — verify before relying on it.

## Setup

1. `cp .env.example .env` and fill:
   - `ROOM_AUTH_SECRET` — HMAC secret for room join tokens.
   - `TURN_SECRET` — coturn shared secret (≥32 chars).
   - `PUBLIC_HOST` — the server's FQDN (used for TLS SAN + `turns:` URL).
2. Put `TURN_SECRET` into `turnserver.conf` → `static-auth-secret=` (must match).
3. `docker compose up -d --build`.
   - `init` auto-generates `certs/init-cert.pem` + `init-key.pem` on first run and
     **prints the cert SHA-256** in its logs (`docker compose logs init`).
   - coturn reuses those certs for `turns://`.

## Client

```
SERVER=wss://<PUBLIC_HOST>:8080/ws
CERT_SHA256=<fingerprint from init logs>
ROOM=<room>
TOKEN=<run `init-connection mint <room>` with ROOM_AUTH_SECRET set>
```

## Firewall

- TCP `8080` — signaling (wss).
- TCP/UDP `3478`, TCP `5349` — coturn STUN/TURN(S).
- **UDP `49152-65535`** — TURN relay range (must be open).

## Notes

- coturn runs `network_mode: host` — TURN relay needs real source ports (docker
  NAT rewrite breaks ICE).
- Mint a room token: `ROOM_AUTH_SECRET=… docker compose exec init init-connection mint <room>`.

## Rename migration (SquadLink Lite → subraum)

The repo now points at the new names; these are host-side steps that must be done
by hand on the deploy box, or the pull/serve path breaks:

- Rename the download dir: `/opt/RDOC-Suite/downloads/squadlink` →
  `/opt/RDOC-Suite/downloads/subraum` (matches `DOWNLOADS_DIR` in
  `docker-compose.proxy.yml` and `DEST` in `pull-installer.sh`).
- Point Caddy at `subraum.cc` (cert + reverse-proxy vhost).
- Keep the old `squadlink.raumdock.org` vhost alive, but **redirect the pages
  only — never `/ws`.** Already-installed clients have
  `wss://squadlink.raumdock.org/ws` compiled in, and the WebSocket client
  (`tokio-tungstenite`, `signaling.rs`) does **not** follow HTTP redirects: a 301
  on `/ws` fails the handshake and every old install loses signaling. Proxy that
  path to the same backend instead:

  ```caddyfile
  squadlink.raumdock.org {
      # Old installs still dial this — must stay a real WS proxy, not a redirect.
      handle /ws* {
          reverse_proxy 127.0.0.1:8090
      }
      # Everything else (landing page, /get, /j/<code>) moves to the new domain.
      handle {
          redir https://subraum.cc{uri} permanent
      }
  }
  ```

  Old and new clients share one InitConnection and one room namespace — the wire
  protocol did not change, so a mixed squad still connects. Retire the `/ws`
  proxy only once the old builds are out of circulation.
- The installer-pull systemd unit and its forced-command entry keep their old
  name unless you rename both together.
- Container name changed to `subraum-init` — `docker compose up -d` creates a new
  container; remove the old `squadlink-init` afterwards.
