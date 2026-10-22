// Channel overlay geometry (the separate transparent, click-through window).
// Pure placement math; window creation lives in App.tsx.

export type OverlaySize = "s" | "m" | "l";
export type OverlayPos = "tl" | "tc" | "tr" | "bl" | "bc" | "br";

export const OVERLAY_DIMS: Record<OverlaySize, { w: number; h: number }> = {
  s: { w: 180, h: 40 },
  m: { w: 224, h: 52 },
  l: { w: 288, h: 64 },
};

export const OVERLAY_POSITIONS: { key: OverlayPos; label: string; title: string }[] = [
  { key: "tl", label: "◤", title: "oben links" },
  { key: "tc", label: "▲", title: "oben mitte" },
  { key: "tr", label: "◥", title: "oben rechts" },
  { key: "bl", label: "◣", title: "unten links" },
  { key: "bc", label: "▼", title: "unten mitte" },
  { key: "br", label: "◢", title: "unten rechts" },
];

export const OVERLAY_MARGIN = 24;

/** What we need from a Tauri `Monitor`: physical size/position + scale factor. */
export type MonitorLike = {
  size: { width: number; height: number };
  position: { x: number; y: number };
  scaleFactor: number;
} | null;

/** Logical top-left corner for the overlay anchored to a corner/center of the
 *  monitor with a small margin. A missing monitor (`null`) assumes 1920×1080 at
 *  origin and scale 1. Coordinates are rounded to whole logical pixels. */
export function overlayPosition(mon: MonitorLike, pos: OverlayPos, size: OverlaySize): { x: number; y: number } {
  const d = OVERLAY_DIMS[size];
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
  return { x: Math.round(x), y: Math.round(y) };
}
