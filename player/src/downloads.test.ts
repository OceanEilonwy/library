// @vitest-environment node
import { openPlayerDb } from './db';
import { DOWNLOAD_CACHE, EXTRAS_CACHE, createDownloadStore, groupDownloads, DownloadError } from './downloads';
import { FakeCacheStorage } from './test/fakeCaches';
import { mediaFile } from './test/fixtures';

let counter = 0;

function setup(responses: Record<string, () => Response> = {}) {
  counter += 1;
  const db = openPlayerDb(`downloads-test-${counter}`);
  const caches = new FakeCacheStorage();
  const fetch = vi.fn(async (input: RequestInfo | URL) => {
    const url = String(input);
    const make = responses[url];
    if (make) return make();
    if (url.endsWith('/stream'))
      return new Response('x'.repeat(10), { headers: { 'Content-Type': 'audio/flac', 'Content-Length': '10' } });
    return new Response('extra', { status: 200 });
  });
  const storage = {
    persist: vi.fn(async () => true),
    persisted: vi.fn(async () => false),
    estimate: vi.fn(async () => ({ usage: 1234, quota: 99999 })),
  };
  const store = createDownloadStore({
    db: () => db,
    caches,
    fetch: fetch as unknown as typeof globalThis.fetch,
    storage,
    now: () => 42,
  });
  return { store, caches, fetch, storage };
}

test('downloads a file into the cache with progress and remembers it', async () => {
  const { store, caches } = setup();
  const progress: number[] = [];
  const record = await store.download(mediaFile(1), { onProgress: (received) => progress.push(received) });
  expect(record).toMatchObject({ id: 1, itemId: 1, sizeBytes: 10, downloadedAt: 42 });
  expect(progress.at(-1)).toBe(10);
  expect(caches.urls(DOWNLOAD_CACHE)).toEqual(['/api/media/1/stream']);
  const cached = await (await caches.open(DOWNLOAD_CACHE)).match('/api/media/1/stream');
  expect(cached?.headers.get('Content-Type')).toBe('audio/flac');
  expect(await cached?.text()).toBe('xxxxxxxxxx');
  expect((await store.list()).map((r) => r.id)).toEqual([1]);
});

test('caches subtitles and the cover too', async () => {
  const { store, caches } = setup();
  await store.download(
    mediaFile(5, { kind: 'video', subtitles: [{ id: 9, language: 'en', label: 'English', url: '/api/subtitles/9' }] }),
  );
  expect(caches.urls(EXTRAS_CACHE).sort()).toEqual(['/api/subtitles/9', '/photos/blue-train.jpg']);
});

test('asks for persistent storage on the first download only', async () => {
  const { store, storage } = setup();
  await store.download(mediaFile(1));
  await store.download(mediaFile(2));
  expect(storage.persist).toHaveBeenCalledOnce();
});

test('failed downloads explain themselves and save nothing', async () => {
  const { store, caches } = setup({ '/api/media/3/stream': () => new Response('{}', { status: 404 }) });
  await expect(store.download(mediaFile(3))).rejects.toBeInstanceOf(DownloadError);
  expect(caches.urls(DOWNLOAD_CACHE)).toEqual([]);
  expect(await store.list()).toEqual([]);
});

test('network failures become a plain message', async () => {
  const { store } = setup({
    '/api/media/3/stream': () => {
      throw new TypeError('Failed to fetch');
    },
  });
  await expect(store.download(mediaFile(3))).rejects.toThrow(/Check your connection/);
});

test('removing a download keeps extras other downloads still use', async () => {
  const { store, caches } = setup();
  await store.download(mediaFile(1));
  await store.download(mediaFile(2));
  await store.remove(1);
  expect(caches.urls(DOWNLOAD_CACHE)).toEqual(['/api/media/2/stream']);
  expect(caches.urls(EXTRAS_CACHE)).toEqual(['/photos/blue-train.jpg']);
  await store.remove(2);
  expect(caches.urls(EXTRAS_CACHE)).toEqual([]);
});

test('removes a whole item', async () => {
  const { store } = setup();
  await store.download(mediaFile(1));
  await store.download(mediaFile(2));
  await store.download(mediaFile(3, { itemId: 2 }));
  await store.removeItem(1);
  expect((await store.list()).map((r) => r.id)).toEqual([3]);
});

test('purges downloads the server no longer allows', async () => {
  const { store } = setup();
  await store.download(mediaFile(1));
  await store.download(mediaFile(2));
  const check = vi.fn(async (ids: number[]) => ({ allowed: ids.filter((id) => id !== 2) }));
  expect(await store.purge(check)).toBe(1);
  expect(check).toHaveBeenCalledWith([1, 2]);
  expect((await store.list()).map((r) => r.id)).toEqual([1]);
});

test('purge with nothing downloaded does not call the server', async () => {
  const { store } = setup();
  const check = vi.fn();
  expect(await store.purge(check)).toBe(0);
  expect(check).not.toHaveBeenCalled();
});

test('clear all empties caches and records', async () => {
  const { store, caches } = setup();
  await store.download(mediaFile(1));
  await store.clearAll();
  expect(await caches.keys()).not.toContain(DOWNLOAD_CACHE);
  expect(await store.list()).toEqual([]);
});

test('clearing other data keeps downloads and the app shell', async () => {
  const { store, caches } = setup();
  await store.download(mediaFile(1));
  await caches.open('photos');
  await caches.open('fonts');
  await caches.open('workbox-precache-v2-http://localhost/player/');
  expect(await store.clearOtherCaches()).toBe(2);
  expect((await caches.keys()).sort()).toEqual(
    [DOWNLOAD_CACHE, EXTRAS_CACHE, 'workbox-precache-v2-http://localhost/player/'].sort(),
  );
});

test('reports the storage estimate', async () => {
  const { store } = setup();
  expect(await store.estimate()).toEqual({ usage: 1234, quota: 99999 });
});

test('groups downloads by item with sizes', () => {
  const records = [
    { id: 2, itemId: 1, file: mediaFile(2), sizeBytes: 20, downloadedAt: 0 },
    { id: 9, itemId: 7, file: mediaFile(9, { itemId: 7, itemTitle: 'A Love Supreme' }), sizeBytes: 5, downloadedAt: 0 },
    { id: 1, itemId: 1, file: mediaFile(1), sizeBytes: 10, downloadedAt: 0 },
  ];
  const groups = groupDownloads(records);
  expect(groups.map((g) => g.itemTitle)).toEqual(['A Love Supreme', 'Blue Train']);
  expect(groups[1]?.records.map((r) => r.id)).toEqual([1, 2]);
  expect(groups[1]?.sizeBytes).toBe(30);
});

test('without the Cache API, downloads explain why they cannot work', async () => {
  const db = openPlayerDb(`downloads-test-nocache`);
  const store = createDownloadStore({ db: () => db, caches: null, fetch: vi.fn() as unknown as typeof fetch });
  await expect(store.download(mediaFile(1))).rejects.toThrow(/https/);
  expect(await store.clearOtherCaches()).toBe(0);
});
