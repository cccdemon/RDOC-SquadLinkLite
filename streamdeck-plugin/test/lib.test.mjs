// Unit tests for the plugin's pure logic (bin/lib.js). Runs with the Node
// test runner, no dependencies:  npm test  (from streamdeck-plugin/).
//
// The wire format asserted here is the one apps/companion/src-tauri/src/control.rs
// accepts: first frame {"t":"auth","token":…}, then ptt {on}, chan-cycle {dir},
// mic-toggle, deafen-toggle, channel {name}, rekey, disconnect, volume-delta {d}.
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);
const lib = require(path.join(here, "..", "org.raumdock.subraum.sdPlugin", "bin", "lib.js"));
const manifest = JSON.parse(
  readFileSync(path.join(here, "..", "org.raumdock.subraum.sdPlugin", "manifest.json"), "utf8")
);

const { A } = lib;

/** Decode a key image back to its SVG source so labels can be asserted. */
function svgOf(dataUri) {
  const prefix = "data:image/svg+xml;base64,";
  assert.ok(dataUri.startsWith(prefix), "key image must be an SVG data URI");
  return Buffer.from(dataUri.slice(prefix.length), "base64").toString("utf8");
}

const ONLINE = { connected: true, transmitting: false, mic_muted: false, deafened: false, channel: "Funk 1", volume: 100 };

// ── launch arguments ──────────────────────────────────────────────────────────
describe("parseLaunchArgs", () => {
  it("reads the four Stream Deck launch flags in any order", () => {
    const r = lib.parseLaunchArgs(["-registerEvent", "registerPlugin", "-info", "{}", "-port", "28196", "-pluginUUID", "abc"]);
    assert.deepEqual(r, { port: "28196", pluginUUID: "abc", registerEvent: "registerPlugin", info: "{}" });
  });

  it("returns null when a required flag is missing (not launched by Stream Deck)", () => {
    assert.equal(lib.parseLaunchArgs([]), null);
    assert.equal(lib.parseLaunchArgs(["-port", "1", "-pluginUUID", "u"]), null);
    assert.equal(lib.parseLaunchArgs(["-port", "1", "-registerEvent", "e"]), null);
    assert.equal(lib.parseLaunchArgs(["-pluginUUID", "u", "-registerEvent", "e"]), null);
  });

  it("treats a trailing flag without a value as missing", () => {
    assert.equal(lib.parseLaunchArgs(["-pluginUUID", "u", "-registerEvent", "e", "-port"]), null);
  });

  it("tolerates a missing -info", () => {
    const r = lib.parseLaunchArgs(["-port", "1", "-pluginUUID", "u", "-registerEvent", "e"]);
    assert.equal(r.info, undefined);
    assert.equal(r.port, "1");
  });
});

// ── control.json discovery ────────────────────────────────────────────────────
describe("controlFile", () => {
  it("honours the SUBRAUM_CONTROL_FILE override on every platform", () => {
    for (const p of ["win32", "darwin", "linux"]) {
      assert.equal(lib.controlFile({ SUBRAUM_CONTROL_FILE: "/tmp/x.json", APPDATA: "C:\\a", HOME: "/h" }, p), "/tmp/x.json");
    }
  });

  it("uses %APPDATA%\\org.raumdock.subraum on Windows", () => {
    const f = lib.controlFile({ APPDATA: "C:\\Users\\u\\AppData\\Roaming" }, "win32");
    assert.equal(f, path.join("C:\\Users\\u\\AppData\\Roaming", "org.raumdock.subraum", "control.json"));
  });

  it("uses ~/Library/Application Support/org.raumdock.subraum on macOS", () => {
    const f = lib.controlFile({ HOME: "/Users/u" }, "darwin");
    assert.equal(f, path.join("/Users/u", "Library", "Application Support", "org.raumdock.subraum", "control.json"));
  });

  it("uses ~/.config/org.raumdock.subraum elsewhere", () => {
    const f = lib.controlFile({ HOME: "/home/u" }, "linux");
    assert.equal(f, path.join("/home/u", ".config", "org.raumdock.subraum", "control.json"));
    assert.equal(lib.controlFile({ HOME: "/home/u" }, "freebsd"), f);
  });

  it("does not throw when the home/appdata variable is unset", () => {
    assert.ok(lib.controlFile({}, "win32").endsWith("control.json"));
    assert.ok(lib.controlFile({}, "darwin").endsWith("control.json"));
    assert.ok(lib.controlFile({}, "linux").endsWith("control.json"));
  });

  it("the Tauri identifier in the path matches the manifest's action namespace", () => {
    // control.rs writes control.json into the app config dir, which Tauri names
    // after the identifier org.raumdock.subraum — the same prefix every action UUID uses.
    const f = lib.controlFile({ HOME: "/h" }, "linux");
    assert.ok(f.includes("org.raumdock.subraum"));
    for (const a of manifest.Actions) assert.ok(a.UUID.startsWith("org.raumdock.subraum."), a.UUID);
  });
});

