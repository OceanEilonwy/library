# The Player

The Library's listening room: a small installable web app (PWA) for the
digital copies in the catalogue. Music plays with a queue, favourites and
history; films play with subtitles; documents open in the browser's viewer.
Anything can be downloaded to play offline.

It is served by the Rust server at **`/player/`** and talks to the JSON API
described in [`docs/player.md`](../docs/player.md), which is the contract for
everything here.

## Commands

Run these from this folder (or add `--prefix player` from the repository root).

| Command                | What it does                                                                                                                                       |
| ---------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| `npm ci`               | Install exactly what `package-lock.json` lists.                                                                                                    |
| `npm run dev`          | Vite on <http://localhost:5173/player/>, proxying `/api`, `/photos` and `/static` to the Rust server on `127.0.0.1:8080` (or `$LIBRARY_BACKEND`).  |
| `npm run dev:mock`     | The same, but with a pretend API (`scripts/mock-api.ts`) so you can click through every screen without the server. Sign in as `demo` / `password`. |
| `npm run build`        | Type-check, then build into `dist/` (app shell, `sw.js`, `manifest.webmanifest`, icons). The server embeds this folder.                            |
| `npm run preview:mock` | Build and serve `dist/` with the pretend API on <http://localhost:4173/player/>, service worker and all, to try offline downloads.                 |
| `npm test`             | Vitest, once.                                                                                                                                      |
| `npm run typecheck`    | `tsc --noEmit` for the app and for `e2e/`.                                                                                                         |
| `npm run test:e2e`     | Playwright end-to-end tests against the real Rust server (run `npm run build` first; see Tests).                                                   |
| `npm run format`       | Prettier over the project.                                                                                                                         |
| `npm run icons`        | Re-render the PNG icons in `public/icons/` from `icons/*.svg`.                                                                                     |

Service workers only run on `localhost` or over HTTPS. Opened over plain
HTTP from another device the Player still works, but cannot be installed or
download anything (it says so when you try).

## Versions

| Package                    | Version | Notes                                                                                                                                                                                                            |
| -------------------------- | ------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `solid-js`, `@solidjs/web` | 2.0.2   | Solid 2. `render` is in `@solidjs/web`; stores are in `solid-js`.                                                                                                                                                |
| `@solidjs/router`          | 2.0.0   | Config-object routes, plain `<a>` links, `base: '/player'`.                                                                                                                                                      |
| `@solidjs/vite-plugin`     | 3.0.1   | `vite-plugin-solid@next` (3.0.0-next.27) is now only a shim that re-exports this renamed package, so we depend on it directly. It brings `@solidjs/babel-plugin` 2.0.2; `babel-preset-solid@next` is not needed. |
| `vite`                     | 8.3     |                                                                                                                                                                                                                  |
| `vite-plugin-pwa`          | 2.0.0   | `injectManifest` with our own `src/sw.ts`. Its peer range includes Vite 8.                                                                                                                                       |
| `workbox-*`                | 7.4     | Precaching, routing, strategies, expiration, window.                                                                                                                                                             |
| `typescript`               | 7.0     | The native compiler; `strict`, `noUncheckedIndexedAccess`.                                                                                                                                                       |
| `vitest`                   | 5.1     | jsdom 29, `@solidjs/testing-library` 1.0.0-rc.2, user-event 14, `fake-indexeddb` 6.                                                                                                                              |
| `idb`                      | 8       | A thin promise wrapper over IndexedDB.                                                                                                                                                                           |

