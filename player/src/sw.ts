/// <reference lib="webworker" />
// The Player's service worker.
//
// - Precaches the app shell (the list is injected at build time).
// - Serves downloaded media from the `media-downloads` cache, answering
//   Range requests by slicing the cached file, so music and films play
//   offline and seek properly.
// - Serves downloaded subtitles and covers from `media-extras`.
// - Keeps a small runtime cache of pictures (/photos) and fonts.
// - Never caches API JSON: the app keeps its own offline details in IndexedDB.
// - Waits to be told before taking over, so an update never interrupts playback.

import { cleanupOutdatedCaches, createHandlerBoundToURL, precacheAndRoute } from 'workbox-precaching';
import { NavigationRoute, registerRoute } from 'workbox-routing';
import { CacheFirst, StaleWhileRevalidate } from 'workbox-strategies';
import { ExpirationPlugin } from 'workbox-expiration';
import { clientsClaim } from 'workbox-core';
import { isStreamPath, rangeResponse } from './sw/range';

declare const self: ServiceWorkerGlobalScope;

const DOWNLOAD_CACHE = 'media-downloads';
const EXTRAS_CACHE = 'media-extras';

precacheAndRoute(self.__WB_MANIFEST);
cleanupOutdatedCaches();
// Take charge of open pages once active, so the very first visit can already
// download and play offline. (A new version still waits to be told; see below.)
clientsClaim();

// Client-side routes all load the app shell.
registerRoute(
  new NavigationRoute(createHandlerBoundToURL(`${import.meta.env.BASE_URL}index.html`), {
    allowlist: [new RegExp(`^${import.meta.env.BASE_URL}`)],
  }),
);

registerRoute(
  ({ url, request }) =>
    url.origin === self.location.origin && url.pathname.startsWith('/static/fonts/') && request.method === 'GET',
  new CacheFirst({
    cacheName: 'fonts',
    plugins: [new ExpirationPlugin({ maxEntries: 30, maxAgeSeconds: 365 * 24 * 60 * 60 })],
  }),
);

/** Answers from the downloads cache, slicing for Range requests; null when not downloaded. */
async function fromDownloads(request: Request): Promise<Response | null> {
  const cache = await caches.open(DOWNLOAD_CACHE);
  const cached = await cache.match(request.url, { ignoreVary: true, ignoreSearch: true });
  if (!cached) return null;
  const blob = await cached.blob();
  const type = cached.headers.get('Content-Type') ?? 'application/octet-stream';
  return rangeResponse(blob, request.headers.get('Range'), type);
}

async function fromExtras(request: Request): Promise<Response | null> {
  const cache = await caches.open(EXTRAS_CACHE);
  return (await cache.match(request.url, { ignoreVary: true })) ?? null;
}

const photos = new StaleWhileRevalidate({
  cacheName: 'photos',
  plugins: [new ExpirationPlugin({ maxEntries: 400, maxAgeSeconds: 30 * 24 * 60 * 60 })],
});

self.addEventListener('fetch', (event) => {
  const { request } = event;
  if (request.method !== 'GET') return;
  const url = new URL(request.url);
  if (url.origin !== self.location.origin) return;

  if (isStreamPath(url.pathname)) {
    // Downloaded? Serve it from the device. Otherwise fetch it from the network as usual.
    event.respondWith(fromDownloads(request).then((response) => response ?? fetch(request)));
    return;
  }

  if (url.pathname.startsWith('/api/subtitles/')) {
    event.respondWith(fromExtras(request).then((response) => response ?? fetch(request)));
    return;
  }

  if (url.pathname.startsWith('/photos/')) {
    event.respondWith(fromExtras(request).then((response) => response ?? photos.handle({ event, request })));
  }
  // Everything else (including all other /api requests) goes straight to the network.
});

self.addEventListener('message', (event) => {
  if (event.data && typeof event.data === 'object' && (event.data as { type?: unknown }).type === 'SKIP_WAITING') {
    void self.skipWaiting();
  }
});