describe("parseControlFile", () => {
  it("accepts the shape control.rs writes", () => {
    assert.deepEqual(lib.parseControlFile('{"port":41234,"token":"cafebabe"}'), { port: 41234, token: "cafebabe" });
  });

  it("accepts a numeric string port", () => {
    assert.deepEqual(lib.parseControlFile('{"port":"41234","token":"t"}'), { port: 41234, token: "t" });
  });

  it("rejects malformed JSON, non-objects and arrays", () => {
    assert.equal(lib.parseControlFile(""), null);
    assert.equal(lib.parseControlFile("{not json"), null);
    assert.equal(lib.parseControlFile("null"), null);
    assert.equal(lib.parseControlFile("42"), null);
    assert.equal(lib.parseControlFile("[1,2]"), null);
  });

  it("rejects a missing or out-of-range port", () => {
    assert.equal(lib.parseControlFile('{"token":"t"}'), null);
    assert.equal(lib.parseControlFile('{"port":0,"token":"t"}'), null);
    assert.equal(lib.parseControlFile('{"port":65536,"token":"t"}'), null);
    assert.equal(lib.parseControlFile('{"port":1.5,"token":"t"}'), null);
    assert.equal(lib.parseControlFile('{"port":"abc","token":"t"}'), null);
  });

  it("rejects a missing or empty token", () => {
    assert.equal(lib.parseControlFile('{"port":1}'), null);
    assert.equal(lib.parseControlFile('{"port":1,"token":""}'), null);
    assert.equal(lib.parseControlFile('{"port":1,"token":7}'), null);
  });
});

// ── control API frames ────────────────────────────────────────────────────────
describe("authFrame", () => {
  it("is exactly the first frame control.rs expects", () => {
    assert.deepEqual(lib.authFrame("deadbeef"), { t: "auth", token: "deadbeef" });
    assert.equal(JSON.stringify(lib.authFrame("x")), '{"t":"auth","token":"x"}');
  });
});

describe("classifyControlMessage", () => {
  it("hello carries the snapshot and marks the socket live", () => {
    const m = lib.classifyControlMessage(JSON.stringify({ t: "hello", app: "subraum", state: ONLINE }));
    assert.equal(m.kind, "hello");
    assert.deepEqual(m.state, ONLINE);
  });

  it("state broadcasts carry the new snapshot", () => {
    const m = lib.classifyControlMessage(Buffer.from(JSON.stringify({ t: "state", state: { channel: "Funk 2" } })));
    assert.equal(m.kind, "state");
    assert.deepEqual(m.state, { channel: "Funk 2" });
  });

  it("a hello/state without a state object yields an empty snapshot, not null", () => {
    assert.deepEqual(lib.classifyControlMessage('{"t":"hello"}').state, {});
    assert.deepEqual(lib.classifyControlMessage('{"t":"state","state":null}').state, {});
    assert.deepEqual(lib.classifyControlMessage('{"t":"state","state":"x"}').state, {});
  });

  it("auth-failed is recognised so the plugin can close and re-read the token", () => {
    assert.deepEqual(lib.classifyControlMessage('{"t":"auth-failed"}'), { kind: "auth-failed" });
  });

  it("ignores garbage, non-objects and unknown tags", () => {
    assert.deepEqual(lib.classifyControlMessage("not json"), { kind: "ignore" });
    assert.deepEqual(lib.classifyControlMessage("[]").kind, "ignore");
    assert.deepEqual(lib.classifyControlMessage("null"), { kind: "ignore" });
    assert.deepEqual(lib.classifyControlMessage('{"t":"shutdown-the-machine"}'), { kind: "ignore" });
    assert.deepEqual(lib.classifyControlMessage("{}"), { kind: "ignore" });
  });
});

