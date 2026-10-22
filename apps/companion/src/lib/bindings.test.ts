import { describe, expect, it } from "vitest";
import { PTT_LATCH_MS, PTT_LONGPRESS_MS, baseLabel, pttLabel } from "./bindings";

describe("baseLabel", () => {
  it("strips the Key prefix from letter keys and leaves others as-is", () => {
    expect(baseLabel("KeyR")).toBe("R");
    expect(baseLabel("F8")).toBe("F8");
    expect(baseLabel("Space")).toBe("Space");
  });

  it("labels gamepad buttons", () => {
    expect(baseLabel("Pad:3")).toBe("Gamepad-Taste 3");
  });

  it("labels mouse wheel directions", () => {
    expect(baseLabel("Mouse:WheelUp")).toBe("Mausrad ▲");
    expect(baseLabel("Mouse:WheelDown")).toBe("Mausrad ▼");
    expect(baseLabel("Mouse:WheelLeft")).toBe("Mausrad ◀");
    expect(baseLabel("Mouse:WheelRight")).toBe("Mausrad ▶");
  });

  it("maps Unknown(n) mouse buttons to the human numbering (Unknown(1) = Maustaste 4)", () => {
    expect(baseLabel("Mouse:Unknown(1)")).toBe("Maustaste 4");
    expect(baseLabel("Mouse:Unknown(2)")).toBe("Maustaste 5");
  });

  it("falls back to 'Maus <button>' for named mouse buttons", () => {
    expect(baseLabel("Mouse:Left")).toBe("Maus Left");
    expect(baseLabel("Mouse:Middle")).toBe("Maus Middle");
  });

  it("returns the empty string for an empty code", () => {
    expect(baseLabel("")).toBe("");
  });
});

describe("pttLabel", () => {
  it("shows a dash for no binding", () => {
    expect(pttLabel("")).toBe("—");
  });

  it("labels a plain key like baseLabel", () => {
    expect(pttLabel("F8")).toBe("F8");
    expect(pttLabel("KeyT")).toBe("T");
    expect(pttLabel("Mouse:Unknown(1)")).toBe("Maustaste 4");
  });

  it("renders chords with ' + ' between modifiers and base", () => {
    expect(pttLabel("Shift+KeyT")).toBe("Shift + T");
    expect(pttLabel("Ctrl+Shift+Mouse:Left")).toBe("Ctrl + Shift + Maus Left");
    expect(pttLabel("Ctrl+Alt+Shift+Meta+F1")).toBe("Ctrl + Alt + Shift + Meta + F1");
  });

  it("drops unknown prefix tokens and keeps the last token as the base", () => {
    expect(pttLabel("Bogus+KeyA")).toBe("A");
  });

  it("keeps the modifier order as written", () => {
    expect(pttLabel("Shift+Ctrl+KeyA")).toBe("Shift + Ctrl + A");
  });
});

describe("timing constants", () => {
  it("double-tap window is shorter than the long-press latch", () => {
    expect(PTT_LATCH_MS).toBe(350);
    expect(PTT_LONGPRESS_MS).toBe(700);
    expect(PTT_LATCH_MS).toBeLessThan(PTT_LONGPRESS_MS);
  });
});
