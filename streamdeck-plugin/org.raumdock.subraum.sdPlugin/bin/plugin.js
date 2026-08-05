// subraum Stream Deck plugin. Talks two WebSockets:
//  1. to the Stream Deck software (registration handshake via argv),
//  2. to the subraum app's local control API (discovered via control.json in
//     the app's config dir; loopback + per-start token).
//
// Deliberately dependency-light: the Stream Deck registration protocol is
// spoken raw (no Elgato SDK), the only npm dependency is `ws`. State flows
// subraum → plugin as {"t":"state"} broadcasts; every visible key re-renders
// from the latest snapshot, so feedback survives reconnects of either socket.
"use strict";

const fs = require("node:fs");
const path = require("node:path");
const WebSocket = require("ws");

// ── Stream Deck registration (argv: -port N -pluginUUID U -registerEvent E -info J)
const argv = process.argv.slice(2);
const arg = (name) => {
  const i = argv.indexOf(name);
  return i >= 0 ? argv[i + 1] : undefined;
};
const sdPort = arg("-port");
const pluginUUID = arg("-pluginUUID");
const registerEvent = arg("-registerEvent");
if (!sdPort || !pluginUUID || !registerEvent) {
  process.exit(1); // not launched by Stream Deck
}

const sd = new WebSocket(`ws://127.0.0.1:${sdPort}`);
const sdSend = (obj) => {
  if (sd.readyState === WebSocket.OPEN) sd.send(JSON.stringify(obj));
};

// ── subraum control API ───────────────────────────────────────────────────────
// control.json is rewritten on every app start (port + token rotate), so it is
// re-read on every (re)connect attempt.
function controlFile() {
  // Test-harness override; unset in production.
  if (process.env.SUBRAUM_CONTROL_FILE) return process.env.SUBRAUM_CONTROL_FILE;
  if (process.platform === "win32") {
    return path.join(process.env.APPDATA || "", "org.raumdock.subraum", "control.json");
  }
  if (process.platform === "darwin") {
    return path.join(
      process.env.HOME || "",
      "Library",
      "Application Support",
      "org.raumdock.subraum",
      "control.json"
    );
  }
  return path.join(process.env.HOME || "", ".config", "org.raumdock.subraum", "control.json");
}

let sub = null; // live socket to subraum, null while down
let state = null; // last {"t":"state"} snapshot, null = app unreachable
let retryDelay = 1000;

function connectSubraum() {
  let cfg;
  try {
    cfg = JSON.parse(fs.readFileSync(controlFile(), "utf8"));
  } catch {
    return scheduleRetry(); // app not running yet (or never ran)
  }
  const ws = new WebSocket(`ws://127.0.0.1:${cfg.port}`);
  ws.on("open", () => {
    ws.send(JSON.stringify({ t: "auth", token: cfg.token }));
  });
  ws.on("message", (data) => {
    let msg;
    try {
      msg = JSON.parse(data.toString());
    } catch {
      return;
    }
    if (msg.t === "hello") {
      sub = ws;
      retryDelay = 1000;
      state = msg.state || {};
      renderAll();
    } else if (msg.t === "state") {
      state = msg.state || {};
      renderAll();
    }
    // auth-failed: stale token (app restarted between read and connect) — the
    // close handler retries, and the retry re-reads the fresh file.
  });
  ws.on("close", () => {
    if (sub === ws) sub = null;
    state = null;
    renderAll();
    scheduleRetry();
  });
  ws.on("error", () => {
    /* close fires next; retry happens there */
  });
}

let retryTimer = null;
function scheduleRetry() {
  if (retryTimer) return;
  retryTimer = setTimeout(() => {
    retryTimer = null;
    connectSubraum();
  }, retryDelay);
  retryDelay = Math.min(retryDelay * 2, 5000);
}

const send = (cmd) => {
  if (sub && sub.readyState === WebSocket.OPEN) {
    sub.send(JSON.stringify(cmd));
    return true;
  }
  return false;
};

// ── Key rendering ─────────────────────────────────────────────────────────────
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
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="144" height="144" viewBox="0 0 144 144">
  <rect width="144" height="144" rx="18" fill="${bg}"/>
  <rect x="4" y="4" width="136" height="136" rx="15" fill="none" stroke="${ring}" stroke-width="4"/>
  <text x="72" y="66" font-family="Segoe UI, Helvetica, Arial, sans-serif" font-size="44" fill="${dead ? C.dim : accent}" text-anchor="middle">${esc(glyph)}</text>
  <text x="72" y="112" font-family="Segoe UI, Helvetica, Arial, sans-serif" font-size="${label.length > 9 ? 18 : 22}" fill="${fill}" text-anchor="middle">${esc(label)}</text>
