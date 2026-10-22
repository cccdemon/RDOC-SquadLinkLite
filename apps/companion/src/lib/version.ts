// Update-check helpers: version comparison and release-note rendering.
// Pure functions; the fetch + state live in App.tsx.

/** Parse "0.1.10" → [0,1,10]; true if `a` is a newer version than `b`.
 *  Compares the first three dotted numbers; missing parts count as 0 and
 *  non-numeric parts as 0 as well, so a malformed tag never "wins". */
export function isNewer(a: string, b: string): boolean {
  const pa = a.split(".").map(Number);
  const pb = b.split(".").map(Number);
  for (let i = 0; i < 3; i++) {
    if ((pa[i] || 0) !== (pb[i] || 0)) return (pa[i] || 0) > (pb[i] || 0);
  }
  return false;
}

/** Newest CHANGELOG section: from the first "## " heading to the next one. */
export function topChangelogSection(md: string): string {
  const start = md.indexOf("## ");
  if (start < 0) return "";
  const next = md.indexOf("\n## ", start + 3);
  return md.slice(start, next < 0 ? undefined : next).trim();
}

/** Render markdown release notes as readable plain text. */
export function mdToText(s: string): string {
  return s
    .replace(/^#{1,6}\s*/gm, "")
    .replace(/^\s*[-*]\s+/gm, "• ")
    .replace(/\*\*(.*?)\*\*/g, "$1")
    .replace(/`/g, "")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}
