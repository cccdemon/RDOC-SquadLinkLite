import { describe, expect, it } from "vitest";
import {
  DSP_DEFAULT,
  RADIO_DEFAULT,
  UI_SCALES,
  UI_SCALE_DEFAULT,
  mergeStoredConfig,
  uiScaleFromStored,
} from "./settings";

describe("uiScaleFromStored", () => {
  it("accepts every listed scale", () => {
    for (const s of UI_SCALES) expect(uiScaleFromStored(String(s))).toBe(s);
    expect(uiScaleFromStored("1")).toBe(1.0);
  });

  it("falls back to 100 % for anything outside the list", () => {
    expect(uiScaleFromStored(null)).toBe(UI_SCALE_DEFAULT);
    expect(uiScaleFromStored(undefined)).toBe(UI_SCALE_DEFAULT);
    expect(uiScaleFromStored("")).toBe(UI_SCALE_DEFAULT);
    expect(uiScaleFromStored("3")).toBe(UI_SCALE_DEFAULT);
    expect(uiScaleFromStored("0.01")).toBe(UI_SCALE_DEFAULT);
    expect(uiScaleFromStored("abc")).toBe(UI_SCALE_DEFAULT);
    expect(uiScaleFromStored("1.24")).toBe(UI_SCALE_DEFAULT);
  });

  it("default is 100 % and part of the list", () => {
    expect(UI_SCALE_DEFAULT).toBe(1.0);
    expect(UI_SCALES).toContain(UI_SCALE_DEFAULT);
  });
});

describe("mergeStoredConfig", () => {
  it("returns the defaults for missing, empty or invalid storage", () => {
    expect(mergeStoredConfig(DSP_DEFAULT, null)).toEqual(DSP_DEFAULT);
    expect(mergeStoredConfig(DSP_DEFAULT, "")).toEqual(DSP_DEFAULT);
    expect(mergeStoredConfig(DSP_DEFAULT, "{not json")).toEqual(DSP_DEFAULT);
    expect(mergeStoredConfig(DSP_DEFAULT, "42")).toEqual(DSP_DEFAULT);
    expect(mergeStoredConfig(DSP_DEFAULT, "[1,2]")).toEqual(DSP_DEFAULT);
    expect(mergeStoredConfig(DSP_DEFAULT, "null")).toEqual(DSP_DEFAULT);
  });

  it("overrides only the stored keys and keeps new defaults for the rest", () => {
    const out = mergeStoredConfig(DSP_DEFAULT, JSON.stringify({ gate: false, comp_ratio: 5 }));
    expect(out.gate).toBe(false);
    expect(out.comp_ratio).toBe(5);
    expect(out.limiter).toBe(DSP_DEFAULT.limiter);
    expect(out.limiter_ceiling).toBe(DSP_DEFAULT.limiter_ceiling);
  });

  it("never mutates the defaults object", () => {
    const before = { ...RADIO_DEFAULT };
    const out = mergeStoredConfig(RADIO_DEFAULT, JSON.stringify({ enabled: true }));
    expect(out.enabled).toBe(true);
    expect(RADIO_DEFAULT).toEqual(before);
    expect(out).not.toBe(RADIO_DEFAULT);
  });
});

describe("defaults", () => {
  it("DSP defaults are sane linear values", () => {
    expect(DSP_DEFAULT.gate_threshold).toBeGreaterThan(0);
    expect(DSP_DEFAULT.gate_threshold).toBeLessThan(DSP_DEFAULT.comp_threshold);
    expect(DSP_DEFAULT.comp_ratio).toBeGreaterThan(1);
    expect(DSP_DEFAULT.limiter_ceiling).toBeLessThanOrEqual(1);
  });

  it("radio effect is off by default with a speech band-pass", () => {
    expect(RADIO_DEFAULT.enabled).toBe(false);
    expect(RADIO_DEFAULT.low_cut).toBeLessThan(RADIO_DEFAULT.high_cut);
    expect(RADIO_DEFAULT.saturation).toBeGreaterThanOrEqual(0);
    expect(RADIO_DEFAULT.saturation).toBeLessThanOrEqual(1);
    expect(RADIO_DEFAULT.destruction).toBeGreaterThanOrEqual(0);
    expect(RADIO_DEFAULT.destruction).toBeLessThanOrEqual(1);
  });
});
