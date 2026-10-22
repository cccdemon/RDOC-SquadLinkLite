import { describe, expect, it } from "vitest";
import {
  DEFAULT_CHANNEL,
  MAX_CHANNEL_LEN,
  MAX_CHAT_LINES,
  appendCapped,
  canonChannel,
  cleanChannelName,
  cycleTarget,
  dedupeChannels,
} from "./channels";

describe("canonChannel", () => {
  it("trims and lowercases so matching is case-insensitive like the Rust side", () => {
    expect(canonChannel("  Funk 1 ")).toBe("funk 1");
    expect(canonChannel("ALPHA")).toBe(canonChannel("alpha"));
    expect(canonChannel("   ")).toBe("");
  });
});

describe("cleanChannelName", () => {
  it("trims and bounds to MAX_CHANNEL_LEN", () => {
    expect(cleanChannelName("  Bravo ")).toBe("Bravo");
    const long = "x".repeat(MAX_CHANNEL_LEN + 10);
    expect(cleanChannelName(long)).toHaveLength(MAX_CHANNEL_LEN);
    expect(cleanChannelName("")).toBe("");
  });
});

describe("dedupeChannels", () => {
  it("keeps first spelling, drops case/space duplicates and empties", () => {
    expect(dedupeChannels(["Funk 1", "funk 1", " FUNK 1", "Alpha", "", "  ", "alpha"])).toEqual(["Funk 1", "Alpha"]);
  });

  it("preserves order of first appearance", () => {
    expect(dedupeChannels(["b", "a", "B", "c"])).toEqual(["b", "a", "c"]);
  });

  it("returns an empty list for no input", () => {
    expect(dedupeChannels([])).toEqual([]);
  });
});

describe("cycleTarget", () => {
  const list = ["Funk 1", "Alpha", "Bravo"];

  it("steps forward and backward with wrap-around", () => {
    expect(cycleTarget(list, "Funk 1", 1)).toBe("Alpha");
    expect(cycleTarget(list, "Bravo", 1)).toBe("Funk 1");
    expect(cycleTarget(list, "Funk 1", -1)).toBe("Bravo");
    expect(cycleTarget(list, "Alpha", -1)).toBe("Funk 1");
  });

  it("matches the current channel case-insensitively", () => {
    expect(cycleTarget(list, "alpha", 1)).toBe("Bravo");
  });

  it("treats an unknown current channel as index 0", () => {
    expect(cycleTarget(list, "nope", 1)).toBe("Alpha");
    expect(cycleTarget(list, "nope", -1)).toBe("Bravo");
  });

  it("treats any non-negative dir as next and any negative as prev", () => {
    expect(cycleTarget(list, "Funk 1", 0)).toBe("Alpha");
    expect(cycleTarget(list, "Funk 1", -7)).toBe("Bravo");
  });

  it("returns null when there is nothing to cycle to", () => {
    expect(cycleTarget([], "x", 1)).toBeNull();
    expect(cycleTarget(["only"], "only", 1)).toBeNull();
  });
});

describe("appendCapped", () => {
  it("appends below the cap", () => {
    expect(appendCapped([1, 2], 3, 5)).toEqual([1, 2, 3]);
  });

  it("drops the oldest lines once the cap is exceeded", () => {
    expect(appendCapped([1, 2, 3], 4, 3)).toEqual([2, 3, 4]);
    expect(appendCapped([1, 2, 3, 4, 5], 6, 2)).toEqual([5, 6]);
  });

  it("does not mutate the input", () => {
    const a = [1, 2];
    appendCapped(a, 3, 2);
    expect(a).toEqual([1, 2]);
  });

  it("defaults to MAX_CHAT_LINES", () => {
    const full = Array.from({ length: MAX_CHAT_LINES }, (_, i) => i);
    const out = appendCapped(full, 9999);
    expect(out).toHaveLength(MAX_CHAT_LINES);
    expect(out[0]).toBe(1);
    expect(out[out.length - 1]).toBe(9999);
  });
});

describe("constants", () => {
  it("default channel is itself a valid, clean name", () => {
    expect(cleanChannelName(DEFAULT_CHANNEL)).toBe(DEFAULT_CHANNEL);
    expect(DEFAULT_CHANNEL.length).toBeLessThanOrEqual(MAX_CHANNEL_LEN);
  });
});
