// Persisted settings: display scale, capture DSP and the receive-side radio
// effect. Pure parsing/validation of stored values; localStorage access stays
// in App.tsx so these run under node (vitest) as well.

// Display size. Scales the whole webview, not just type: the stylesheet is
// px-based, so lifting a root font size would move the text and leave every
// control, gap and icon at its old size. 100 % stays the default so nobody's
// window silently reflows on update.
export const UI_SCALES = [0.9, 1.0, 1.1, 1.25, 1.5] as const;
export type UiScale = (typeof UI_SCALES)[number];
export const UI_SCALE_DEFAULT: UiScale = 1.0;

/** The scale to use for a stored raw value. Guards the entry: an old or
 *  hand-edited value must not be able to zoom the window to something unusable,
 *  so anything outside UI_SCALES falls back to the default. */
export function uiScaleFromStored(raw: string | null | undefined): number {
  const v = Number(raw);
  return UI_SCALES.includes(v as UiScale) ? v : UI_SCALE_DEFAULT;
}

/** Capture-path DSP (must match companion_core::audio::DspConfig field names). */
export type DspConfig = {
  gate: boolean;
  gate_threshold: number;
  compressor: boolean;
  comp_threshold: number;
  comp_ratio: number;
  comp_makeup: number;
  limiter: boolean;
  limiter_ceiling: number;
};
export const DSP_DEFAULT: DspConfig = {
  gate: true,
  gate_threshold: 0.015,
  compressor: true,
  comp_threshold: 0.15,
  comp_ratio: 3.0,
  comp_makeup: 1.4,
  limiter: true,
  limiter_ceiling: 0.97,
};

/** Funk-Effekt: band-pass + saturation + destruction on the RECEIVE mix (all
 *  incoming voices). Listener-side only — changes nothing about what is sent. */
export type RadioCfg = {
  enabled: boolean;
  low_cut: number;   // Hz — cut lows
  high_cut: number;  // Hz — cut highs
  saturation: number;  // 0..1
  destruction: number; // 0..1
};
export const RADIO_DEFAULT: RadioCfg = {
  enabled: false,
  low_cut: 300,
  high_cut: 3400,
  saturation: 0.35,
  destruction: 0.15,
};

/** Merge a stored JSON object over the defaults. Missing keys keep their
 *  default (so a config saved by an older build gains new fields), invalid JSON
 *  or a non-object yields the defaults unchanged. */
export function mergeStoredConfig<T extends object>(defaults: T, raw: string | null | undefined): T {
  try {
    const parsed: unknown = JSON.parse(raw || "{}");
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return { ...defaults };
    return { ...defaults, ...(parsed as Partial<T>) };
  } catch {
    return { ...defaults };
  }
}
