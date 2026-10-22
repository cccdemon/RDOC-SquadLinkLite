import { describe, expect, it } from "vitest";
import { isNewer, mdToText, topChangelogSection } from "./version";

describe("isNewer", () => {
  it("compares dotted versions numerically, not lexically", () => {
    expect(isNewer("0.1.10", "0.1.9")).toBe(true);
    expect(isNewer("0.1.9", "0.1.10")).toBe(false);
    expect(isNewer("0.4.0", "0.3.4")).toBe(true);
    expect(isNewer("1.0.0", "0.99.99")).toBe(true);
  });

  it("is false for equal versions", () => {
    expect(isNewer("0.3.4", "0.3.4")).toBe(false);
    expect(isNewer("0.3", "0.3.0")).toBe(false); // missing part counts as 0
  });

  it("treats a missing or malformed part as 0 so garbage never wins", () => {
    expect(isNewer("0.3.4", "0.3")).toBe(true);
    expect(isNewer("", "0.0.1")).toBe(false);
    expect(isNewer("abc", "0.0.1")).toBe(false);
    expect(isNewer("0.0.1", "abc")).toBe(true);
  });

  it("only looks at the first three components", () => {
    expect(isNewer("0.3.4.9", "0.3.4.1")).toBe(false);
  });
});

describe("topChangelogSection", () => {
  const md = [
    "# Changelog",
    "",
    "intro line",
    "",
    "## Unveröffentlicht",
    "",
    "- **Fix A.** text",
    "",
    "## v0.3.4 — 2026-08-11",
    "",
    "- older",
  ].join("\n");

  it("returns the first ## section up to the next ## heading, trimmed", () => {
    const s = topChangelogSection(md);
    expect(s.startsWith("## Unveröffentlicht")).toBe(true);
    expect(s).toContain("Fix A");
    expect(s).not.toContain("v0.3.4");
    expect(s.endsWith("text")).toBe(true);
  });

  it("returns the whole tail when there is no later heading", () => {
    expect(topChangelogSection("## only\n\nbody\n")).toBe("## only\n\nbody");
  });

  it("returns an empty string when no ## heading exists", () => {
    expect(topChangelogSection("# title\nno sections")).toBe("");
    expect(topChangelogSection("")).toBe("");
  });

  it("does not stop at ### sub-headings", () => {
    const s = topChangelogSection("## a\n### Neu\n- x\n### Behoben\n- y\n## b");
    expect(s).toContain("### Behoben");
    expect(s).not.toContain("## b");
  });
});

describe("mdToText", () => {
  it("strips headings, bold and backticks and turns list markers into bullets", () => {
    const out = mdToText("## Titel\n\n- **Fett.** mit `code`\n* zweiter");
    // The list-marker rule eats leading whitespace including the blank line
    // before the first bullet, so heading and list end up adjacent.
    expect(out).toBe("Titel\n• Fett. mit code\n• zweiter");
  });

  it("keeps a blank line between paragraphs that are not list items", () => {
    expect(mdToText("a\n\nb")).toBe("a\n\nb");
  });

  it("collapses runs of blank lines to one blank line and trims", () => {
    expect(mdToText("\n\na\n\n\n\n\nb\n\n")).toBe("a\n\nb");
  });

  it("leaves plain text untouched", () => {
    expect(mdToText("plain words")).toBe("plain words");
    expect(mdToText("")).toBe("");
  });
});
