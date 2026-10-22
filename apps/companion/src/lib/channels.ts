// Channel (frequency) names and chat-line bookkeeping. Mirrors the Rust side's
// matching rule: channels compare case-insensitively after trimming.

export const DEFAULT_CHANNEL = "Funk 1";
export const MAX_CHANNEL_LEN = 32;

/** Cap retained chat lines so a flood of messages can't grow webview memory. */
export const MAX_CHAT_LINES = 500;

/** Canonical form for channel matching: trim + lowercase (mirrors the Rust side). */
export const canonChannel = (s: string) => s.trim().toLowerCase();

/** A channel name as the user typed it, bounded the way the input is. */
export function cleanChannelName(name: string): string {
  return name.trim().slice(0, MAX_CHANNEL_LEN);
}

/** Ordered, deduped channel list in the order the cycle hotkeys and the chips
 *  walk through. First spelling wins; empty names are dropped. */
export function dedupeChannels(labels: string[]): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const label of labels) {
    const k = canonChannel(label);
    if (!k || seen.has(k)) continue;
    seen.add(k);
    out.push(label);
  }
  return out;
}

/** Index of the channel to switch to when cycling prev (dir < 0) / next from
 *  `current` through `list`, wrapping around. null when there is nothing to
 *  cycle to (fewer than two channels). An unknown current counts as index 0. */
export function cycleTarget(list: string[], current: string, dir: number): string | null {
  if (list.length < 2) return null;
  const idx = list.findIndex((c) => canonChannel(c) === canonChannel(current));
  const cur = idx < 0 ? 0 : idx;
  const nextIdx = (cur + (dir < 0 ? -1 : 1) + list.length) % list.length;
  return list[nextIdx];
}

/** Append one line and drop the oldest beyond `max` (default MAX_CHAT_LINES). */
export function appendCapped<T>(lines: T[], line: T, max = MAX_CHAT_LINES): T[] {
  const next = [...lines, line];
  return next.length > max ? next.slice(next.length - max) : next;
}