// ── backoff ───────────────────────────────────────────────────────────────────
describe("nextRetryDelay", () => {
  it("starts at one second and doubles up to a five second cap", () => {
    assert.equal(lib.RETRY_INITIAL_MS, 1000);
    assert.equal(lib.RETRY_MAX_MS, 5000);
    const seq = [];
    let d = lib.RETRY_INITIAL_MS;
    for (let i = 0; i < 5; i++) {
      seq.push(d);
      d = lib.nextRetryDelay(d);
    }
    assert.deepEqual(seq, [1000, 2000, 4000, 5000, 5000]);
  });

  it("never exceeds the cap even from above it", () => {
    assert.equal(lib.nextRetryDelay(9000), 5000);
  });
});

// ── key presses → commands ────────────────────────────────────────────────────
describe("keyDownCommand", () => {
  it("maps every stateless action to its control.rs command", () => {
    assert.deepEqual(lib.keyDownCommand(A.ptt), { t: "ptt", on: true });
    assert.deepEqual(lib.keyDownCommand(A.mic), { t: "mic-toggle" });
    assert.deepEqual(lib.keyDownCommand(A.deafen), { t: "deafen-toggle" });
    assert.deepEqual(lib.keyDownCommand(A.channelnext), { t: "chan-cycle", dir: 1 });
    assert.deepEqual(lib.keyDownCommand(A.channelprev), { t: "chan-cycle", dir: -1 });
    assert.deepEqual(lib.keyDownCommand(A.volup), { t: "volume-delta", d: 5 });
    assert.deepEqual(lib.keyDownCommand(A.voldown), { t: "volume-delta", d: -5 });
    assert.deepEqual(lib.keyDownCommand(A.rekey), { t: "rekey" });
    assert.deepEqual(lib.keyDownCommand(A.disconnect), { t: "disconnect" });
  });

  it("only uses tags control.rs accepts", () => {
    const accepted = new Set(["ptt", "chan-cycle", "mic-toggle", "deafen-toggle", "channel", "rekey", "disconnect", "volume", "volume-delta"]);
    for (const a of lib.ACTIONS) {
      const cmd = lib.keyDownCommand(a, { channel: "Funk 1" });
      assert.ok(cmd, a);
      assert.ok(accepted.has(cmd.t), `${a} sends unknown tag ${cmd.t}`);
    }
  });

  it("chan-cycle dir is exactly ±1 (control.rs rejects anything else)", () => {
    assert.ok([1, -1].includes(lib.keyDownCommand(A.channelnext).dir));
    assert.ok([1, -1].includes(lib.keyDownCommand(A.channelprev).dir));
  });

  it("channel key sends the trimmed configured name", () => {
    assert.deepEqual(lib.keyDownCommand(A.channel, { channel: "  Funk 2 " }), { t: "channel", name: "Funk 2" });
  });

  it("channel key with no (or blank) channel configured sends nothing → alert", () => {
    assert.equal(lib.keyDownCommand(A.channel, {}), null);
    assert.equal(lib.keyDownCommand(A.channel, undefined), null);
    assert.equal(lib.keyDownCommand(A.channel, { channel: "   " }), null);
  });

  it("unknown actions send nothing", () => {
    assert.equal(lib.keyDownCommand("org.raumdock.subraum.nope", {}), null);
    assert.equal(lib.keyDownCommand(undefined, {}), null);
  });
});

