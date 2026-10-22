// subraum Stream Deck plugin. Talks two WebSockets:
//  1. to the Stream Deck software (registration handshake via argv),
//  2. to the subraum app's local control API (discovered via control.json in
//     the app's config dir; loopback + per-start token).
//
// Deliberately dependency-light: the Stream Deck registration protocol is
// spoken raw (no Elgato SDK), the only npm dependency is `ws`. State flows
// subraum → plugin as {"t":"state"} broadcasts; every visible key re-renders
// from the latest snapshot, so feedback survives reconnects of either socket.
//
// This file is the socket wiring only; every decision (what a key shows, what
// a press sends, where control.json lives, backoff) lives in ./lib.js and is
// unit-tested from ../../test.
"use strict";

const fs = require("node:fs");
const WebSocket = require("ws");
const lib = require("./lib");

// ── Stream Deck registration (argv: -port N -pluginUUID U -registerEvent E -info J)
const launch = lib.parseLaunchArgs(process.argv.slice(2));
if (!launch) {
  process.exit(1); // not launched by Stream Deck
}
const { port: sdPort, pluginUUID, registerEvent } = launch;

const sd = new WebSocket(`ws://127.0.0.1:${sdPort}`);
const sdSend = (obj) => {
  if (sd.readyState === WebSocket.OPEN) sd.send(JSON.stringify(obj));
};

// ── subraum control API ───────────────────────────────────────────────────────
let sub = null; // live socket to subraum, null while down
let state = null; // last {"t":"state"} snapshot, null = app unreachable
let retryDelay = lib.RETRY_INITIAL_MS;

function connectSubraum() {
  // control.json is rewritten on every app start (port + token rotate), so it
  // is re-read on every (re)connect attempt.
  let cfg = null;
  try {
    cfg = lib.parseControlFile(fs.readFileSync(lib.controlFile(), "utf8"));
  } catch {
    /* missing file: app not running yet (or never ran) */
  }
  if (!cfg) return scheduleRetry();

  const ws = new WebSocket(`ws://127.0.0.1:${cfg.port}`);
  ws.on("open", () => {
    ws.send(JSON.stringify(lib.authFrame(cfg.token)));
  });
  ws.on("message", (data) => {
    const m = lib.classifyControlMessage(data);
    if (m.kind === "hello") {
      sub = ws;
      retryDelay = lib.RETRY_INITIAL_MS;
      state = m.state;
      renderAll();
    } else if (m.kind === "state") {
      state = m.state;
      renderAll();
    } else if (m.kind === "auth-failed") {
      // Stale token (app restarted between read and connect). The server only
      // closes after three bad tries, so close ourselves; the close handler
      // retries and re-reads the fresh token file.
      ws.close();
    }
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
  retryDelay = lib.nextRetryDelay(retryDelay);
}

const send = (cmd) => {
  if (sub && sub.readyState === WebSocket.OPEN) {
    sub.send(JSON.stringify(cmd));
    return true;
  }
  return false;
};

// ── Key rendering ─────────────────────────────────────────────────────────────
// Visible action instances: context → { action, settings }.
const contexts = new Map();

function renderOne(context, info) {
  const img = lib.keyImage(info.action, info.settings, state);
  if (img) sdSend({ event: "setImage", context, payload: { image: img, target: 0 } });
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

  if (event === "keyDown") {
    const cmd = lib.keyDownCommand(action, contexts.get(context)?.settings);
    if (!(cmd && send(cmd))) sdSend({ event: "showAlert", context });
    return;
  }

  if (event === "keyUp") {
    const cmd = lib.keyUpCommand(action);
    if (cmd) send(cmd);
  }
});

sd.on("close", () => process.exit(0));
