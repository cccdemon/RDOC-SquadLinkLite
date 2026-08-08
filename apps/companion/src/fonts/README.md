# Brand faces (app)

The same four subset woff2 files the website ships — byte-identical copies of
`server/init/assets/fonts/`, so the app and subraum.cc render the brand in the
same cuts:

| File | Face | Used for |
| --- | --- | --- |
| `mi-400.woff2` | Michroma 400 | the wordmark, and nothing else in this UI |
| `ps-400.woff2` | IBM Plex Sans 400 | body and controls |
| `ps-600.woff2` | IBM Plex Sans 600 | emphasis, button labels |
| `pm-400.woff2` | IBM Plex Mono 400 | codes, PINs, technical labels, the net bar |

Michroma has exactly one cut. Never declare a heavier weight for it — the
browser would synthesise one and it is visibly broken. Emphasis comes from Plex
Sans 600, which is a real file.

Vite bundles these as hashed assets, so they are same-origin and the strict
Tauri CSP (`default-src 'self'`) covers them with no `font-src` entry.

Regeneration is documented once, next to the server copies:
`server/init/assets/font/README.md` in the brand-kit tooling. Copy the result
here rather than subsetting twice — the two sets must stay identical.

Licence: SIL Open Font License 1.1 (Michroma by Vernon Adams, IBM Plex by IBM).