describe("keyUpCommand", () => {
  it("PTT release always maps to ptt off", () => {
    assert.deepEqual(lib.keyUpCommand(A.ptt), { t: "ptt", on: false });
  });

  it("no other key acts on release", () => {
    for (const a of lib.ACTIONS.filter((x) => x !== A.ptt)) assert.equal(lib.keyUpCommand(a), null, a);
    assert.equal(lib.keyUpCommand("something.else"), null);
  });
});

// ── action list vs manifest ───────────────────────────────────────────────────
describe("ACTIONS", () => {
  it("match the manifest one to one", () => {
    const fromManifest = manifest.Actions.map((a) => a.UUID).sort();
    assert.deepEqual([...lib.ACTIONS].sort(), fromManifest);
  });

  it("manifest CodePath is the socket wiring, which requires this lib", () => {
    assert.equal(manifest.CodePath, "bin/plugin.js");
    const src = readFileSync(path.join(here, "..", "org.raumdock.subraum.sdPlugin", "bin", "plugin.js"), "utf8");
    assert.match(src, /require\("\.\/lib"\)/);
  });
});

// ── rendering ─────────────────────────────────────────────────────────────────
describe("esc", () => {
  it("escapes the three characters that break SVG text", () => {
    assert.equal(lib.esc('<a & "b">'), '&lt;a &amp; "b"&gt;');
    assert.equal(lib.esc(42), "42");
  });
});

describe("short", () => {
  it("passes short labels through and ellipsises long ones to n chars", () => {
    assert.equal(lib.short("Funk 1"), "Funk 1");
    assert.equal(lib.short("0123456789"), "0123456789");
    assert.equal(lib.short("01234567890"), "012345678…");
    assert.equal(lib.short("01234567890").length, 10);
    assert.equal(lib.short("abcdef", 4), "abc…");
  });

  it("maps null/undefined to an empty string", () => {
    assert.equal(lib.short(null), "");
    assert.equal(lib.short(undefined), "");
    assert.equal(lib.short(""), "");
  });
});

describe("sameChannel", () => {
  it("is trim + case-insensitive, like the app", () => {
    assert.ok(lib.sameChannel("Funk 1", " funk 1 "));
    assert.ok(lib.sameChannel("ALPHA", "alpha"));
  });

  it("never matches an empty or missing side", () => {
    assert.equal(lib.sameChannel("", "x"), false);
    assert.equal(lib.sameChannel("x", ""), false);
    assert.equal(lib.sameChannel(undefined, "x"), false);
    assert.equal(lib.sameChannel("x", null), false);
    assert.equal(lib.sameChannel("Funk 1", "Funk 2"), false);
  });
});

