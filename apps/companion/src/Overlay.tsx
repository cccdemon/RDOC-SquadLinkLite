import { useEffect, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen, emit } from "@tauri-apps/api/event";

type Size = "s" | "m" | "l";
type OverlayUpdate = { channel: string; size: Size };

// Minimal, click-through channel overlay shown over the game. Idle = dim pill
// with the current channel; a switch flashes it bright for ~1.5 s. It never
// takes focus or input (the window is transparent + ignore-cursor-events).
export default function Overlay() {
  const [channel, setChannel] = useState<string>("");
  const [size, setSize] = useState<Size>("m");
  const [flash, setFlash] = useState(false);
  const firstRef = useRef(true);
  const timerRef = useRef<number | undefined>(undefined);

  useEffect(() => {
    // Never steal cursor/input from the game.
    getCurrentWindow().setIgnoreCursorEvents(true).catch(() => {});
    const un = listen<OverlayUpdate>("overlay-update", (e) => {
      setChannel(e.payload.channel);
      setSize(e.payload.size);
      // Flash on every update except the very first (initial state sync).
      if (firstRef.current) {
        firstRef.current = false;
        return;
      }
      setFlash(true);
      window.clearTimeout(timerRef.current);
      timerRef.current = window.setTimeout(() => setFlash(false), 1500);
    });
    // Ask the main window for the current channel/size (we were just created).
    emit("overlay-ready").catch(() => {});
    return () => {
      un.then((f) => f());
      window.clearTimeout(timerRef.current);
    };
  }, []);

  return (
    <div className={`ovl ovl-${size} ${flash ? "ovl-flash" : ""}`}>
      <span className="ovl-ico">📻</span>
      <span className="ovl-name">{channel || "—"}</span>
    </div>
  );
}
