// Pure logic of the subraum Stream Deck plugin: everything that needs neither a
// socket nor the Stream Deck runtime. `plugin.js` wires these onto the two
// WebSockets; `../../test/*.test.mjs` exercises them directly. Keep this file
// free of side effects at require time (no argv parsing, no process.exit).
"use strict";

const path = require("node:path");

// ── Stream Deck launch arguments ──────────────────────────────────────────────
// Stream Deck starts the plugin with `-port N -pluginUUID U -registerEvent E
// -info J`. Returns null when any of the three required values is missing,
// which means we were not launched by Stream Deck.
function parseLaunchArgs(argv) {
  const arg = (name) => {
    const i = argv.indexOf(name);
    return i >= 0 ? argv[i + 1] : undefined;
  };
  const port = arg("-port");
  const pluginUUID = arg("-pluginUUID");
  const registerEvent = arg("-registerEvent");
  if (!port || !pluginUUID || !registerEvent) return null;
  return { port, pluginUUID, registerEvent, info: arg("-info") };
}

// ── subraum control API discovery ─────────────────────────────────────────────
// control.json is rewritten on every app start (port + token rotate), so the
// caller re-reads it on every (re)connect attempt. `env`/`platform` default to
// the process values; tests pass their own.
function controlFile(env = process.env, platform = process.platform) {
  // Test-harness override; unset in production.
  if (env.SUBRAUM_CONTROL_FILE) return env.SUBRAUM_CONTROL_FILE;
  if (platform === "win32") {
    return path.join(env.APPDATA || "", "org.raumdock.subraum", "control.json");
  }
  if (platform === "darwin") {
    return path.join(env.HOME || "", "Library", "Application Support", "org.raumdock.subraum", "control.json");
  }
  return path.join(env.HOME || "", ".config", "org.raumdock.subraum", "control.json");
}

// Parse the contents of control.json. Null when it is not JSON or lacks a
// usable loopback port / token — the plugin then treats the app as not running.
function parseControlFile(text) {
  let cfg;
  try {
    cfg = JSON.parse(text);
  } catch {
    return null;
  }
  if (!cfg || typeof cfg !== "object") return null;
  const port = Number(cfg.port);
  if (!Number.isInteger(port) || port < 1 || port > 65535) return null;
  if (typeof cfg.token !== "string" || cfg.token.length === 0) return null;
  return { port, token: cfg.token };
}

// ── control API frames ────────────────────────────────────────────────────────
// First frame on the control socket; the app ignores everything before it and
// drops the socket after three bad tokens (control.rs).
function authFrame(token) {
  return { t: "auth", token };
}

// Classify an inbound control-API frame. `hello` and `state` carry the full
// state snapshot (an empty object when the app sent none); `auth-failed` means
// our token is stale (app restarted between read and connect) and the caller
// must close + re-read control.json; anything else is ignored.
function classifyControlMessage(raw) {
  let msg;
  try {
    msg = JSON.parse(typeof raw === "string" ? raw : raw.toString());
  } catch {
    return { kind: "ignore" };
  }
  if (!msg || typeof msg !== "object") return { kind: "ignore" };
  if (msg.t === "hello" || msg.t === "state") {
    return { kind: msg.t, state: msg.state && typeof msg.state === "object" ? msg.state : {} };
  }
  if (msg.t === "auth-failed") return { kind: "auth-failed" };
  return { kind: "ignore" };
}

// ── reconnect backoff ─────────────────────────────────────────────────────────
const RETRY_INITIAL_MS = 1000;
const RETRY_MAX_MS = 5000;
// Delay to use after a retry at `current` was scheduled: doubles, capped.
function nextRetryDelay(current) {
  return Math.min(current * 2, RETRY_MAX_MS);
}

// ── actions ───────────────────────────────────────────────────────────────────
const A = {
  ptt: "org.raumdock.subraum.ptt",
  mic: "org.raumdock.subraum.mic",
  deafen: "org.raumdock.subraum.deafen",
  channel: "org.raumdock.subraum.channel",
  channelnext: "org.raumdock.subraum.channelnext",
  channelprev: "org.raumdock.subraum.channelprev",
  volup: "org.raumdock.subraum.volup",
  voldown: "org.raumdock.subraum.voldown",
  rekey: "org.raumdock.subraum.rekey",
  disconnect: "org.raumdock.subraum.disconnect",
};
const ACTIONS = Object.values(A);

// Control-API command for a key press, or null when the press has nothing to
// send (unknown action, or a channel key with no channel configured). The
// caller shows the Stream Deck alert for null.
function keyDownCommand(action, settings) {
  switch (action) {
    case A.ptt:
      return { t: "ptt", on: true };
    case A.mic:
      return { t: "mic-toggle" };
    case A.deafen:
      return { t: "deafen-toggle" };
    case A.channel: {
      const name = ((settings && settings.channel) || "").trim();
      return name ? { t: "channel", name } : null;
    }
    case A.channelnext:
      return { t: "chan-cycle", dir: 1 };
    case A.channelprev:
      return { t: "chan-cycle", dir: -1 };
    case A.volup:
      return { t: "volume-delta", d: 5 };
    case A.voldown:
      return { t: "volume-delta", d: -5 };
    case A.rekey:
      return { t: "rekey" };
    case A.disconnect:
      return { t: "disconnect" };
    default:
      return null;
  }
}