describe("keySvg / svgDataUri", () => {
  it("renders a 144×144 SVG with glyph and label, escaped", () => {
    const svg = lib.keySvg({ glyph: "<", label: "a&b" });
    assert.match(svg, /^<svg xmlns="http:\/\/www\.w3\.org\/2000\/svg" width="144" height="144"/);
    assert.ok(svg.includes(">&lt;</text>"));
    assert.ok(svg.includes(">a&amp;b</text>"));
  });

  it("active keys get a tinted background and an accent ring; idle keys the line ring", () => {
    const active = lib.keySvg({ glyph: "x", label: "l", accent: "#E0524D", active: true });
    assert.ok(active.includes(`fill="#E0524D26"`), "15% tint of the accent");
    assert.ok(active.includes(`stroke="#E0524D"`));
    const idle = lib.keySvg({ glyph: "x", label: "l", accent: "#E0524D", active: false });
    assert.ok(idle.includes(`fill="${lib.C.bg}"`));
    assert.ok(idle.includes(`stroke="${lib.C.line}"`));
  });

  it("dead keys are drawn dim for glyph and label", () => {
    const dead = lib.keySvg({ glyph: "x", label: "l", dead: true });
    const dims = dead.match(new RegExp(`fill="${lib.C.dim}"`, "g")) || [];
    assert.equal(dims.length, 2);
  });

  it("long labels drop to the smaller font size", () => {
    assert.ok(lib.keySvg({ glyph: "x", label: "123456789" }).includes('font-size="22"'));
    assert.ok(lib.keySvg({ glyph: "x", label: "1234567890" }).includes('font-size="18"'));
  });

  it("svgDataUri base64-encodes with the SVG media type", () => {
    const uri = lib.svgDataUri("<svg/>");
    assert.equal(uri, "data:image/svg+xml;base64," + Buffer.from("<svg/>").toString("base64"));
    assert.equal(svgOf(uri), "<svg/>");
  });
});

