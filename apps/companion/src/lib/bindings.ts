// Labels for raw input codes as the Rust side reports them ("F8", "KeyR",
// "Mouse:Unknown(1)", "Pad:3", chords like "Ctrl+Shift+KeyT").

/** Max gap between two PTT key presses that still counts as a double-tap (ms). */
export const PTT_LATCH_MS = 350;

/** Hold time on the on-screen PTT button that latches transmit on (ms). */
export const PTT_LONGPRESS_MS = 700;

const MODIFIERS = new Set(["Ctrl", "Alt", "Shift", "Meta"]);

/** Friendly label for one base key (no modifiers). */
export function baseLabel(code: string): string {
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

/** Label a binding, including modifier chords ("Shift+KeyT" → "Shift + T").
 *  Empty binding → "—". Unknown "modifier" tokens before the base are dropped. */
export function pttLabel(code: string): string {
  if (!code) return "—";
  const parts = code.split("+");
  const base = parts.pop() ?? "";
  const prefix = parts.filter((p) => MODIFIERS.has(p));
  return [...prefix, baseLabel(base)].join(" + ");
}