// Only PTT acts on release: always send the release so a mid-hold reconnect
// cannot leave TX latched.
function keyUpCommand(action) {
  return action === A.ptt ? { t: "ptt", on: false } : null;
}

// ── key rendering ─────────────────────────────────────────────────────────────
// One SVG per key, sent as a data URI. Two-line layout: symbol + label. Colors
// follow the app: void background, signal blue, TX red, warn amber.
const C = { bg: "#0F131B", line: "#2B3646", ink: "#E4E8EE", dim: "#8C96A6", signal: "#7FB0FF", tx: "#E0524D", warn: "#E0A244", ok: "#3FCF8E" };

function esc(s) {
  return String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

function keySvg({ glyph, label, accent = C.signal, active = false, dead = false }) {
  const ring = active ? accent : C.line;
  const fill = dead ? C.dim : C.ink;
  const bg = active ? `${accent}26` : C.bg; // 15% tint when active
  return `<svg xmlns="http://www.w3.org/2000/svg" width="144" height="144" viewBox="0 0 144 144">
  <rect width="144" height="144" rx="18" fill="${bg}"/>
  <rect x="4" y="4" width="136" height="136" rx="15" fill="none" stroke="${ring}" stroke-width="4"/>
  <text x="72" y="66" font-family="Segoe UI, Helvetica, Arial, sans-serif" font-size="44" fill="${dead ? C.dim : accent}" text-anchor="middle">${esc(glyph)}</text>
  <text x="72" y="112" font-family="Segoe UI, Helvetica, Arial, sans-serif" font-size="${label.length > 9 ? 18 : 22}" fill="${fill}" text-anchor="middle">${esc(label)}</text>
</svg>`;
}

function svgDataUri(svg) {
  return "data:image/svg+xml;base64," + Buffer.from(svg).toString("base64");
}

// Truncate a label with an ellipsis so it fits one key line.
function short(v, n = 10) {
  return v && v.length > n ? v.slice(0, n - 1) + "…" : v || "";
}

// Channel names match the way the app matches them: trimmed, case-insensitive.
function sameChannel(a, b) {
  return !!a && !!b && a.trim().toLowerCase() === b.trim().toLowerCase();
}

// What a key shows for `action` given its `settings` and the last state
// snapshot (`null` = subraum unreachable → every key reads "offline" and is
// dimmed). Returns `{ glyph, label, accent, active, dead }` or null for an
// unknown action.
function keyFace(action, settings, s) {
  const dead = s == null;
  switch (action) {
    case A.ptt:
      return {
        glyph: s && s.transmitting ? "●" : "🎙",
        label: dead ? "offline" : s.mic_muted ? "stumm" : s.transmitting ? "SENDET" : "Funk",
        accent: s && s.transmitting ? C.tx : C.signal,
        active: !!(s && s.transmitting),
        dead: dead || !!(s && s.mic_muted),
      };
    case A.mic:
      return {
        glyph: s && s.mic_muted ? "🔇" : "🎙",
        label: dead ? "offline" : s.mic_muted ? "stumm" : "Mikro an",
        accent: s && s.mic_muted ? C.warn : C.ok,
        active: !!(s && s.mic_muted),
        dead,
      };
    case A.deafen:
      return {
        glyph: s && s.deafened ? "🔕" : "🔔",
        label: dead ? "offline" : s.deafened ? "taub" : "Ton an",
        accent: s && s.deafened ? C.warn : C.ok,
        active: !!(s && s.deafened),
        dead,
      };
    case A.channel: {
      const want = (settings && settings.channel) || "";
      return {
        glyph: "📻",
        label: want ? short(want) : "Kanal?",
        accent: C.signal,
        active: !dead && sameChannel(s.channel, want),
        dead,
      };
    }
    case A.channelnext:
    case A.channelprev:
      return {
        glyph: action.endsWith("next") ? "▶" : "◀",
        label: dead ? "offline" : short(s.channel || ""),
        accent: C.signal,
        active: false,
        dead,
      };
    case A.volup:
    case A.voldown:
      return {
        glyph: action.endsWith("up") ? "🔊" : "🔉",
        label: dead ? "offline" : `${s.volume != null ? s.volume : "?"} %`,
        accent: s && s.deafened ? C.warn : C.signal,
        active: false,
        dead,
      };
    case A.rekey:
      return {
        glyph: "🔑",
        label: dead ? "offline" : s.rekeying ? "läuft…" : "Rekey",
        accent: C.signal,
        active: !!(s && s.rekeying),
        dead: dead || !!(s && !s.connected),
      };
    case A.disconnect:
      return {
        glyph: "⏻",
        label: dead ? "offline" : s.connected ? "Verlassen" : "getrennt",
        accent: C.tx,
        active: false,
        dead: dead || !!(s && !s.connected),
      };
    default:
      return null;
  }
}

// The image to put on a key, or null for an unknown action.
function keyImage(action, settings, state) {
  const face = keyFace(action, settings, state);
  return face ? svgDataUri(keySvg(face)) : null;
}

module.exports = {
  A,
  ACTIONS,
  C,
  RETRY_INITIAL_MS,
  RETRY_MAX_MS,
  authFrame,
  classifyControlMessage,
  controlFile,
  esc,
  keyDownCommand,
  keyFace,
  keyImage,
  keySvg,
  keyUpCommand,
  nextRetryDelay,
  parseControlFile,
  parseLaunchArgs,
  sameChannel,
  short,
  svgDataUri,
};