describe("keyFace", () => {
  it("every action reads 'offline' and is dead while subraum is unreachable", () => {
    for (const a of lib.ACTIONS) {
      const f = lib.keyFace(a, { channel: "Funk 1" }, null);
      assert.ok(f, a);
      assert.equal(f.dead, true, a);
      assert.equal(f.active, false, a);
      if (a !== A.channel) assert.equal(f.label, "offline", a);
    }
  });

  it("the channel key keeps showing its configured name while offline", () => {
    assert.equal(lib.keyFace(A.channel, { channel: "Funk 2" }, null).label, "Funk 2");
    assert.equal(lib.keyFace(A.channel, {}, null).label, "Kanal?");
  });

  it("PTT: idle → Funk, transmitting → red SENDET + active, muted → dim 'stumm'", () => {
    assert.deepEqual(lib.keyFace(A.ptt, {}, ONLINE), { glyph: "🎙", label: "Funk", accent: lib.C.signal, active: false, dead: false });
    const tx = lib.keyFace(A.ptt, {}, { ...ONLINE, transmitting: true });
    assert.equal(tx.label, "SENDET");
    assert.equal(tx.glyph, "●");
    assert.equal(tx.accent, lib.C.tx);
    assert.equal(tx.active, true);
    assert.equal(tx.dead, false);
    const muted = lib.keyFace(A.ptt, {}, { ...ONLINE, mic_muted: true });
    assert.equal(muted.label, "stumm");
    assert.equal(muted.dead, true, "muted PTT is drawn dim: pressing it would not transmit");
  });

  it("mic: shows and highlights the muted state in warn colour", () => {
    assert.deepEqual(lib.keyFace(A.mic, {}, ONLINE), { glyph: "🎙", label: "Mikro an", accent: lib.C.ok, active: false, dead: false });
    assert.deepEqual(lib.keyFace(A.mic, {}, { ...ONLINE, mic_muted: true }), { glyph: "🔇", label: "stumm", accent: lib.C.warn, active: true, dead: false });
  });

  it("deafen: shows and highlights the deafened state in warn colour", () => {
    assert.deepEqual(lib.keyFace(A.deafen, {}, ONLINE), { glyph: "🔔", label: "Ton an", accent: lib.C.ok, active: false, dead: false });
    assert.deepEqual(lib.keyFace(A.deafen, {}, { ...ONLINE, deafened: true }), { glyph: "🔕", label: "taub", accent: lib.C.warn, active: true, dead: false });
  });

  it("channel key lights up only on its own channel (trim + case-insensitive)", () => {
    assert.equal(lib.keyFace(A.channel, { channel: " funk 1" }, ONLINE).active, true);
    assert.equal(lib.keyFace(A.channel, { channel: "Funk 2" }, ONLINE).active, false);
    assert.equal(lib.keyFace(A.channel, {}, ONLINE).active, false);
    assert.equal(lib.keyFace(A.channel, { channel: "Funk 1" }, { ...ONLINE, channel: undefined }).active, false);
  });

  it("channel key truncates long names", () => {
    assert.equal(lib.keyFace(A.channel, { channel: "Einsatzleitung Nord" }, ONLINE).label, "Einsatzle…");
  });

  it("next/prev show the current channel with the matching arrow", () => {
    const n = lib.keyFace(A.channelnext, {}, ONLINE);
    const p = lib.keyFace(A.channelprev, {}, ONLINE);
    assert.equal(n.glyph, "▶");
    assert.equal(p.glyph, "◀");
    assert.equal(n.label, "Funk 1");
    assert.equal(p.label, "Funk 1");
    assert.equal(lib.keyFace(A.channelnext, {}, { ...ONLINE, channel: undefined }).label, "");
  });

  it("volume keys show the percentage, '?' when unknown, warn colour while deafened", () => {
    assert.equal(lib.keyFace(A.volup, {}, ONLINE).label, "100 %");
    assert.equal(lib.keyFace(A.voldown, {}, { ...ONLINE, volume: 0 }).label, "0 %");
    assert.equal(lib.keyFace(A.volup, {}, { ...ONLINE, volume: undefined }).label, "? %");
    assert.equal(lib.keyFace(A.volup, {}, ONLINE).glyph, "🔊");
    assert.equal(lib.keyFace(A.voldown, {}, ONLINE).glyph, "🔉");
    assert.equal(lib.keyFace(A.volup, {}, { ...ONLINE, deafened: true }).accent, lib.C.warn);
    assert.equal(lib.keyFace(A.volup, {}, ONLINE).accent, lib.C.signal);
  });

  it("rekey is dim when not connected and active while a rotation runs", () => {
    assert.deepEqual(lib.keyFace(A.rekey, {}, ONLINE), { glyph: "🔑", label: "Rekey", accent: lib.C.signal, active: false, dead: false });
    assert.equal(lib.keyFace(A.rekey, {}, { ...ONLINE, rekeying: true }).label, "läuft…");
    assert.equal(lib.keyFace(A.rekey, {}, { ...ONLINE, rekeying: true }).active, true);
    assert.equal(lib.keyFace(A.rekey, {}, { ...ONLINE, connected: false }).dead, true);
  });

  it("disconnect reads Verlassen in a session and getrennt (dim) outside one", () => {
    assert.deepEqual(lib.keyFace(A.disconnect, {}, ONLINE), { glyph: "⏻", label: "Verlassen", accent: lib.C.tx, active: false, dead: false });
    const off = lib.keyFace(A.disconnect, {}, { ...ONLINE, connected: false });
    assert.equal(off.label, "getrennt");
    assert.equal(off.dead, true);
  });

  it("an empty snapshot (hello without state) renders as online-but-unknown, not offline", () => {
    const f = lib.keyFace(A.ptt, {}, {});
    assert.equal(f.label, "Funk");
    assert.equal(f.dead, false);
  });

  it("returns null for an unknown action", () => {
    assert.equal(lib.keyFace("org.raumdock.subraum.nope", {}, ONLINE), null);
  });
});

describe("keyImage", () => {
  it("is the SVG data URI of the face", () => {
    const uri = lib.keyImage(A.ptt, {}, { ...ONLINE, transmitting: true });
    const svg = svgOf(uri);
    assert.ok(svg.includes(">SENDET</text>"));
    assert.ok(svg.includes(`stroke="${lib.C.tx}"`));
  });

  it("returns null for an unknown action so the caller sends nothing", () => {
    assert.equal(lib.keyImage("nope", {}, ONLINE), null);
  });

  it("re-renders differently when the state changes (the harness' appear → TX check)", () => {
    const a = lib.keyImage(A.ptt, {}, ONLINE);
    const b = lib.keyImage(A.ptt, {}, { ...ONLINE, transmitting: true });
    assert.notEqual(a, b);
  });
});
