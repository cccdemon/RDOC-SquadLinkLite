// Static PNGs the Stream Deck manifest needs (plugin/category icon, action
// list icon, default key background). Key faces are drawn at RUNTIME as SVG
// data URIs by bin/plugin.js — these here are only what appears in the Stream
// Deck UI before the plugin runs. Same subraum mark as store-art/generate.mjs.
// Run once after changing: `node generate-icons.mjs` (needs `npm i sharp`).
import sharp from "sharp";
import { mkdirSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const OUT = join(dirname(fileURLToPath(import.meta.url)), "org.raumdock.subraum.sdPlugin", "imgs");
mkdirSync(OUT, { recursive: true });

// The mark, verbatim from server/init/assets/logo.svg (viewBox 0 0 64 64).
const mark = (x, y, size, stroke = 1) => `
  <g transform="translate(${x} ${y}) scale(${size / 64})">
    <line x1="4" y1="14" x2="60" y2="14" stroke="#3A4152" stroke-width="${2.5 * stroke}" stroke-linecap="round"/>
    <g fill="none" stroke-linecap="round">
      <path d="M32 14 V56 M11 35 H53" stroke="#3D6FD8" stroke-width="${2 * stroke}"/>
      <path d="M32 14 L53 35 L32 56 L11 35 Z" stroke="#7FB0FF" stroke-width="${2.2 * stroke}" stroke-linejoin="round"/>
    </g>
    <g fill="#7FB0FF">
      <circle cx="32" cy="14" r="${3.6 * stroke}"/>
      <circle cx="53" cy="35" r="${3.6 * stroke}"/>
      <circle cx="32" cy="56" r="${3.6 * stroke}"/>
      <circle cx="11" cy="35" r="${3.6 * stroke}"/>
    </g>
  </g>`;

// Transparent background: the Stream Deck UI supplies its own; action-list
// icons are expected to be monochrome-ish glyphs on transparency.
const onTransparent = (size) =>
  `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 ${size} ${size}">${mark(size * 0.08, size * 0.08, size * 0.84)}</svg>`;

// Key default: app-dark tile with the mark, matching the runtime key faces.
const keyTile = (size) =>
  `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 ${size} ${size}">
    <rect width="${size}" height="${size}" rx="${size * 0.125}" fill="#0F131B"/>
    ${mark(size * 0.2, size * 0.2, size * 0.6)}
  </svg>`;

const jobs = [
  ["plugin.png", onTransparent(28), 28],
  ["plugin@2x.png", onTransparent(56), 56],
  ["action.png", onTransparent(20), 20],
  ["action@2x.png", onTransparent(40), 40],
  ["key.png", keyTile(72), 72],
  ["key@2x.png", keyTile(144), 144],
];

for (const [name, svg, size] of jobs) {
  await sharp(Buffer.from(svg)).resize(size, size).png().toFile(join(OUT, name));
  console.log(`${name} ${size}x${size}`);
}
