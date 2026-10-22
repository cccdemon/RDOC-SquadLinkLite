// Session codes, share links and subraum:// deep links. Pure parsing; the
// fetch/connect side effects stay in App.tsx.

/** The hosted public session service. */
export const SESSION_BASE = "https://subraum.cc";

/** Session code from either a bare code or a share link ".../j/<code>".
 *  Anything that is not a share link is returned trimmed, as typed. */
export function parseCode(s: string): string {
  const t = s.trim();
  const m = t.match(/\/j\/([A-Za-z0-9]+)/);
  return m ? m[1] : t;
}

/** Origin to talk to for a pasted input: the link's own host for an http(s)
 *  share link (so an old `squadlink.raumdock.org/j/…` link still reaches the
 *  old host), otherwise the hosted default. */
export function baseFromInput(input: string): string {
  const t = input.trim();
  if (/^https?:\/\//.test(t)) {
    try {
      const u = new URL(t);
      return `${u.protocol}//${u.host}`;
    } catch {
      /* fall through */
    }
  }
  return SESSION_BASE;
}

/** Deep-link schemes we accept. `squadlink:` is the pre-rename scheme — same
 *  payload, still accepted so links already handed out keep working. Must stay
 *  in sync with DEEPLINK_SCHEMES in src-tauri and tauri.conf.json. */
export const LINK_SCHEMES = ["subraum:", "squadlink:"];

export type DirectLink = { ws: string; room: string; token: string | null; name?: string; uid?: string };

/** Parse `subraum://connect?ws=<wss url>&room=<id>&token=<hex>&name=<>&uid=<>`.
 *  null for any other scheme, a missing ws/room, or something that is not a URL.
 *  The URL parser lowercases the scheme, so `SUBRAUM://…` is accepted here; the
 *  Rust side enforces the wss policy on `ws` itself. */
export function parseDirectLink(raw: string): DirectLink | null {
  try {
    const u = new URL(raw.trim());
    if (!LINK_SCHEMES.includes(u.protocol)) return null;
    const ws = u.searchParams.get("ws");
    const room = u.searchParams.get("room");
    if (!ws || !room) return null;
    return {
      ws,
      room,
      token: u.searchParams.get("token"),
      name: u.searchParams.get("name") || undefined,
      uid: u.searchParams.get("uid") || undefined,
    };
  } catch {
    return null;
  }
}

/** Hosts a deep link may connect to without asking. Anything else needs a click. */
export const TRUSTED_LINK_HOSTS = ["subraum.cc", "squadlink.raumdock.org"];

/** Lowercased hostname (no port) of a deep link's `ws` URL, or null when it is
 *  not a URL at all. "Subraum.cc:443" → "subraum.cc". */
export function linkHost(ws: string): string | null {
  try {
    return new URL(ws).hostname.toLowerCase();
  } catch {
    return null;
  }
}

/** True when the deep link's server is one we connect to without confirmation. */
export function isTrustedLinkHost(host: string): boolean {
  return TRUSTED_LINK_HOSTS.includes(host.toLowerCase());
}

/** Stable identity from a deep link (e.g. a Discord name), reduced to the id
 *  charset the backend accepts ([A-Za-z0-9_.-]) and bounded to 64 chars.
 *  Empty when nothing survives — the caller then falls back to a random id. */
export function sanitizeUid(raw: string | undefined | null): string {
  return (raw || "").replace(/[^A-Za-z0-9_.-]/g, "").slice(0, 64);
}
