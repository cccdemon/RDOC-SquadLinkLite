import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, emitTo } from "@tauri-apps/api/event";
import { getVersion } from "@tauri-apps/api/app";
import { currentMonitor, LogicalPosition, LogicalSize } from "@tauri-apps/api/window";
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import logo from "./Squad_Link_Lite.png";

const REPO = "cccdemon/RDOC-SquadLinkLite";

// Cap retained chat lines so a flood of messages can't grow webview memory.
const MAX_CHAT_LINES = 500;

// Parse "0.1.10" → [0,1,10]; true if `a` is a newer version than `b`.
function isNewer(a: string, b: string): boolean {
  const pa = a.split(".").map(Number);
  const pb = b.split(".").map(Number);
  for (let i = 0; i < 3; i++) {
    if ((pa[i] || 0) !== (pb[i] || 0)) return (pa[i] || 0) > (pb[i] || 0);
  }
  return false;
}

// Newest CHANGELOG section: from the first "## " heading to the next one.
function topChangelogSection(md: string): string {
  const start = md.indexOf("## ");
  if (start < 0) return "";
  const next = md.indexOf("\n## ", start + 3);
  return md.slice(start, next < 0 ? undefined : next).trim();
}

// Render markdown release notes as readable plain text.
function mdToText(s: string): string {
  return s
    .replace(/^#{1,6}\s*/gm, "")
    .replace(/^\s*[-*]\s+/gm, "• ")
    .replace(/\*\*(.*?)\*\*/g, "$1")
    .replace(/`/g, "")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

// Friendly label for raw input codes (e.g. "F8", "KeyR", "Mouse:Unknown(1)").
// Label one base key (no modifiers).
function baseLabel(code: string): string {
  if (code.startsWith("Pad:")) return `Gamepad-Taste ${code.slice(4)}`;
  if (code.startsWith("Mouse:")) {
    const b = code.slice(6);
    const wheel: Record<string, string> = {
      WheelUp: "Mausrad ▲", WheelDown: "Mausrad ▼",
      WheelLeft: "Mausrad ◀", WheelRight: "Mausrad ▶",
    };
    if (wheel[b]) return wheel[b];
    const m = b.match(/Unknown\((\d+)\)/);
    if (m) return `Maustaste ${Number(m[1]) + 3}`; // Unknown(1)→Mouse4
    return `Maus ${b}`;
  }
  return code.replace(/^Key/, "");
}

// Label a binding, including modifier chords ("Shift+KeyT" → "Shift + T").
function pttLabel(code: string): string {
  if (!code) return "—";
  const mods = new Set(["Ctrl", "Alt", "Shift", "Meta"]);
  const parts = code.split("+");
  const base = parts.pop() ?? "";
  const prefix = parts.filter((p) => mods.has(p));
  return [...prefix, baseLabel(base)].join(" + ");
}

type Participant = {
  user_id: string;
  name: string;
  you: boolean;
  badge: string | null;
  speaking: boolean;
  channel: string;
};
type ChatLine = { from: string; text: string };

type UiEvent =
  | { type: "roster"; participants: Participant[] }
  | { type: "chat"; from: string; text: string }
  | { type: "status"; connected: boolean; transmitting: boolean }
  | { type: "log"; text: string }
  | { type: "net"; peers: number; up_kbps: number; down_kbps: number }
  | { type: "rekeyed"; generation: number; by: string }
  | { type: "signaling"; up: boolean }
  | { type: "channel"; mine: string }
  | { type: "channels"; names: string[] }
  | { type: "room_audio"; gen: number | null; authority: boolean };

const DEFAULT_CHANNEL = "Funk 1";
const MAX_CHANNEL_LEN = 32;
// Canonical form for channel matching: trim + lowercase (mirrors the Rust side).
const canonChannel = (s: string) => s.trim().toLowerCase();

// ── Channel overlay (separate transparent, click-through window) ──────────────
type OverlaySize = "s" | "m" | "l";
type OverlayPos = "tl" | "tc" | "tr" | "bl" | "bc" | "br";
const OVERLAY_DIMS: Record<OverlaySize, { w: number; h: number }> = {
  s: { w: 180, h: 40 },
  m: { w: 224, h: 52 },
  l: { w: 288, h: 64 },
};
const OVERLAY_POSITIONS: { key: OverlayPos; label: string; title: string }[] = [
  { key: "tl", label: "◤", title: "oben links" },
  { key: "tc", label: "▲", title: "oben mitte" },
  { key: "tr", label: "◥", title: "oben rechts" },
  { key: "bl", label: "◣", title: "unten links" },
  { key: "bc", label: "▼", title: "unten mitte" },
  { key: "br", label: "◢", title: "unten rechts" },
];
const OVERLAY_MARGIN = 24;

// Create-if-needed / size / position (or destroy) the overlay window. The window
// is created lazily only while enabled and fully closed when disabled, so it
// costs no RAM/CPU when off. Logical coordinates; anchored to a corner/center of
// the current monitor with a small margin.
async function applyOverlayWindow(on: boolean, pos: OverlayPos, size: OverlaySize) {
  let w = await WebviewWindow.getByLabel("overlay");
  if (!on) {
    if (w) await w.close().catch(() => {}); // destroy → frees the WebView2 process
    return;
  }
  const d = OVERLAY_DIMS[size];
  if (!w) {
    w = new WebviewWindow("overlay", {
      url: "index.html",
      title: "SquadLink Overlay",
      width: d.w,
      height: d.h,
      transparent: true,
      decorations: false,
      alwaysOnTop: true,
      skipTaskbar: true,
      resizable: false,
      focus: false,
      shadow: false,
      visible: false,
    });
    // Wait until the window is actually created before positioning/showing it.
    await new Promise<void>((resolve) => {
      const done = () => resolve();
      w!.once("tauri://created", done);
      w!.once("tauri://error", done);
    });
  }
  await w.setSize(new LogicalSize(d.w, d.h)).catch(() => {});
  const mon = await currentMonitor().catch(() => null);
  const sf = mon?.scaleFactor ?? 1;
  const MW = (mon?.size.width ?? 1920) / sf;
  const MH = (mon?.size.height ?? 1080) / sf;
  const OX = (mon?.position.x ?? 0) / sf;
  const OY = (mon?.position.y ?? 0) / sf;
  const left = OX + OVERLAY_MARGIN;
  const right = OX + MW - d.w - OVERLAY_MARGIN;
  const cx = OX + (MW - d.w) / 2;
  const top = OY + OVERLAY_MARGIN;
  const bottom = OY + MH - d.h - OVERLAY_MARGIN;
  const map: Record<OverlayPos, [number, number]> = {
    tl: [left, top], tc: [cx, top], tr: [right, top],
    bl: [left, bottom], bc: [cx, bottom], br: [right, bottom],
  };
  const [x, y] = map[pos];
  await w.setPosition(new LogicalPosition(Math.round(x), Math.round(y))).catch(() => {});
  await w.setAlwaysOnTop(true).catch(() => {});
  await w.setIgnoreCursorEvents(true).catch(() => {});
  await w.show().catch(() => {});
}

// Capture-path DSP (must match companion_core::audio::DspConfig field names).
type DspConfig = {
  gate: boolean;
  gate_threshold: number;
  compressor: boolean;
  comp_threshold: number;
  comp_ratio: number;
  comp_makeup: number;
  limiter: boolean;
  limiter_ceiling: number;
};
const DSP_DEFAULT: DspConfig = {
  gate: true,
  gate_threshold: 0.015,
  compressor: true,
  comp_threshold: 0.15,
  comp_ratio: 3.0,
  comp_makeup: 1.4,
  limiter: true,
  limiter_ceiling: 0.97,
};

export default function App() {
  const [connected, setConnected] = useState(false);
  const [transmitting, setTransmitting] = useState(false);
  const [participants, setParticipants] = useState<Participant[]>([]);
  // My current channel (frequency); persisted and re-applied on each connect.
  const [myChannel, setMyChannel] = useState<string>(() => {
    try {
      return localStorage.getItem("sa.channel") || DEFAULT_CHANNEL;
    } catch {
      return DEFAULT_CHANNEL;
    }
  });
  const [channelDraft, setChannelDraft] = useState<string>("");
  // Channels known this session: every channel I create or that a peer announces
  // stays in the switcher for the whole session, even after everyone leaves it
  // (a created channel doesn't vanish when its roster count hits zero).
  const [sessionChannels, setSessionChannels] = useState<string[]>([]);
  const rememberChannel = (name: string) => {
    const clean = name.trim().slice(0, MAX_CHANNEL_LEN);
    if (!clean) return;
    setSessionChannels((prev) =>
      prev.some((c) => canonChannel(c) === canonChannel(clean)) ? prev : [...prev, clean]
    );
  };
  // Remove a channel from the session list. Only meaningful for an empty channel
  // (one with a live member re-appears from the roster union).
  const deleteChannel = (canon: string) => {
    if (canon === canonChannel(DEFAULT_CHANNEL)) return; // base channel is permanent
    // Refuse if anyone is currently on it (roster still shows members there).
    if (participants.some((p) => canonChannel(p.channel) === canon)) return;
    setSessionChannels((prev) => prev.filter((c) => canonChannel(c) !== canon));
  };
  // Streamer mode: blur the shareable link + PIN so they can't be read on stream.
  // Copy buttons still copy the real values.
  const [streamerMode, setStreamerMode] = useState<boolean>(() => {
    try {
      return localStorage.getItem("sa.streamer") === "1";
    } catch {
      return false;
    }
  });
  const toggleStreamer = () =>
    setStreamerMode((v) => {
      const nv = !v;
      try { localStorage.setItem("sa.streamer", nv ? "1" : "0"); } catch { /* ignore */ }
      return nv;
    });
  const [chat, setChat] = useState<ChatLine[]>([]);
  const [log, setLog] = useState("");
  const [connecting, setConnecting] = useState(false);
  const [form, setForm] = useState(() => {
    try {
      const s = localStorage.getItem("sa.form");
      if (s) return JSON.parse(s);
    } catch {
      /* ignore */
    }
    return { name: "" };
  });
  const [msg, setMsg] = useState("");
  const chatEnd = useRef<HTMLDivElement>(null);

  // Session brokering (PIN-protected link).
  const [sessionInfo, setSessionInfo] = useState<{ link: string; pin: string; code: string } | null>(null);
  const [joinInput, setJoinInput] = useState("");
  const [joinPin, setJoinPin] = useState("");

  // Audio settings (gear): device choice + volumes.
  const [showSettings, setShowSettings] = useState(false);
  const [devices, setDevices] = useState<{ inputs: string[]; outputs: string[] }>({ inputs: [], outputs: [] });
  const [audioCfg, setAudioCfg] = useState<{ input: string; output: string }>(() => {
    try {
      const s = localStorage.getItem("sa.audio");
      if (s) return JSON.parse(s);
    } catch {
      /* ignore */
    }
    return { input: "", output: "" };
  });
  const [masterVol, setMasterVol] = useState(100); // percent
  const [peerVol, setPeerVol] = useState<Record<string, number>>({});
  const [net, setNet] = useState<{ peers: number; up: number; down: number } | null>(null);
  // Group-audio encryption: gen=null while negotiating the room key, else the
  // installed key generation; `authority` = this client mints/rotates the key.
  const [roomAudio, setRoomAudio] = useState<{ gen: number | null; authority: boolean }>({ gen: null, authority: false });
  const [keyInfo, setKeyInfo] = useState<{ gen: number; at: number }>({ gen: 1, at: 0 });
  const [rotating, setRotating] = useState(false);
  const [sigUp, setSigUp] = useState(true);
  const [resuming, setResuming] = useState(false);
  const [micMuted, setMicMuted] = useState(false);
  const micMutedRef = useRef(false);
  const [deaf, setDeaf] = useState(false);
  const [dsp, setDsp] = useState<DspConfig>(() => {
    try {
      return { ...DSP_DEFAULT, ...JSON.parse(localStorage.getItem("sa.dsp") || "{}") };
    } catch {
      return DSP_DEFAULT;
    }
  });
  const [monitoring, setMonitoring] = useState(false);
  const [netCheck, setNetCheck] = useState<{ signaling: boolean; can_send: boolean; can_receive: boolean; stun: boolean } | null>(null);
  const [checking, setChecking] = useState(false);
  const [settingsTab, setSettingsTab] = useState<"simple" | "expert">("simple");
  // Channel overlay + cycle hotkeys.
  const [overlayOn, setOverlayOn] = useState<boolean>(() => {
    try { return localStorage.getItem("sa.ovl") === "1"; } catch { return false; }
  });
  const [overlayPos, setOverlayPos] = useState<OverlayPos>(() => {
    try { return (localStorage.getItem("sa.ovlpos") as OverlayPos) || "tc"; } catch { return "tc"; }
  });
  const [overlaySize, setOverlaySize] = useState<OverlaySize>(() => {
    try { return (localStorage.getItem("sa.ovlsize") as OverlaySize) || "m"; } catch { return "m"; }
  });
  const [chanPrev, setChanPrev] = useState<string>(() => {
    try { return localStorage.getItem("sa.chanprev") || ""; } catch { return ""; }
  });
  const [chanNext, setChanNext] = useState<string>(() => {
    try { return localStorage.getItem("sa.channext") || ""; } catch { return ""; }
  });
  const [capturingChan, setCapturingChan] = useState<number | null>(null);
  const [showKbps, setShowKbps] = useState<boolean>(() => {
    try {
      return localStorage.getItem("sa.showkbps") !== "0"; // default: show
    } catch {
      return true;
    }
  });
  const [showRekeyBtn, setShowRekeyBtn] = useState<boolean>(() => {
    try {
      return localStorage.getItem("sa.showrekey") !== "0"; // default: show
    } catch {
      return true;
    }
  });
  const [lowBw, setLowBw] = useState<boolean>(() => {
    try {
      return localStorage.getItem("sa.lowbw") === "1";
    } catch {
      return false;
    }
  });
  const [relayFb, setRelayFb] = useState<boolean>(() => {
    try {
      return localStorage.getItem("sa.relayFallback") === "1"; // default OFF (serverless)
    } catch {
      return false;
    }
  });
  // Fleetplanner-Modus: opt-in manual direct-link entry (squadlink:// deep links
  // always auto-connect regardless of this toggle).
  const [fpMode, setFpMode] = useState<boolean>(() => {
    try {
      return localStorage.getItem("sa.fpmode") === "1";
    } catch {
      return false;
    }
  });
  const [directLink, setDirectLink] = useState("");
  const [appVersion, setAppVersion] = useState("");
  // MSIX/Store build → the Store updates the app, so the self-update prompt is
  // hidden (Store policy). Detected once at startup via Rust.
  const [storeBuild, setStoreBuild] = useState(false);
  const [update, setUpdate] = useState<{ version: string; notes: string } | null>(null);
  const [showUpdate, setShowUpdate] = useState(true);
  const [pttBinding, setPttBinding] = useState<string>(() => {
    try {
      return localStorage.getItem("sa.ptt") || "F8";
    } catch {
      return "F8";
    }
  });
  // Optional second PTT trigger (key / mouse / gamepad). Holding either transmits.
  const [pttBinding2, setPttBinding2] = useState<string>(() => {
    try {
      return localStorage.getItem("sa.ptt2") || "";
    } catch {
      return "";
    }
  });
  // null = idle; 0 or 1 = currently capturing a new binding for that slot.
  const [capturingSlot, setCapturingSlot] = useState<number | null>(null);
  // Auto-duck other apps (game etc.) while SquadLink voice is active (Windows).
  const [duckOthers, setDuckOthers] = useState<boolean>(() => {
    try {
      return localStorage.getItem("sa.duck") !== "0";
    } catch {
      return true;
    }
  });
  // How much quieter other apps get while voice is active, in percent.
  const [duckAmount, setDuckAmount] = useState<number>(() => {
    try {
      const v = Number(localStorage.getItem("sa.duckAmount"));
      return Number.isFinite(v) && v >= 0 && v <= 100 ? v : 75;
    } catch {
      return 75;
    }
  });
  // Local "Funk-Klick" earcon at the start of an incoming transmission, so you can
  // hear that audio is coming from SquadLink. Default on.
  const [earcon, setEarcon] = useState<boolean>(() => {
    try {
      return localStorage.getItem("sa.earcon") !== "0";
    } catch {
      return true;
    }
  });
  // Funk-Klick volume, 0..200 % (1.0 = normal). Default 100 %.
  const [earconVol, setEarconVol] = useState<number>(() => {
    try {
      const v = parseInt(localStorage.getItem("sa.earconvol") || "100", 10);
      return Number.isFinite(v) ? Math.min(200, Math.max(0, v)) : 100;
    } catch {
      return 100;
    }
  });
  const onEarconVol = (v: number) => {
    setEarconVol(v);
    try { localStorage.setItem("sa.earconvol", String(v)); } catch { /* ignore */ }
    invoke("set_earcon_volume", { volume: v / 100 }).catch(() => {});
  };

  // Load device list once (for the gear settings).
  useEffect(() => {
    invoke<[string[], string[]]>("list_audio_devices")
      .then(([inputs, outputs]) => setDevices({ inputs, outputs }))
      .catch(() => {});
  }, []);
  const saveAudioCfg = (next: { input: string; output: string }) => {
    setAudioCfg(next);
    try {
      localStorage.setItem("sa.audio", JSON.stringify(next));
    } catch {
      /* ignore */
    }
  };
  const onMaster = (v: number) => {
    setMasterVol(v);
    invoke("set_master_volume", { volume: deaf ? 0 : v / 100 }).catch(() => {});
  };
  // Self-mute mic: stop sending now + gate PTT (I still hear everyone).
  const toggleMic = () => {
    setMicMuted((m) => {
      const nv = !m;
      micMutedRef.current = nv;
      if (nv) invoke("set_transmit", { on: false }).catch(() => {});
      return nv;
    });
  };
  // Deafen: mute all output without losing the slider value.
  const toggleDeaf = () => {
    setDeaf((d) => {
      const nv = !d;
      invoke("set_master_volume", { volume: nv ? 0 : masterVol / 100 }).catch(() => {});
      return nv;
    });
  };
  const toggleLowBw = () => {
    setLowBw((v) => {
      const nv = !v;
      try {
        localStorage.setItem("sa.lowbw", nv ? "1" : "0");
      } catch {
        /* ignore */
      }
      invoke("set_low_bandwidth", { on: nv }).catch(() => {});
      return nv;
    });
  };
  const runNetCheck = () => {
    setChecking(true);
    setNetCheck(null);
    invoke<{ signaling: boolean; can_send: boolean; can_receive: boolean; stun: boolean }>("net_selfcheck", {
      server: "wss://squadlink.raumdock.org/ws",
    })
      .then((r) => setNetCheck(r))
      .catch(() => setNetCheck(null))
      .finally(() => setChecking(false));
  };
  const toggleKbps = () => {
    setShowKbps((v) => {
      const nv = !v;
      try {
        localStorage.setItem("sa.showkbps", nv ? "1" : "0");
      } catch {
        /* ignore */
      }
      return nv;
    });
  };
  const toggleRekeyBtn = () => {
    setShowRekeyBtn((v) => {
      const nv = !v;
      try {
        localStorage.setItem("sa.showrekey", nv ? "1" : "0");
      } catch {
        /* ignore */
      }
      return nv;
    });
  };
  const toggleMonitor = () => {
    setMonitoring((m) => {
      const nv = !m;
      invoke("set_monitor", { on: nv }).catch(() => {});
      return nv;
    });
  };
  const onDisconnect = () => {
    invoke("disconnect").catch(() => {});
    setMonitoring(false);
    setMicMuted(false);
    micMutedRef.current = false;
    setDeaf(false);
    setShowSettings(false);
    // The engine emits Status{connected:false}, which returns us to the start screen.
  };
  const updateDsp = (patch: Partial<DspConfig>) => {
    setDsp((d) => {
      const nv = { ...d, ...patch };
      try {
        localStorage.setItem("sa.dsp", JSON.stringify(nv));
      } catch {
        /* ignore */
      }
      invoke("set_dsp", { cfg: nv }).catch(() => {});
      return nv;
    });
  };
  const onPeerVol = (userId: string, v: number) => {
    setPeerVol((m) => ({ ...m, [userId]: v }));
    invoke("set_peer_volume", { userId, volume: v / 100 }).catch(() => {});
  };

  useEffect(() => {
    const un = listen<UiEvent>("ui", (e) => {
      const p = e.payload;
      if (p.type === "roster") setParticipants(p.participants);
      else if (p.type === "chat") setChat((c) => {
        const next = [...c, { from: p.from, text: p.text }];
        return next.length > MAX_CHAT_LINES ? next.slice(next.length - MAX_CHAT_LINES) : next;
      });
      else if (p.type === "status") {
        setConnected(p.connected);
        setTransmitting(p.transmitting);
        if (p.connected) {
          setConnecting(false);
          setSigUp(true);
        }
      } else if (p.type === "log") setLog(p.text);
      else if (p.type === "net") setNet({ peers: p.peers, up: p.up_kbps, down: p.down_kbps });
      else if (p.type === "rekeyed") {
        setKeyInfo({ gen: p.generation, at: Date.now() });
        setRotating(false);
        setLog(`🔑 Schlüssel rotiert (Generation #${p.generation}${p.by ? `, durch ${p.by}` : ""})`);
      } else if (p.type === "signaling") {
        setSigUp(p.up);
        if (p.up) setResuming(false);
      } else if (p.type === "channel") setMyChannel(p.mine);
      else if (p.type === "channels") p.names.forEach(rememberChannel);
      else if (p.type === "room_audio") setRoomAudio({ gen: p.gen, authority: p.authority });
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  useEffect(() => {
    chatEnd.current?.scrollIntoView({ behavior: "smooth" });
  }, [chat]);

  // Configurable PTT via Windows Raw Input (Rust). The bound key/mouse button
  // emits "ptt" (down/up); "ptt-bound" fires after a rebind capture.
  useEffect(() => {
    invoke("set_ptt_binding", { slot: 0, code: pttBinding }).catch(() => {});
    invoke("set_ptt_binding", { slot: 1, code: pttBinding2 || null }).catch(() => {});
    const offPtt = listen<boolean>("ptt", (e) => {
      if (micMutedRef.current) return; // self-muted: ignore push-to-talk
      invoke("set_transmit", { on: e.payload }).catch(() => {});
    });
    const offBound = listen<{ slot: number; code: string }>("ptt-bound", (e) => {
      const { slot, code } = e.payload;
      setCapturingSlot(null);
      if (slot === 1) {
        setPttBinding2(code);
        try { localStorage.setItem("sa.ptt2", code); } catch { /* ignore */ }
      } else {
        setPttBinding(code);
        try { localStorage.setItem("sa.ptt", code); } catch { /* ignore */ }
      }
    });
    return () => {
      offPtt.then((f) => f());
      offBound.then((f) => f());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  useEffect(() => {
    getVersion().then(setAppVersion).catch(() => {});
    invoke<boolean>("is_store_build").then(setStoreBuild).catch(() => {});
  }, []);

  // Update check: compare the newest GitHub release (prereleases included) to the
  // running version; if newer, surface it with the changelog. Skipped in Store
  // builds — the Microsoft Store handles updates (self-update violates policy).
  useEffect(() => {
    if (storeBuild) return;
    (async () => {
      try {
        const cur = await getVersion();
        const r = await fetch(`https://api.github.com/repos/${REPO}/releases?per_page=30`, {
          headers: { Accept: "application/vnd.github+json" },
        });
        const rels = await r.json();
        if (!Array.isArray(rels)) return;
        // The REST /releases order (created_at) is unreliable for force-pushed
        // tags — pick the highest semver ourselves instead of trusting [0].
        let lv: string | undefined;
        let body = "";
        for (const x of rels as { draft?: boolean; tag_name?: string; body?: string }[]) {
          if (x.draft) continue;
          const v = x.tag_name?.match(/(\d+\.\d+\.\d+)/)?.[1];
          if (v && (!lv || isNewer(v, lv))) {
            lv = v;
            body = x.body || "";
          }
        }
        if (!lv || !isNewer(lv, cur)) return;
        let notes: string = body;
        try {
          const cl = await fetch(`https://raw.githubusercontent.com/${REPO}/main/CHANGELOG.md`);
          notes = topChangelogSection(await cl.text()) || notes;
        } catch {
          /* fall back to release body */
        }
        setUpdate({ version: lv, notes: mdToText(notes) });
        setShowUpdate(true);
      } catch {
        /* offline / API error: silently skip */
      }
    })();
  }, [storeBuild]);

  // Push saved DSP settings to the engine once connected.
  useEffect(() => {
    if (connected) {
      invoke("set_dsp", { cfg: dsp }).catch(() => {});
      invoke("set_low_bandwidth", { on: lowBw }).catch(() => {});
      invoke("set_earcon", { on: earcon }).catch(() => {});
      invoke("set_earcon_volume", { volume: earconVol / 100 }).catch(() => {});
      // Fresh session → start the channel list from just my channel; peers'
      // channels + any I create get added as they're seen.
      setSessionChannels([myChannel]);
      // Re-apply the saved channel (engine starts on the default).
      if (canonChannel(myChannel) !== canonChannel(DEFAULT_CHANNEL)) {
        invoke("set_channel", { name: myChannel }).catch(() => {});
      }
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [connected]);

  // Persist the channel whenever it changes (switch or engine confirmation).
  useEffect(() => {
    try { localStorage.setItem("sa.channel", myChannel); } catch { /* ignore */ }
  }, [myChannel]);

  // Keep the session's channel list growing: remember my channel + every channel
  // a peer announces, so created channels stay selectable all session.
  useEffect(() => {
    rememberChannel(myChannel);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [myChannel]);
  useEffect(() => {
    participants.forEach((p) => rememberChannel(p.channel));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [participants]);

  // Switch to a (validated) channel: optimistic UI + tell the engine.
  const switchChannel = (name: string) => {
    const clean = name.trim().slice(0, MAX_CHANNEL_LEN);
    if (!clean) return;
    rememberChannel(clean); // keep even a brand-new channel in the list right away
    if (canonChannel(clean) === canonChannel(myChannel)) return;
    setMyChannel(clean);
    invoke("set_channel", { name: clean }).catch(() => {});
  };

  // Ordered, deduped channel list (session-remembered + mine + live peers) —
  // the order the cycle hotkeys and the chips walk through.
  const orderedChannels = () => {
    const seen = new Set<string>();
    const out: string[] = [];
    for (const label of [...sessionChannels, myChannel, ...participants.map((p) => p.channel)]) {
      const k = canonChannel(label);
      if (!k || seen.has(k)) continue;
      seen.add(k);
      out.push(label);
    }
    return out;
  };
  // Cycle prev (-1) / next (+1). Kept in a ref so the (once-registered) global
  // hotkey listener always sees the current channel list, not a stale closure.
  const cycleRef = useRef<(dir: number) => void>(() => {});
  cycleRef.current = (dir: number) => {
    const list = orderedChannels();
    if (list.length < 2) return;
    const idx = list.findIndex((c) => canonChannel(c) === canonChannel(myChannel));
    const cur = idx < 0 ? 0 : idx;
    const nextIdx = (cur + (dir < 0 ? -1 : 1) + list.length) % list.length;
    switchChannel(list[nextIdx]);
  };

  // Latest overlay state for the handshake below (a freshly-created overlay
  // window asks for the current channel/size once its listener is ready).
  const overlayStateRef = useRef<{ channel: string; size: OverlaySize }>({ channel: myChannel, size: overlaySize });
  overlayStateRef.current = { channel: myChannel, size: overlaySize };

  // Apply the overlay window (show/size/position or hide) + persist the choice.
  useEffect(() => {
    applyOverlayWindow(overlayOn, overlayPos, overlaySize);
    try {
      localStorage.setItem("sa.ovl", overlayOn ? "1" : "0");
      localStorage.setItem("sa.ovlpos", overlayPos);
      localStorage.setItem("sa.ovlsize", overlaySize);
    } catch { /* ignore */ }
    if (overlayOn) emitTo("overlay", "overlay-update", { channel: myChannel, size: overlaySize }).catch(() => {});
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [overlayOn, overlayPos, overlaySize]);
  // Push the current channel to the overlay whenever it changes.
  useEffect(() => {
    if (overlayOn) emitTo("overlay", "overlay-update", { channel: myChannel, size: overlaySize }).catch(() => {});
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [myChannel]);

  // Channel-cycle hotkeys: push saved bindings + listen for global press edges.
  useEffect(() => {
    invoke("set_chan_binding", { slot: 0, code: chanPrev || null }).catch(() => {});
    invoke("set_chan_binding", { slot: 1, code: chanNext || null }).catch(() => {});
    const offCycle = listen<number>("chan-cycle", (e) => cycleRef.current(e.payload));
    const offBound = listen<{ slot: number; code: string }>("chan-bound", (e) => {
      const { slot, code } = e.payload;
      if (slot === 0) { setChanPrev(code); try { localStorage.setItem("sa.chanprev", code); } catch { /* ignore */ } }
      else { setChanNext(code); try { localStorage.setItem("sa.channext", code); } catch { /* ignore */ } }
      setCapturingChan(null);
    });
    // A freshly-created overlay window announces itself → send current state.
    const offReady = listen("overlay-ready", () => {
      emitTo("overlay", "overlay-update", overlayStateRef.current).catch(() => {});
    });
    return () => { offCycle.then((f) => f()); offBound.then((f) => f()); offReady.then((f) => f()); };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  const rebindChan = (slot: number) => {
    setCapturingChan(slot);
    invoke("start_chan_capture", { slot }).catch(() => {});
  };
  const clearChan = (slot: number) => {
    if (slot === 0) { setChanPrev(""); try { localStorage.removeItem("sa.chanprev"); } catch { /* ignore */ } }
    else { setChanNext(""); try { localStorage.removeItem("sa.channext"); } catch { /* ignore */ } }
    setCapturingChan(null);
    invoke("set_chan_binding", { slot, code: null }).catch(() => {});
  };
  const rebindPtt = (slot: number) => {
    setCapturingSlot(slot);
    invoke("start_ptt_capture", { slot }).catch(() => {});
  };
  const clearPtt2 = () => {
    setPttBinding2("");
    setCapturingSlot(null);
    invoke("set_ptt_binding", { slot: 1, code: null }).catch(() => {});
    try { localStorage.removeItem("sa.ptt2"); } catch { /* ignore */ }
  };
  // Duck other apps while I transmit or any peer is speaking; restore on silence.
  const duckActive = duckOthers && (transmitting || participants.some((p) => !p.you && p.speaking));
  const duckRef = useRef(false);
  useEffect(() => {
    if (duckRef.current !== duckActive) {
      duckRef.current = duckActive;
      invoke("set_ducking", { active: duckActive }).catch(() => {});
    }
  }, [duckActive]);
  // Push the amount on mount too — the backend defaults to 75 and would otherwise
  // ignore a stored setting until the slider is touched.
  useEffect(() => {
    invoke("set_duck_amount", { percent: duckAmount }).catch(() => {});
  }, [duckAmount]);
  const rotateKey = () => {
    setRotating(true);
    invoke("rotate_key").catch(() => setRotating(false));
    // safety: clear the spinner even if no rekeyed event arrives
    setTimeout(() => setRotating(false), 8000);
  };
  const resumeSession = () => {
    setResuming(true);
    invoke("reconnect_session").catch(() => setResuming(false));
    setTimeout(() => setResuming(false), 8000);
  };

  const copy = (t: string) => navigator.clipboard?.writeText(t);

  // ── Session brokering (PIN-protected link via InitConnection REST) ──────────
  // The session service is the hosted public endpoint.
  const SESSION_BASE = "https://squadlink.raumdock.org";
  const parseCode = (s: string) => {
    const t = s.trim();
    const m = t.match(/\/j\/([A-Za-z0-9]+)/);
    return m ? m[1] : t;
  };
  const baseFromInput = (input: string) => {
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
  };
  const connectWith = async (ws: string, room: string, token: string | null, nameOverride?: string, userIdOverride?: string) => {
    const name = (nameOverride ?? form.name).trim() || "Commander";
    if (nameOverride) setForm((f: any) => ({ ...f, name }));
    // Stable identity. From a deep link this is the player's Discord name; sanitize
    // to the id charset the backend accepts ([A-Za-z0-9_.-]). Fallback: random.
    const uid = (userIdOverride || "").replace(/[^A-Za-z0-9_.-]/g, "").slice(0, 64);
    try {
      localStorage.setItem("sa.form", JSON.stringify({ ...form, name }));
    } catch {
      /* ignore */
    }
    await invoke("connect", {
      server: ws,
      room,
      userId: uid || crypto.randomUUID().slice(0, 8),
      name,
      token: token || null,
      certSha256: null,
      inputDevice: audioCfg.input || null,
      outputDevice: audioCfg.output || null,
      relayEnabled: relayFb,
    });
  };
  const toggleRelayFb = () => {
    setRelayFb((v) => {
      const nv = !v;
      try {
        localStorage.setItem("sa.relayFallback", nv ? "1" : "0");
      } catch {
        /* ignore */
      }
      return nv;
    });
  };
  const createSession = async () => {
    setConnecting(true);
    setLog("");
    setChat([]);
    try {
      const r = await fetch(`${SESSION_BASE}/session`, { method: "POST" });
      if (!r.ok) throw new Error("Server " + r.status);
      const j = await r.json();
      setSessionInfo({ link: j.link, pin: j.pin, code: j.code });
      await connectWith(j.ws, j.room, j.token);
    } catch (e) {
      setLog(String(e));
      setConnecting(false);
    }
  };
  const joinSession = async () => {
    setConnecting(true);
    setLog("");
    setChat([]);
    try {
      const code = parseCode(joinInput);
      const base = baseFromInput(joinInput);
      const r = await fetch(`${base}/session/${encodeURIComponent(code)}/join`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ pin: joinPin.trim() }),
      });
      if (r.status === 403) throw new Error("Falsche PIN");
      if (r.status === 429) throw new Error("Zu viele Versuche — Session gesperrt");
      if (r.status === 404) throw new Error("Session nicht gefunden / abgelaufen");
      if (!r.ok) throw new Error("Server " + r.status);
      const j = await r.json();
      // Let a joiner re-share the EXACT same session: same code + same PIN.
      setSessionInfo({ link: `${base}/j/${code}`, pin: joinPin.trim(), code });
      await connectWith(j.ws, j.room, j.token);
    } catch (e) {
      setLog(String(e));
      setConnecting(false);
    }
  };

  // ── Direct-link config (Fleetplanner / squadlink:// deep link) ──────────────
  // Format: squadlink://connect?ws=<wss url>&room=<id>&token=<hex>&name=<>&uid=<>
  // Carries the full creds so neither link nor PIN entry is needed. `uid` is the
  // player's stable identity (e.g. Discord name); `name` is the display name.
  const parseDirectLink = (raw: string): { ws: string; room: string; token: string | null; name?: string; uid?: string } | null => {
    try {
      const u = new URL(raw.trim());
      if (u.protocol !== "squadlink:") return null;
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
  };
  const connectDirect = async (raw: string) => {
    const cfg = parseDirectLink(raw);
    if (!cfg) {
      setLog("Ungültiger SquadLink-Direktlink");
      return;
    }
    setConnecting(true);
    setLog("");
    setChat([]);
    setSessionInfo(null);
    try {
      await connectWith(cfg.ws, cfg.room, cfg.token, cfg.name, cfg.uid);
    } catch (e) {
      setLog(String(e));
      setConnecting(false);
    }
  };
  const toggleFpMode = () => {
    setFpMode((v) => {
      const nv = !v;
      try {
        localStorage.setItem("sa.fpmode", nv ? "1" : "0");
      } catch {
        /* ignore */
      }
      return nv;
    });
  };

  // squadlink:// deep link (from Rust) → auto-connect with the carried creds.
  const connectDirectRef = useRef(connectDirect);
  connectDirectRef.current = connectDirect;
  useEffect(() => {
    const off = listen<string>("deeplink", (e) => connectDirectRef.current(e.payload));
    // Cold start: app launched by a squadlink:// link (event fired before this
    // listener existed) → drain the stashed URL from Rust.
    invoke<string | null>("take_pending_deeplink")
      .then((url) => {
        if (url) connectDirectRef.current(url);
      })
      .catch(() => {});
    return () => {
      off.then((f) => f());
    };
  }, []);

  const ptt = () => invoke("toggle_transmit");
  const send = () => {
    const t = msg.trim();
    if (t) {
      invoke("send_chat", { text: t });
      setMsg("");
    }
  };

  const deviceSettings = (
    <div
      className="settings-overlay"
      onClick={(e) => {
        if (e.target === e.currentTarget) setShowSettings(false);
      }}
    >
    <div className="settings">
      <div className="setshead">
        <b>Einstellungen</b>
        <button className="x" title="Schließen" onClick={() => setShowSettings(false)}>×</button>
      </div>
      <div className="settabs">
        <button className={settingsTab === "simple" ? "on" : ""} onClick={() => setSettingsTab("simple")}>Einfach</button>
        <button className={settingsTab === "expert" ? "on" : ""} onClick={() => setSettingsTab("expert")}>Experte</button>
      </div>

      {settingsTab === "simple" && (
        <>
          <label>🎤 Mikrofon</label>
          <select
            value={audioCfg.input}
            onChange={(e) => {
              saveAudioCfg({ ...audioCfg, input: e.target.value });
              invoke("set_input_device", { name: e.target.value || null }).catch(() => {});
            }}
          >
            <option value="">Standard-Gerät</option>
            {devices.inputs.map((d) => <option key={d} value={d}>{d}</option>)}
          </select>
          <label>🔊 Ausgabe</label>
          <select
            value={audioCfg.output}
            onChange={(e) => {
              saveAudioCfg({ ...audioCfg, output: e.target.value });
              invoke("set_output_device", { name: e.target.value || null }).catch(() => {});
            }}
          >
            <option value="">Standard-Gerät</option>
            {devices.outputs.map((d) => <option key={d} value={d}>{d}</option>)}
          </select>
          <label>🎙 Push-to-Talk</label>
          <div className="pttrow">
            <span className="pttcur">{capturingSlot === 0 ? "Drücke Taste / Maus / Gamepad…" : pttLabel(pttBinding)}</span>
            <button className="btn sm" onClick={() => rebindPtt(0)} disabled={capturingSlot !== null}>Neu belegen</button>
          </div>
          <div className="pttrow">
            <span className="pttcur">{capturingSlot === 1 ? "Drücke Taste / Maus / Gamepad…" : (pttBinding2 ? pttLabel(pttBinding2) : "— (optionale 2. Taste)")}</span>
            <button className="btn sm" onClick={() => rebindPtt(1)} disabled={capturingSlot !== null}>{pttBinding2 ? "Neu belegen" : "2. Taste"}</button>
            {pttBinding2 && <button className="btn sm" onClick={clearPtt2} disabled={capturingSlot !== null} title="Zweite Taste entfernen">✕</button>}
          </div>
          <div className="sub2" style={{ opacity: 0.7 }}>
            Push-to-Talk: jede Taste, Maustaste, Mausrad, Gamepad-/Joystick-Taste (RAW) oder Modifier-Kombi (z.B. Shift+T) — bis zu zwei, beide senden. Geräteänderung wirkt sofort (auch während eines Gesprächs).
          </div>
          <label className="ckrow">
            <input
              type="checkbox"
              checked={duckOthers}
              onChange={(e) => {
                const on = e.target.checked;
                setDuckOthers(on);
                try { localStorage.setItem("sa.duck", on ? "1" : "0"); } catch { /* ignore */ }
                if (!on) invoke("set_ducking", { active: false }).catch(() => {});
              }}
            />{" "}
            Andere Apps automatisch leiser, wenn im SquadLink gesprochen wird (Windows)
          </label>
          <div className="volrow" style={{ opacity: duckOthers ? 1 : 0.45 }}>
            <span className="vlabel">🔉 Um wie viel leiser</span>
            <input
              type="range"
              min={0}
              max={100}
              value={duckAmount}
              disabled={!duckOthers}
              onChange={(e) => {
                const pct = Number(e.target.value);
                setDuckAmount(pct);
                try { localStorage.setItem("sa.duckAmount", String(pct)); } catch { /* ignore */ }
              }}
            />
            <span className="vval">{duckAmount}%</span>
          </div>
          <label className="ckrow">
            <input
              type="checkbox"
              checked={earcon}
              onChange={(e) => {
                const on = e.target.checked;
                setEarcon(on);
                try { localStorage.setItem("sa.earcon", on ? "1" : "0"); } catch { /* ignore */ }
                invoke("set_earcon", { on }).catch(() => {});
              }}
            />{" "}
            Funk-Klick abspielen, wenn jemand zu sprechen beginnt (akustische SquadLink-Kennung)
          </label>
          <div className="volrow" style={{ opacity: earcon ? 1 : 0.45 }}>
            <span className="vlabel">🔔 Klick-Lautstärke</span>
            <input
              type="range"
              min={0}
              max={200}
              value={earconVol}
              disabled={!earcon}
              onChange={(e) => onEarconVol(Number(e.target.value))}
            />
            <span className="vval">{earconVol}%</span>
          </div>

          <label>🎧 Mikrofon-Test</label>
          <button className={`btn sm ${monitoring ? "primary" : ""}`} onClick={toggleMonitor}>
            {monitoring ? "■ Test stoppen" : "▶ Eigenwiedergabe"}
          </button>
          <div className="sub2" style={{ opacity: 0.7 }}>
            Du hörst dein eigenes Mikrofon (inkl. Aufbereitung). Headset empfohlen (sonst Rückkopplung).
          </div>
        </>
      )}

      {settingsTab === "expert" && (
        <>
          <label>📺 Kanal-Overlay</label>
          <button className={`btn sm ${overlayOn ? "primary" : ""}`} onClick={() => setOverlayOn((v) => !v)}>
            {overlayOn ? "An — Overlay sichtbar" : "Aus"}
          </button>
          <div className="sub2" style={{ opacity: 0.7 }}>
            Kleines, klick-durchlässiges Overlay über dem Spiel — zeigt den aktuellen Kanal, blinkt beim Wechsel. Kein Game-Eingriff (separates Fenster).
          </div>
          <div className={`ovlcfg ${overlayOn ? "" : "ovlcfg-off"}`}>
            <div className="ovlrow">
              <span className="ovllbl">Position</span>
              <div className="ovlposgrid">
                {OVERLAY_POSITIONS.map((p) => (
                  <button
                    key={p.key}
                    className={`ovlposbtn ${overlayPos === p.key ? "on" : ""}`}
                    onClick={() => setOverlayPos(p.key)}
                    disabled={!overlayOn}
                    title={p.title}
                  >
                    {p.label}
                  </button>
                ))}
              </div>
            </div>
            <div className="ovlrow">
              <span className="ovllbl">Größe</span>
              <div className="ovlsizes">
                {(["s", "m", "l"] as OverlaySize[]).map((s) => (
                  <button
                    key={s}
                    className={`btn sm ${overlaySize === s ? "primary" : ""}`}
                    onClick={() => setOverlaySize(s)}
                    disabled={!overlayOn}
                  >
                    {s === "s" ? "Klein" : s === "m" ? "Mittel" : "Groß"}
                  </button>
                ))}
              </div>
            </div>
          </div>

          <label>📻 Kanal-Hotkeys (Cycle)</label>
          <div className="pttrow">
            <span className="pttcur">◀ {capturingChan === 0 ? "Drücke Taste…" : (chanPrev ? pttLabel(chanPrev) : "— (nicht belegt)")}</span>
            <button className="btn sm" onClick={() => rebindChan(0)} disabled={capturingChan !== null}>{chanPrev ? "Neu belegen" : "Belegen"}</button>
            {chanPrev && <button className="btn sm" onClick={() => clearChan(0)} disabled={capturingChan !== null} title="Entfernen">✕</button>}
          </div>
          <div className="pttrow">
            <span className="pttcur">▶ {capturingChan === 1 ? "Drücke Taste…" : (chanNext ? pttLabel(chanNext) : "— (nicht belegt)")}</span>
            <button className="btn sm" onClick={() => rebindChan(1)} disabled={capturingChan !== null}>{chanNext ? "Neu belegen" : "Belegen"}</button>
            {chanNext && <button className="btn sm" onClick={() => clearChan(1)} disabled={capturingChan !== null} title="Entfernen">✕</button>}
          </div>
          <div className="sub2" style={{ opacity: 0.7 }}>
            Globale Tasten (RAW, auch im Vollbild-Game) zum Durchschalten der Session-Kanäle — vorheriger / nächster. Auch Mausrad, Gamepad-/Joystick-Taste oder Modifier-Kombi (z.B. Shift+T). Mausrad ▲/▼ eignet sich ideal zum Cyceln.
          </div>

          <label>🔑 Session-Verschlüsselung</label>
          <button className="btn sm" onClick={rotateKey} disabled={rotating || !connected}>
            {rotating ? "⏳ Verschlüssele neu…" : `Session neu verschlüsseln · #${keyInfo.gen}`}
          </button>
          <div className="sub2" style={{ opacity: 0.7 }}>
            Erzeugt für alle Teilnehmer neue Schlüssel (DTLS-SRTP re-handshake). Nur während einer Session.
          </div>
          <button className={`btn sm ${showRekeyBtn ? "primary" : ""}`} onClick={toggleRekeyBtn}>
            {showRekeyBtn ? "Button in der Leiste: sichtbar" : "Button in der Leiste: ausgeblendet"}
          </button>

          <label>🛰 TURN-Relay-Fallback</label>
          <button className={`btn sm ${relayFb ? "primary" : ""}`} onClick={toggleRelayFb}>
            {relayFb ? "An (nutzt Relay falls nötig)" : "Aus (nur direkt/STUN)"}
          </button>
          <div className="sub2" style={{ opacity: 0.7 }}>
            Aus = nie über einen Relay; bei striktem NAT ggf. keine Verbindung. Greift beim nächsten Verbinden.
          </div>

          <label>📊 Bandbreiten-Anzeige (kbps)</label>
          <button className={`btn sm ${showKbps ? "primary" : ""}`} onClick={toggleKbps}>
            {showKbps ? "Anzeigen" : "Ausgeblendet (Sende-/Empfangslicht)"}
          </button>
          <div className="sub2" style={{ opacity: 0.7 }}>
            Aus = statt kbps ein kleines Sende-(rot)/Empfangs-(grün)-Licht.
          </div>

          <label>🐢 Low-Bandwidth-Modus</label>
          <button className={`btn sm ${lowBw ? "primary" : ""}`} onClick={toggleLowBw}>
            {lowBw ? "An — ≈14 kbps + Stille-Unterdrückung" : "Aus"}
          </button>
          <div className="sub2" style={{ opacity: 0.7 }}>
            Niedrige Opus-Bitrate + DTX (Stille sendet nichts) für schwache Verbindungen.
          </div>

          <label>🧪 Netzwerk-Selbsttest</label>
          <button className="btn sm" onClick={runNetCheck} disabled={checking}>
            {checking ? "Teste… (bis ~10 s)" : "Test starten"}
          </button>
          {netCheck && (
            <div className="netcheck">
              <div>Signaling-Server: {netCheck.signaling ? "✅ ja" : "❌ nein"}</div>
              <div>Kann senden: {netCheck.can_send ? "✅ ja" : "❌ nein"}</div>
              <div>Kann empfangen: {netCheck.can_receive ? "✅ ja" : "❌ nein"}</div>
              <div>Internet / STUN: {netCheck.stun ? "✅ ja" : "❌ nein"}</div>
            </div>
          )}

          <label>🎚 Audio-Aufbereitung</label>
          <div className="dsp">
        <div className="dsphead">
          <label className="chk"><input type="checkbox" checked={dsp.gate} onChange={(e) => updateDsp({ gate: e.target.checked })} /> Noise Gate</label>
        </div>
        <div className="dsprow">
          <span>Schwelle</span>
          <input type="range" min={0} max={80} value={Math.round(dsp.gate_threshold * 1000)} disabled={!dsp.gate} onChange={(e) => updateDsp({ gate_threshold: Number(e.target.value) / 1000 })} />
          <span className="vval">{Math.round(dsp.gate_threshold * 1000)}</span>
        </div>

        <div className="dsphead">
          <label className="chk"><input type="checkbox" checked={dsp.compressor} onChange={(e) => updateDsp({ compressor: e.target.checked })} /> Kompressor</label>
        </div>
        <div className="dsprow">
          <span>Schwelle</span>
          <input type="range" min={5} max={50} value={Math.round(dsp.comp_threshold * 100)} disabled={!dsp.compressor} onChange={(e) => updateDsp({ comp_threshold: Number(e.target.value) / 100 })} />
          <span className="vval">{Math.round(dsp.comp_threshold * 100)}</span>
        </div>
        <div className="dsprow">
          <span>Ratio</span>
          <input type="range" min={10} max={100} value={Math.round(dsp.comp_ratio * 10)} disabled={!dsp.compressor} onChange={(e) => updateDsp({ comp_ratio: Number(e.target.value) / 10 })} />
          <span className="vval">{(dsp.comp_ratio).toFixed(1)}:1</span>
        </div>
        <div className="dsprow">
          <span>Makeup</span>
          <input type="range" min={10} max={30} value={Math.round(dsp.comp_makeup * 10)} disabled={!dsp.compressor} onChange={(e) => updateDsp({ comp_makeup: Number(e.target.value) / 10 })} />
          <span className="vval">{(dsp.comp_makeup).toFixed(1)}×</span>
        </div>

        <div className="dsphead">
          <label className="chk"><input type="checkbox" checked={dsp.limiter} onChange={(e) => updateDsp({ limiter: e.target.checked })} /> Limiter (gegen Knacken)</label>
        </div>
        <div className="dsprow">
          <span>Ceiling</span>
          <input type="range" min={50} max={100} value={Math.round(dsp.limiter_ceiling * 100)} disabled={!dsp.limiter} onChange={(e) => updateDsp({ limiter_ceiling: Number(e.target.value) / 100 })} />
          <span className="vval">{Math.round(dsp.limiter_ceiling * 100)}%</span>
        </div>
      </div>
        </>
      )}
    </div>
    </div>
  );
  const rotatedAt = keyInfo.at
    ? new Date(keyInfo.at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })
    : null;
  const pqcVoice = roomAudio.gen != null;
  const encFooter = (
    <div className="encfoot">
      🔒 Encryption: <b>DTLS-SRTP{pqcVoice ? " + PQC" : ""}</b> (Audio) · <b>DTLS-SCTP</b> (Chat) · <b>TLS/wss</b> (Signaling)
      — Ende-zu-Ende P2P, encrypted by default &amp; by session
      <span className="keygen">
        · Schlüssel-Generation <b>#{keyInfo.gen}</b>
        {rotatedAt ? ` (rotiert ${rotatedAt})` : ""}
      </span>
      {connected && (
        <span className={`pqcvoice ${pqcVoice ? "on" : "neg"}`} title="Post-Quantum-Verschlüsselung der Sprache (ML-KEM-768 Raum-Schlüssel, verteilt über die pairwise PQC-Sessions)">
          {" · "}
          {pqcVoice ? (
            <>🛡️ Voice quantensicher <b>#{roomAudio.gen}</b>{roomAudio.authority ? " ★" : ""}</>
          ) : (
            <>🛡️ Voice-Verschlüsselung: aushandeln…</>
          )}
        </span>
      )}
      {appVersion && <span className="ver"> · v{appVersion}</span>}
    </div>
  );

  const updateBanner =
    update && showUpdate && !storeBuild ? (
      <div className="updbar">
        <div className="updhead">
          <b>⬆ Neue Version {update.version} verfügbar</b>
          <button className="x" title="Schließen" onClick={() => setShowUpdate(false)}>×</button>
        </div>
        <div className="updnotes">{update.notes}</div>
        <div className="updact">
          <button className="btn primary" onClick={() => invoke("open_download").catch(() => {})}>Herunterladen</button>
          <button className="btn" onClick={() => setShowUpdate(false)}>Später</button>
        </div>
      </div>
    ) : null;

  if (!connected) {
    return (
      <div className="screen center">
        {updateBanner}
        <div className="card connect">
          <div className="brandrow">
            <div className="brandwrap">
              <img src={logo} className="applogo" alt="" />
              <div className="brand">RDOC <span>// SQUADLINK LITE</span></div>
            </div>
            <button className="gear" title="Audio-Einstellungen" onClick={() => setShowSettings((s) => !s)}>⚙</button>
          </div>
          <div className="sub">P2P Voice + Chat{appVersion ? ` · v${appVersion}` : ""}</div>
          {showSettings && deviceSettings}

          <div className="testmode">
            <button className="btn sm" onClick={runNetCheck} disabled={checking}>
              {checking ? "🧪 Teste Netzwerk… (bis ~10 s)" : "🧪 Testmode — Verbindung prüfen"}
            </button>
            {netCheck && (() => {
              const core = netCheck.can_send && netCheck.can_receive;
              const allOk = core && netCheck.signaling && netCheck.stun;
              const yn = (b: boolean) => (b ? "✅" : "❌");
              return (
                <div className={`testverdict ${allOk ? "ok" : core ? "warn" : "bad"}`}>
                  <div className="tvhead">
                    {allOk
                      ? "✅ Alles funktioniert"
                      : core
                        ? "⚠️ P2P läuft — mit Einschränkung"
                        : "❌ Grundfunktion gestört"}
                  </div>
                  <div className="tvrow">{yn(netCheck.can_send)} Senden · {yn(netCheck.can_receive)} Empfangen</div>
                  <div className="tvrow">{yn(netCheck.stun)} Internet/STUN · {yn(netCheck.signaling)} Signaling-Server</div>
                  {!core && <div className="tvhint">P2P-Datenpfad blockiert — Firewall/VPN prüfen.</div>}
                  {core && !netCheck.stun && <div className="tvhint">Kein STUN — hinter striktem NAT evtl. Relay nötig.</div>}
                </div>
              );
            })()}
          </div>

          <label>Name</label>
          <input value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} placeholder="Commander" />

          <div className="session">
            <div className="sub2">
              <b>Host:</b> Session erstellen → <b>Link + 6-stellige PIN</b> an die Mitspieler geben.
              <br /><b>Mitspieler:</b> Link/Code + PIN eingeben — komplett ohne Konfiguration.
            </div>
            <button className="btn primary" onClick={createSession} disabled={connecting}>
              {connecting ? "…" : "SESSION ERSTELLEN (HOST)"}
            </button>
            {sessionInfo && (
              <div className="sessbox">
                <div className="sesshead">
                  <label style={{ margin: 0 }}>Link — an Mitspieler</label>
                  <button
                    type="button"
                    className={`streamtoggle ${streamerMode ? "on" : ""}`}
                    onClick={toggleStreamer}
                    title="Streamer-Modus: Link + PIN verbergen"
                  >
                    {streamerMode ? "🕶 Verborgen" : "👁 Sichtbar"}
                  </button>
                </div>
                <input readOnly value={sessionInfo.link} className={`mono ${streamerMode ? "redacted" : ""}`} onFocus={(e) => e.currentTarget.select()} />
                <label>PIN — separat weitergeben</label>
                <div className={`pin mono ${streamerMode ? "redacted" : ""}`}>{sessionInfo.pin}</div>
                <button className="btn sm" onClick={() => copy(`${sessionInfo.link}\nPIN: ${sessionInfo.pin}`)}>
                  LINK + PIN KOPIEREN
                </button>
              </div>
            )}
            <div className="sub2" style={{ marginTop: "1rem", opacity: 0.7 }}>— oder beitreten —</div>
            <label>Link oder Code</label>
            <input value={joinInput} onChange={(e) => setJoinInput(e.target.value)} placeholder="https://…/j/abc oder abc" className="mono" spellCheck={false} />
            <label>PIN (6-stellig)</label>
            <input value={joinPin} onChange={(e) => setJoinPin(e.target.value)} inputMode="numeric" maxLength={6} placeholder="123456" />
            <button className="btn primary" onClick={joinSession} disabled={connecting || !joinInput.trim() || joinPin.trim().length < 6}>
              {connecting ? "VERBINDE…" : "BEITRETEN"}
            </button>
          </div>
          {fpMode && (
            <div className="fpbox">
              <div className="sub2">
                <b>Fleetplanner-Modus</b> — Direkt-Link einfügen und verbinden. Kein Code, keine PIN.
              </div>
              <label>SquadLink-Direktlink</label>
              <input
                value={directLink}
                onChange={(e) => setDirectLink(e.target.value)}
                placeholder="squadlink://connect?ws=…&room=…&token=…"
                className="mono"
                spellCheck={false}
              />
              <button className="btn primary" onClick={() => connectDirect(directLink)} disabled={connecting || !directLink.trim()}>
                {connecting ? "VERBINDE…" : "DIREKT VERBINDEN"}
              </button>
            </div>
          )}
          {log && <div className="err">{log}</div>}
          {encFooter}
        </div>
        <button
          className={`fpcorner ${fpMode ? "on" : ""}`}
          title={fpMode ? "Fleetplanner-Modus: an" : "Fleetplanner-Modus"}
          onClick={toggleFpMode}
        >
          ⛴
        </button>
      </div>
    );
  }

  const estPeers = participants.filter((p) => !p.you && p.badge).length;
  const p2pCount = net?.peers ?? estPeers;
  const up = net ? net.up : estPeers * 32;
  const down = net ? net.down : estPeers * 32;
  const measured = net != null;
  // Receiving = measurable incoming audio, else any peer marked speaking.
  const receiving = net ? net.down > 4 : participants.some((p) => !p.you && p.speaking);

  return (
    <div className="screen app">
      <header>
        <div className="brand sm">RDOC <span>// SQUADLINK LITE</span></div>
        <div className={`dot ${transmitting ? "tx" : "ok"}`} />
        <div className="hstatus">{transmitting ? "SENDEN" : "VERBUNDEN"}</div>
        <button className="gear" title="Audio-Einstellungen" onClick={() => setShowSettings((s) => !s)}>⚙</button>
        <button className="leave" title="Session verlassen" onClick={onDisconnect}>Verlassen</button>
      </header>
      {updateBanner}
      {showSettings && deviceSettings}

      {!sigUp && (
        <div className="sigbanner">
          <span>⚠ Signaling verloren — automatischer Reconnect läuft… (P2P-Audio läuft weiter)</span>
          <button className="btn sm" onClick={resumeSession} disabled={resuming}>
            {resuming ? "Verbinde…" : "Jetzt wiederverbinden"}
          </button>
        </div>
      )}

      <div className="netbar">
        <span>P2P: <b>{p2pCount}</b></span>
        {showKbps ? (
          <>
            <span>↑ {measured ? "" : "~"}{up} kbps</span>
            <span>↓ {measured ? "" : "~"}{down} kbps</span>
            <span className="netest">({measured ? "gemessen" : "geschätzt"})</span>
          </>
        ) : (
          <span className="txrx" title="Funk: rot = Senden, grün = Empfangen">
            <span
              className={`txlight ${
                transmitting && receiving ? "both" : transmitting ? "send" : receiving ? "recv" : ""
              }`}
            />
            <span className="txrxlbl">Funk</span>
          </span>
        )}
        {lowBw && <span className="lowbw" title="Low-Bandwidth-Modus aktiv">🐢 Low-BW</span>}
        {showRekeyBtn && (
          <button
            className="rekey"
            title="Erzeugt für alle Teilnehmer neue Verschlüsselungs-Keys (DTLS-SRTP re-handshake)"
            onClick={rotateKey}
            disabled={rotating}
          >
            {rotating ? "⏳ Verschlüssele neu…" : `🔑 Session neu verschlüsseln · #${keyInfo.gen}`}
          </button>
        )}
      </div>

      <div className="volrow">
        <span className="vlabel">🔊 Gesamt</span>
        <input type="range" min={0} max={200} value={masterVol} onChange={(e) => onMaster(Number(e.target.value))} />
        <span className="vval">{masterVol}%</span>
      </div>

      <main>
        <section className="roster">
          {sessionInfo && (
            <div className="sessbox sessbox-live">
              <div className="sesshead">
                <div className="hsec" style={{ margin: 0 }}>Session teilen</div>
                <button
                  type="button"
                  className={`streamtoggle ${streamerMode ? "on" : ""}`}
                  onClick={toggleStreamer}
                  title="Streamer-Modus: Link + PIN verbergen"
                >
                  {streamerMode ? "🕶 Verborgen" : "👁 Sichtbar"}
                </button>
              </div>
              <input readOnly value={sessionInfo.link} className={`mono ${streamerMode ? "redacted" : ""}`} onFocus={(e) => e.currentTarget.select()} />
              <div className="pinrow">
                <span className={`pin mono ${streamerMode ? "redacted" : ""}`}>PIN {sessionInfo.pin}</span>
                <button className="btn sm" onClick={() => copy(`${sessionInfo.link}\nPIN: ${sessionInfo.pin}`)}>
                  LINK + PIN
                </button>
              </div>
            </div>
          )}
          <div className="hsec">📻 Kanal · {myChannel}</div>
          <div className="chanbar">
            <div className="chanchips">
              {(() => {
                // Live occupancy per channel (canonical key → count).
                const counts = participants.reduce<Record<string, number>>((m, p) => {
                  const k = canonChannel(p.channel);
                  m[k] = (m[k] || 0) + 1;
                  return m;
                }, {});
                // Union of session-remembered channels + any live ones, deduped.
                const seen = new Set<string>();
                const chips: { label: string; canon: string; count: number }[] = [];
                for (const label of [...sessionChannels, myChannel, ...participants.map((p) => p.channel)]) {
                  const k = canonChannel(label);
                  if (!k || seen.has(k)) continue;
                  seen.add(k);
                  chips.push({ label, canon: k, count: counts[k] || 0 });
                }
                return chips.map((c) => {
                  const isMine = canonChannel(myChannel) === c.canon;
                  // Deletable only when EMPTY (nobody on it — count includes me)
                  // and not the base channel (the default is permanent).
                  const deletable = c.count === 0 && c.canon !== canonChannel(DEFAULT_CHANNEL);
                  return (
                    <span key={c.canon} className="chanchipwrap">
                      <button
                        className={`chanchip ${isMine ? "active" : ""}`}
                        onClick={() => switchChannel(c.label)}
                        title={`Auf Kanal "${c.label}" wechseln`}
                      >
                        {c.label} · {c.count}
                      </button>
                      {deletable && (
                        <button
                          className="chandel"
                          onClick={() => deleteChannel(c.canon)}
                          title={`Kanal "${c.label}" entfernen`}
                          aria-label={`Kanal ${c.label} entfernen`}
                        >
                          ×
                        </button>
                      )}
                    </span>
                  );
                });
              })()}
            </div>
            <form
              className="channew"
              onSubmit={(e) => {
                e.preventDefault();
                switchChannel(channelDraft);
                setChannelDraft("");
              }}
            >
              <input
                value={channelDraft}
                maxLength={MAX_CHANNEL_LEN}
                placeholder="Neuer Kanal…"
                onChange={(e) => setChannelDraft(e.target.value)}
              />
              <button type="submit" className="btn sm" disabled={!channelDraft.trim()}>
                Wechseln
              </button>
            </form>
          </div>
          <div className="hsec">Teilnehmer · {participants.length}</div>
          <div className="peerlist">
          {participants.map((p) => {
            const sameChan = canonChannel(p.channel) === canonChannel(myChannel);
            return (
            <div key={p.user_id} className={`peer ${p.speaking ? "speaking" : ""} ${sameChan ? "" : "offchannel"}`}>
              <div className="peerhead">
                <span className={`talk ${p.speaking ? "on" : ""}`} />
                <span className="pname">
                  {p.name}
                  {p.you && <span className="me"> (du)</span>}
                </span>
                <span className={`badge chan ${sameChan ? "same" : "other"}`} title={`Kanal: ${p.channel}`}>
                  📻 {p.channel}
                </span>
                {p.badge && (
                  <span className={`badge ${p.badge.includes("RELAY") ? "relay" : "direct"}`}>
                    {p.badge}
                  </span>
                )}
              </div>
              {!p.you && (
                <div className="peervol">
                  <span className="vmini">🔊</span>
                  <input
                    type="range"
                    min={0}
                    max={200}
                    value={peerVol[p.user_id] ?? 100}
                    onChange={(e) => onPeerVol(p.user_id, Number(e.target.value))}
                  />
                  <span className="vval">{peerVol[p.user_id] ?? 100}%</span>
                </div>
              )}
            </div>
            );
          })}
          </div>
          <button className={`ptt ${transmitting ? "live" : ""} ${micMuted ? "muted" : ""}`} onClick={ptt} disabled={micMuted}>
            {micMuted ? "🔇 MIKRO STUMM" : transmitting ? "● SENDEN AKTIV" : "PUSH TO TALK"}
            <span className="ptthint">{pttBinding2 ? `${pttLabel(pttBinding)} / ${pttLabel(pttBinding2)}` : pttLabel(pttBinding)} halten · oder klick zum Umschalten</span>
          </button>
          <div className="selfctl">
            <button className={`ctl ${transmitting ? "on" : ""}`} onClick={ptt} disabled={micMuted} title="Dauersenden ein/aus">
              {transmitting ? "🟢 Sendet (Toggle)" : "🔘 Toggle senden"}
            </button>
            <button className={`ctl ${micMuted ? "on" : ""}`} onClick={toggleMic} title="Eigenes Mikrofon stummschalten (du hörst weiter)">
              {micMuted ? "🔇 Mikro stumm" : "🎙️ Mikro an"}
            </button>
            <button className={`ctl ${deaf ? "on" : ""}`} onClick={toggleDeaf} title="Ton aus (nichts hören)">
              {deaf ? "🔕 Ton aus" : "🔊 Ton an"}
            </button>
          </div>
        </section>

        <section className="chat">
          <div className="hsec">Chat</div>
          <div className="chatlog">
            {chat.map((c, i) => (
              <div key={i} className="line">
                <span className="from">{c.from}</span>
                <span className="text">{c.text}</span>
              </div>
            ))}
            <div ref={chatEnd} />
          </div>
          <div className="chatin">
            <input
              value={msg}
              onChange={(e) => setMsg(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && send()}
              placeholder="Nachricht an alle…"
            />
            <button className="btn" onClick={send}>SENDEN</button>
          </div>
        </section>
      </main>
      {log && <div className="footlog">{log}</div>}
      {encFooter}
    </div>
  );
}