</svg>`;
  return "data:image/svg+xml;base64," + Buffer.from(svg).toString("base64");
}

// Visible action instances: context → { action, settings }.
const contexts = new Map();

function renderOne(context, info) {
  const s = state; // null = subraum unreachable
  const dead = s == null;
  const short = (v, n = 10) => (v && v.length > n ? v.slice(0, n - 1) + "…" : v || "");
  let img;
  switch (info.action) {
    case "org.raumdock.subraum.ptt":
      img = keySvg({
        glyph: s && s.transmitting ? "●" : "🎙",
        label: dead ? "offline" : s.mic_muted ? "stumm" : s.transmitting ? "SENDET" : "Funk",
        accent: s && s.transmitting ? C.tx : C.signal,
        active: !!(s && s.transmitting),
        dead: dead || !!(s && s.mic_muted),
      });
      break;
    case "org.raumdock.subraum.mic":
      img = keySvg({
        glyph: s && s.mic_muted ? "🔇" : "🎙",
        label: dead ? "offline" : s.mic_muted ? "stumm" : "Mikro an",
        accent: s && s.mic_muted ? C.warn : C.ok,
        active: !!(s && s.mic_muted),
        dead,
      });
      break;
    case "org.raumdock.subraum.deafen":
      img = keySvg({
        glyph: s && s.deafened ? "🔕" : "🔔",
        label: dead ? "offline" : s.deafened ? "taub" : "Ton an",
        accent: s && s.deafened ? C.warn : C.ok,
        active: !!(s && s.deafened),
        dead,
      });
      break;
    case "org.raumdock.subraum.channel": {
      const want = (info.settings && info.settings.channel) || "";
      const here = s && s.channel && want && s.channel.trim().toLowerCase() === want.trim().toLowerCase();
      img = keySvg({
        glyph: "📻",
        label: want ? short(want) : "Kanal?",
        accent: C.signal,
        active: !!here,
        dead,
      });
      break;
    }
    case "org.raumdock.subraum.channelnext":
    case "org.raumdock.subraum.channelprev":
      img = keySvg({
        glyph: info.action.endsWith("next") ? "▶" : "◀",
        label: dead ? "offline" : short(s.channel || ""),
        accent: C.signal,
        dead,
      });
      break;
    case "org.raumdock.subraum.volup":
    case "org.raumdock.subraum.voldown":
      img = keySvg({
        glyph: info.action.endsWith("up") ? "🔊" : "🔉",
        label: dead ? "offline" : `${s.volume != null ? s.volume : "?"} %`,
        accent: s && s.deafened ? C.warn : C.signal,
        dead,
      });
      break;
    case "org.raumdock.subraum.rekey":
      img = keySvg({
        glyph: "🔑",
        label: dead ? "offline" : s.rekeying ? "läuft…" : "Rekey",
        accent: C.signal,
        active: !!(s && s.rekeying),
        dead: dead || !!(s && !s.connected),
      });
      break;
    case "org.raumdock.subraum.disconnect":
      img = keySvg({
        glyph: "⏻",
        label: dead ? "offline" : s.connected ? "Verlassen" : "getrennt",
        accent: C.tx,
        dead: dead || !!(s && !s.connected),
      });
      break;
    default:
      return;
  }
  sdSend({ event: "setImage", context, payload: { image: img, target: 0 } });
}

function renderAll() {
  for (const [context, info] of contexts) renderOne(context, info);
}

// ── Stream Deck events ────────────────────────────────────────────────────────
sd.on("open", () => {
  sd.send(JSON.stringify({ event: registerEvent, uuid: pluginUUID }));
  connectSubraum();
});

sd.on("message", (data) => {
  let msg;
  try {
    msg = JSON.parse(data.toString());
  } catch {
    return;
  }
  const { event, action, context, payload } = msg;

  if (event === "willAppear") {
    contexts.set(context, { action, settings: (payload && payload.settings) || {} });
    renderOne(context, contexts.get(context));
    return;
  }
  if (event === "willDisappear") {
    contexts.delete(context);
    return;
  }
  if (event === "didReceiveSettings") {
    const info = contexts.get(context);
    if (info) {
      info.settings = (payload && payload.settings) || {};
      renderOne(context, info);
    }
    return;
  }

  const fail = () => sdSend({ event: "showAlert", context });

  if (event === "keyDown") {
    let ok = true;
    switch (action) {
      case "org.raumdock.subraum.ptt":
        ok = send({ t: "ptt", on: true });
        break;
      case "org.raumdock.subraum.mic":
        ok = send({ t: "mic-toggle" });
        break;
      case "org.raumdock.subraum.deafen":
        ok = send({ t: "deafen-toggle" });
        break;
      case "org.raumdock.subraum.channel": {
        const name = (contexts.get(context)?.settings?.channel || "").trim();
        ok = name ? send({ t: "channel", name }) : false;
        break;
      }
      case "org.raumdock.subraum.channelnext":
        ok = send({ t: "chan-cycle", dir: 1 });
        break;
      case "org.raumdock.subraum.channelprev":
        ok = send({ t: "chan-cycle", dir: -1 });
        break;
      case "org.raumdock.subraum.volup":
        ok = send({ t: "volume-delta", d: 5 });
        break;
      case "org.raumdock.subraum.voldown":
        ok = send({ t: "volume-delta", d: -5 });
        break;
      case "org.raumdock.subraum.rekey":
        ok = send({ t: "rekey" });
        break;
      case "org.raumdock.subraum.disconnect":
        ok = send({ t: "disconnect" });
        break;
    }
    if (!ok) fail();
    return;
  }

  if (event === "keyUp" && action === "org.raumdock.subraum.ptt") {
    // Always try to release — a mid-hold reconnect must not leave TX latched.
    send({ t: "ptt", on: false });
  }
});

sd.on("close", () => process.exit(0));