`npm audit` reports four moderate advisories, all in build-time tooling
(`workbox-build`'s `minimatch`, pulled in by `vite-plugin-pwa`); nothing in
the shipped app. The suggested fix downgrades `workbox-build`, so it is left
alone.

## How it fits together

```
src/
  api.ts              typed client for every endpoint; errors become ApiError with a kind
                      (offline, unauthorized → session ends, validation with fields, …)
  db.ts               the device's IndexedDB: history, download records, small remembered values
  history.ts          listen history store (10 s or a finished track counts as a listen)
  downloads.ts        downloads store: fetch with progress → Cache API + IndexedDB record,
                      remove, purge via /api/media/check, storage estimate
  playlists.ts        display rules: visible playlists, offline availability, reorder id lists
  update.ts           pure "may we switch to the new version now?" decision
  reorder.ts          pointer drag reordering (mouse, pen, touch) and its pure drop maths
  player/
    queue.ts          the queue as a pure immutable value (play list, play next, add, remove,
                      move, shuffle keeping the current track first, repeat off/all/one)
    engine.ts         one persistent <audio> element driven by the queue; testable with a fake
    mediaSession.ts   lock-screen / headset controls and position state
    keyboard.ts       Space, ←/→, Shift+N/P
  services/           reactive (Solid signal) wrappers that screens share through context:
                      session, network, favourites, downloads, history, player, updates,
                      install prompt, toasts and sheets, offline copies of API answers
  sw.ts, sw/range.ts  the service worker and its pure Range-slicing helper
  components/         mini player, Now Playing, scrub bar, Up next, menus, rows, sheets…
  pages/              one file per route
  Shell.tsx           navigation (tabs on phones, rail on wide screens), gate, overlays
  styles.css          one stylesheet: tokens, layout, components, screens, motion
```

**Playback.** `index.html` holds a single `<audio id="player-audio">` that
lives for the whole session, so music carries on across screens. The engine
owns it; the pure queue decides what plays next. The queue is saved to
`localStorage` so a reload (for example after an update) brings it back,
paused.

**Downloads.** A download fetches the whole file, stores it in the
`media-downloads` cache under its stream URL and records the `MediaFile` in
IndexedDB. The service worker answers `/api/media/{id}/stream` from that
cache when present, slicing the blob for `Range` requests (206), so seeking
works offline. Subtitles and covers go to `media-extras`. On every sign-in
the app asks the server which downloads are still allowed and removes the
rest.

**Offline.** With no network the app opens from its precache, signs in from
the remembered account, and shows downloads, favourites and visited
playlists from IndexedDB. API JSON is never cached by the service worker.

**Updates.** A new version installs in the background. The app switches to
it (and reloads) only when nothing is playing, no download is running, and
you move between screens or hide the tab; otherwise it takes over on the next
launch. There is never a prompt.

## Writing code here

- Solid 2 is not Solid 1 (nor React). Props are values (`<X value={count()} />`),
  effects take two functions (compute, then apply), and a signal read straight
  after a write returns the old value until the next microtask; tests call
  `flush()` before asserting. `node_modules/solid-js/CHEATSHEET.md` is the
  quickest reference.
- Keep logic in plain modules with tests (queue, range slicing, update
  decision, playlist rules) and keep components thin.
- User-facing text is plain, friendly English: say what happened and what to
  do next.

## Tests

`npm test` runs 205 Vitest tests: the queue, engine (fake audio
element), Media Session (fake `navigator.mediaSession`), Range slicing,
downloads (fake Cache API + `fake-indexeddb`), history, playlist rules, the
API client's error mapping, the update decision, and components (mini player,
Now Playing, scrub bar keyboard and drag, sign-in and registration, subtitle
picker, track menus, playlist sheet, playlist editing, downloads screen).

`npm run test:e2e` runs the Playwright tests in `e2e/` with headless
Chromium (install it once with `npx playwright install chromium`). The
global setup seeds a throwaway data folder with `../scripts/dev-seed.py`
(accounts, items, files uploaded through the real studio endpoint,
playlists), starts the server on port 18431 (`E2E_PORT`) and removes it all
afterwards (`E2E_KEEP=1` keeps the folder and server log). It uses
`LIBRARY_BIN` if set, otherwise `cargo build`. The server serves `dist/`, so
build first.
