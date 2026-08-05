// Dev harness: proves bin/plugin.js speaks both protocols without a physical
// Stream Deck or a running subraum. Spins a fake Stream Deck WS and a fake
// subraum control WS (token-checked), launches the plugin against them, walks
// one action through willAppear → keyDown/keyUp, and asserts what each side
// must have seen. Exit 0 = pass.
//
//   node test-harness.mjs
import { WebSocketServer } from "./org.raumdock.subraum.sdPlugin/node_modules/ws/wrapper.mjs";
import { spawn } from "node:child_process";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const fail = (msg) => {
  console.error("FAIL:", msg);
  process.exit(1);
};
const timeout = setTimeout(() => fail("timed out after 15s"), 15000);

// ── fake subraum control API ──────────────────────────────────────────────────
const TOKEN = "cafebabe".repeat(8);
const subSrv = new WebSocketServer({ host: "127.0.0.1", port: 0 });
const gotSubCmds = [];
let subClient = null;
subSrv.on("connection", (ws) => {
  let authed = false;
  ws.on("message", (d) => {
    const m = JSON.parse(d.toString());
    if (!authed) {
      if (m.t === "auth" && m.token === TOKEN) {
        authed = true;
        subClient = ws;
        ws.send(JSON.stringify({ t: "hello", app: "subraum", state: { connected: true, transmitting: false, mic_muted: false, channel: "Funk 1", volume: 100 } }));
      }
      return;
    }
    gotSubCmds.push(m);
  });
});

// ── fake Stream Deck ──────────────────────────────────────────────────────────
const sdSrv = new WebSocketServer({ host: "127.0.0.1", port: 0 });
const gotSdEvents = [];
sdSrv.on("connection", (ws) => {
  ws.on("message", (d) => {
    const m = JSON.parse(d.toString());
    gotSdEvents.push(m);
    if (m.event === "registered-test") return;
    if (m.event === "register") {
      // registration ack: feed the plugin one visible PTT key, then press it
      const ctx = "ctx-1";
      ws.send(JSON.stringify({ event: "willAppear", action: "org.raumdock.subraum.ptt", context: ctx, payload: { settings: {} } }));
      setTimeout(() => {
        ws.send(JSON.stringify({ event: "keyDown", action: "org.raumdock.subraum.ptt", context: ctx, payload: {} }));
        ws.send(JSON.stringify({ event: "keyUp", action: "org.raumdock.subraum.ptt", context: ctx, payload: {} }));
      }, 500);
      // push a state broadcast so a render with TX=true happens
      setTimeout(() => {
        if (subClient) subClient.send(JSON.stringify({ t: "state", state: { connected: true, transmitting: true, mic_muted: false, channel: "Funk 1", volume: 100 } }));
      }, 800);
    }
  });
});

await new Promise((r) => {
  let n = 0;
  subSrv.on("listening", () => ++n === 2 && r());
  sdSrv.on("listening", () => ++n === 2 && r());
});

const dir = mkdtempSync(join(tmpdir(), "subraum-deck-"));
const ctl = join(dir, "control.json");
writeFileSync(ctl, JSON.stringify({ port: subSrv.address().port, token: TOKEN }));

const plugin = spawn(process.execPath, [
  "org.raumdock.subraum.sdPlugin/bin/plugin.js",
  "-port", String(sdSrv.address().port),
  "-pluginUUID", "test-uuid",
  "-registerEvent", "register",
  "-info", "{}",
], { env: { ...process.env, SUBRAUM_CONTROL_FILE: ctl }, stdio: "inherit" });
plugin.on("exit", (code) => {
  if (!done) fail(`plugin exited early (code ${code})`);
});

// ── verdict ───────────────────────────────────────────────────────────────────
let done = false;
setTimeout(() => {
  const reg = gotSdEvents.find((e) => e.event === "register" && e.uuid === "test-uuid");
  if (!reg) fail("plugin never registered with the Stream Deck socket");

  const images = gotSdEvents.filter((e) => e.event === "setImage" && e.context === "ctx-1");
  if (images.length < 2) fail(`expected >=2 key renders (appear + TX state), got ${images.length}`);
  if (!images.every((i) => i.payload.image.startsWith("data:image/svg+xml;base64,")))
    fail("key renders must be SVG data URIs");

  const ptt = gotSubCmds.filter((c) => c.t === "ptt").map((c) => c.on);
  if (JSON.stringify(ptt) !== JSON.stringify([true, false]))
    fail(`expected ptt [true,false] at subraum, got ${JSON.stringify(ptt)}`);

  console.log(`PASS: register ✓, ${images.length} key renders ✓, ptt down/up reached subraum ✓`);
  done = true;
  clearTimeout(timeout);
  plugin.kill();
  process.exit(0);
}, 2500);
