import { describe, expect, it } from "vitest";
import {
  LINK_SCHEMES,
  SESSION_BASE,
  TRUSTED_LINK_HOSTS,
  baseFromInput,
  isTrustedLinkHost,
  linkHost,
  parseCode,
  parseDirectLink,
  sanitizeUid,
} from "./links";

describe("parseCode", () => {
  it("extracts the code from a share link on any host", () => {
    expect(parseCode("https://subraum.cc/j/AB12cd")).toBe("AB12cd");
    expect(parseCode("https://squadlink.raumdock.org/j/XYZ789?x=1")).toBe("XYZ789");
    expect(parseCode("  https://subraum.cc/j/Q1  ")).toBe("Q1");
  });

  it("returns a bare code trimmed", () => {
    expect(parseCode(" ABC123 ")).toBe("ABC123");
    expect(parseCode("")).toBe("");
  });

  it("stops the code at the first non-alphanumeric character", () => {
    expect(parseCode("https://subraum.cc/j/AB-12")).toBe("AB");
  });
});

describe("baseFromInput", () => {
  it("uses the link's own origin for http(s) share links", () => {
    expect(baseFromInput("https://squadlink.raumdock.org/j/ABC")).toBe("https://squadlink.raumdock.org");
    expect(baseFromInput("https://subraum.cc/j/ABC")).toBe("https://subraum.cc");
    expect(baseFromInput("http://localhost:8090/j/ABC")).toBe("http://localhost:8090");
  });

  it("falls back to the hosted default for bare codes and unparsable links", () => {
    expect(baseFromInput("ABC123")).toBe(SESSION_BASE);
    expect(baseFromInput("")).toBe(SESSION_BASE);
    expect(baseFromInput("https://")).toBe(SESSION_BASE);
    expect(baseFromInput("ftp://x/j/A")).toBe(SESSION_BASE);
  });

  it("is case-sensitive on the scheme prefix, as the regex is", () => {
    // "HTTPS://" does not match /^https?:\/\// → default host (documented behavior).
    expect(baseFromInput("HTTPS://subraum.cc/j/A")).toBe(SESSION_BASE);
  });

  it("default base is subraum.cc", () => {
    expect(SESSION_BASE).toBe("https://subraum.cc");
  });
});

describe("parseDirectLink", () => {
  const full = "subraum://connect?ws=wss%3A%2F%2Fsubraum.cc%2Fws&room=op-42&token=abc&name=Falcon&uid=disc%23123";

  it("parses every field of a full link", () => {
    expect(parseDirectLink(full)).toEqual({
      ws: "wss://subraum.cc/ws",
      room: "op-42",
      token: "abc",
      name: "Falcon",
      uid: "disc#123",
    });
  });

  it("accepts the pre-rename squadlink: scheme with the same payload", () => {
    const r = parseDirectLink(full.replace("subraum://", "squadlink://"));
    expect(r?.room).toBe("op-42");
    expect(LINK_SCHEMES).toEqual(["subraum:", "squadlink:"]);
  });

  it("accepts an uppercase scheme because the URL parser normalises it", () => {
    expect(parseDirectLink(full.replace("subraum://", "SUBRAUM://"))?.room).toBe("op-42");
  });

  it("tolerates surrounding whitespace", () => {
    expect(parseDirectLink(`  ${full}\n`)?.ws).toBe("wss://subraum.cc/ws");
  });

  it("token is null and name/uid undefined when absent or empty", () => {
    const r = parseDirectLink("subraum://connect?ws=wss://h/ws&room=r&name=&uid=");
    expect(r).toEqual({ ws: "wss://h/ws", room: "r", token: null, name: undefined, uid: undefined });
  });

  it("rejects other schemes, missing ws/room and non-URLs", () => {
    expect(parseDirectLink("https://subraum.cc/connect?ws=wss://h&room=r")).toBeNull();
    expect(parseDirectLink("subraum://connect?room=r")).toBeNull();
    expect(parseDirectLink("subraum://connect?ws=wss://h")).toBeNull();
    expect(parseDirectLink("subraum://connect?ws=&room=r")).toBeNull();
    expect(parseDirectLink("not a url")).toBeNull();
    expect(parseDirectLink("")).toBeNull();
  });
});

describe("linkHost / isTrustedLinkHost", () => {
  it("returns the lowercased hostname without port", () => {
    expect(linkHost("wss://Subraum.cc:443/ws")).toBe("subraum.cc");
    expect(linkHost("wss://squadlink.raumdock.org/ws")).toBe("squadlink.raumdock.org");
    expect(linkHost("ws://127.0.0.1:8090/ws")).toBe("127.0.0.1");
  });

  it("returns null for something that is not a URL", () => {
    expect(linkHost("")).toBeNull();
    expect(linkHost("no scheme here")).toBeNull();
  });

  it("trusts exactly the two known hosts, nothing else", () => {
    expect(TRUSTED_LINK_HOSTS).toEqual(["subraum.cc", "squadlink.raumdock.org"]);
    expect(isTrustedLinkHost("subraum.cc")).toBe(true);
    expect(isTrustedLinkHost("SUBRAUM.CC")).toBe(true);
    expect(isTrustedLinkHost("squadlink.raumdock.org")).toBe(true);
    expect(isTrustedLinkHost("evil.subraum.cc")).toBe(false);
    expect(isTrustedLinkHost("subraum.cc.evil.example")).toBe(false);
    expect(isTrustedLinkHost("")).toBe(false);
  });
});

describe("sanitizeUid", () => {
  it("keeps only the backend id charset", () => {
    expect(sanitizeUid("Falcon#1234")).toBe("Falcon1234");
    expect(sanitizeUid("a.b_c-d")).toBe("a.b_c-d");
    expect(sanitizeUid("Ümlaut Ärger!")).toBe("mlautrger");
  });

  it("bounds to 64 chars", () => {
    expect(sanitizeUid("x".repeat(100))).toHaveLength(64);
  });

  it("is empty for undefined, null, empty or all-invalid input", () => {
    expect(sanitizeUid(undefined)).toBe("");
    expect(sanitizeUid(null)).toBe("");
    expect(sanitizeUid("")).toBe("");
    expect(sanitizeUid("###")).toBe("");
  });
});
