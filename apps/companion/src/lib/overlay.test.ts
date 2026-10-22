import { describe, expect, it } from "vitest";
import { OVERLAY_DIMS, OVERLAY_MARGIN, OVERLAY_POSITIONS, overlayPosition } from "./overlay";

const FHD = { size: { width: 1920, height: 1080 }, position: { x: 0, y: 0 }, scaleFactor: 1 };

describe("overlayPosition", () => {
  it("anchors each corner with the margin on a 1080p monitor (size m)", () => {
    const { w, h } = OVERLAY_DIMS.m;
    expect(overlayPosition(FHD, "tl", "m")).toEqual({ x: 24, y: 24 });
    expect(overlayPosition(FHD, "tr", "m")).toEqual({ x: 1920 - w - 24, y: 24 });
    expect(overlayPosition(FHD, "bl", "m")).toEqual({ x: 24, y: 1080 - h - 24 });
    expect(overlayPosition(FHD, "br", "m")).toEqual({ x: 1920 - w - 24, y: 1080 - h - 24 });
  });

  it("centres horizontally for tc/bc", () => {
    const { w, h } = OVERLAY_DIMS.s;
    expect(overlayPosition(FHD, "tc", "s")).toEqual({ x: (1920 - w) / 2, y: OVERLAY_MARGIN });
    expect(overlayPosition(FHD, "bc", "s")).toEqual({ x: (1920 - w) / 2, y: 1080 - h - OVERLAY_MARGIN });
  });

  it("converts physical pixels to logical via the scale factor", () => {
    const hidpi = { size: { width: 3840, height: 2160 }, position: { x: 0, y: 0 }, scaleFactor: 2 };
    // Same logical geometry as 1080p at scale 1.
    expect(overlayPosition(hidpi, "br", "l")).toEqual(overlayPosition(FHD, "br", "l"));
  });

  it("offsets by the monitor origin for a secondary display", () => {
    const second = { size: { width: 1920, height: 1080 }, position: { x: 1920, y: 100 }, scaleFactor: 1 };
    expect(overlayPosition(second, "tl", "m")).toEqual({ x: 1920 + 24, y: 100 + 24 });
  });

  it("assumes 1920×1080 at origin when no monitor is known", () => {
    expect(overlayPosition(null, "br", "m")).toEqual(overlayPosition(FHD, "br", "m"));
  });

  it("rounds to whole logical pixels", () => {
    const odd = { size: { width: 1921, height: 1081 }, position: { x: 0, y: 0 }, scaleFactor: 1 };
    const p = overlayPosition(odd, "tc", "m");
    expect(Number.isInteger(p.x)).toBe(true);
    expect(Number.isInteger(p.y)).toBe(true);
  });
});

describe("overlay tables", () => {
  it("sizes grow monotonically s < m < l", () => {
    expect(OVERLAY_DIMS.s.w).toBeLessThan(OVERLAY_DIMS.m.w);
    expect(OVERLAY_DIMS.m.w).toBeLessThan(OVERLAY_DIMS.l.w);
    expect(OVERLAY_DIMS.s.h).toBeLessThan(OVERLAY_DIMS.m.h);
    expect(OVERLAY_DIMS.m.h).toBeLessThan(OVERLAY_DIMS.l.h);
  });

  it("lists all six anchors exactly once with German titles", () => {
    expect(OVERLAY_POSITIONS.map((p) => p.key)).toEqual(["tl", "tc", "tr", "bl", "bc", "br"]);
    for (const p of OVERLAY_POSITIONS) expect(p.title).toMatch(/^(oben|unten) (links|mitte|rechts)$/);
  });
});
