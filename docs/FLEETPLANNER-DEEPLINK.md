# Fleetplanner → subraum — Deep-Link Config

How the Fleetplanner generates a `subraum://` config link that subraum
opens and auto-connects from (no code / PIN entry). Parsed by `parseDirectLink` in
`apps/companion/src/App.tsx`; values re-validated Rust-side in the `connect` command.

## Format

```
subraum://connect?ws=<WS_URL>&room=<ROOM>&token=<TOKEN>&name=<NAME>&uid=<UID>
```

- Scheme: `subraum:` (registered by the app — MSIX manifest / registry).
  The pre-rename scheme **`squadlink:` is still accepted** with the identical
  payload, so links already distributed keep working; both are registered by the
  app and both are parsed by the client. Generate new links as `subraum://`.
- Host segment `connect` is conventional and ignored by the parser; keep it for
  readability.
- Everything meaningful is in the **query string**.

## Parameters

| Param   | Required | Format / validation | Notes |
| ------- | -------- | ------------------- | ----- |
| `ws`    | **yes**  | `wss://…` (TLS). `ws://` only for loopback (`localhost` / `127.0.0.1`, dev). | Signaling server WebSocket URL, e.g. `wss://subraum.cc/ws`. |
| `room`  | **yes**  | 1–64 chars, `[A-Za-z0-9_-]` only. | Room / session id. |
| `token` | no       | hex only (`[0-9a-fA-F]`), ≤128 chars. | Session auth token. Omit if the room needs none. |
| `name`  | no       | 1–64 chars, no control chars. | **Display** name. Omitted → app uses "Commander". |
| `uid`   | no       | sanitized to `[A-Za-z0-9_.-]`, ≤64 chars. | **Stable identity** — set to the player's Discord name. Used as the roster/per-peer key. Omitted → app generates a random id. |

If `ws` or `room` is missing/invalid the app ignores the link. `uid` and `name` are
distinct: `uid` is the identity, `name` is what others see. The app sanitizes `uid`
to the id charset (e.g. characters outside `[A-Za-z0-9_.-]` are dropped), so pass the
Discord name as-is.

### Why no `cert_sha256`?

`cert_sha256` is an optional **TLS certificate pin** (trust exactly one cert by its
SHA-256 fingerprint — used only for self-signed / private servers). The public
`subraum.cc` has a normal CA-signed cert, so the app uses standard CA
validation and the field stays `null`. The Fleetplanner does not put it in the link.

## Encoding (important)

Query **values must be URL-encoded**. In particular `ws` contains `:` and `/`, and
`name` may contain spaces. The safe way is to let a URL builder do it:

```js
function buildsubraum({ ws, room, token, name, uid }) {
  const u = new URL("subraum://connect");
  u.searchParams.set("ws", ws);       // wss://subraum.cc/ws
  u.searchParams.set("room", room);   // alpha-fleet-01
  if (token) u.searchParams.set("token", token);
  if (name)  u.searchParams.set("name", name); // display name
  if (uid)   u.searchParams.set("uid", uid);   // Discord name (identity)
  return u.toString();
}
```

`URLSearchParams` percent-encodes for you (`://` → `%3A%2F%2F`, space → `+`/`%20`).
Both encoded and (for `:` `/`) unencoded forms parse correctly in the app, but always
encode `&`, `#`, spaces, and any non-ASCII.

## Example

Input:
- ws = `wss://subraum.cc/ws`
- room = `alpha-fleet-01`
- token = `a1b2c3d4e5f6`
- name = `Commander Ada`
- uid = `ada.commander`

Output link:

```
subraum://connect?ws=wss%3A%2F%2Fsubraum.cc%2Fws&room=alpha-fleet-01&token=a1b2c3d4e5f6&name=Commander+Ada&uid=ada.commander
```

Resulting `connect` call:

```
server      = wss://subraum.cc/ws
room        = alpha-fleet-01
token       = a1b2c3d4e5f6
name        = Commander Ada      (display)
user_id     = ada.commander      (from uid; random if uid omitted)
cert_sha256 = null               (public server uses standard CA validation)
```

## What the app does on activation

1. OS opens the link → subraum (cold start: launch arg; already running:
   forwarded to the live window).
2. App parses the params, fills the name + identity, and connects directly — no
   link/PIN entry.
3. Same `connect` path as a normal join, so all server-side validation still applies.
