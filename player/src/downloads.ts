// Downloads: whole files saved on the device for offline playback.
//
// The bytes go into the Cache API (`media-downloads`) under the file's stream
// URL, where the service worker finds them and answers Range requests by
// slicing (see sw/range.ts). Subtitles and covers go into `media-extras`.
// The file's details (`MediaFile`) are kept in IndexedDB so the app can list
// and play downloads with no network at all.

import type { IDBPDatabase } from 'idb';
import type { MediaFile } from './api';
import { kvGet, kvSet, type DownloadRecord, type PlayerDb } from './db';

export const DOWNLOAD_CACHE = 'media-downloads';
export const EXTRAS_CACHE = 'media-extras';
/** Caches the app needs to keep working offline; "clear other data" leaves these alone. */
const KEEP_CACHE_PREFIXES = ['workbox-precache'];

export type CacheStorageLike = Pick<CacheStorage, 'open' | 'delete' | 'keys'>;
export type StorageManagerLike = Partial<Pick<StorageManager, 'persist' | 'persisted' | 'estimate'>>;

export type DownloadDeps = {
  db: () => Promise<IDBPDatabase<PlayerDb>>;
  /** Null where the browser offers no Cache API (pages served over plain http to another device). */
  caches: CacheStorageLike | null;
  fetch: typeof fetch;
  storage?: StorageManagerLike | undefined;
  now?: () => number;
};

export type DownloadProgress = (received: number, total: number) => void;

/** Why a download failed, in plain English. */
export class DownloadError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'DownloadError';
  }
}

async function readWithProgress(
  response: Response,
  expected: number,
  onProgress?: DownloadProgress,
  signal?: AbortSignal,
): Promise<Blob> {
  const total = Number(response.headers.get('Content-Length')) || expected;
  const type = response.headers.get('Content-Type') ?? '';
  if (!response.body) {
    const blob = await response.blob();
    onProgress?.(blob.size, blob.size);
    return blob;
  }
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let received = 0;
  onProgress?.(0, total);
  for (;;) {
    if (signal?.aborted) {
      await reader.cancel();
      throw new DownloadError('Download cancelled.');
    }
    const { done, value } = await reader.read();
    if (done) break;
    chunks.push(value);
    received += value.byteLength;
    onProgress?.(received, Math.max(total, received));
  }
  return new Blob(chunks as BlobPart[], { type });
}

/** Sort order for listing downloads: by item, then disc, track and title. */
export function compareDownloads(a: DownloadRecord, b: DownloadRecord): number {
  return (
    a.file.itemTitle.localeCompare(b.file.itemTitle) ||
    a.itemId - b.itemId ||
    a.file.disc - b.file.disc ||
    (a.file.track ?? 0) - (b.file.track ?? 0) ||
    a.file.title.localeCompare(b.file.title)
  );
}

export type DownloadGroup = {
  itemId: number;
  itemTitle: string;
  creator: string;
  cover: string | null;
  sizeBytes: number;
  records: DownloadRecord[];
};

/** Groups (sorted) downloads by item, for the downloads screen. */
export function groupDownloads(records: readonly DownloadRecord[]): DownloadGroup[] {
  const groups: DownloadGroup[] = [];
  for (const record of [...records].sort(compareDownloads)) {
    const last = groups[groups.length - 1];
    if (last && last.itemId === record.itemId) {
      last.records.push(record);
      last.sizeBytes += record.sizeBytes;
    } else {
      groups.push({
        itemId: record.itemId,
        itemTitle: record.file.itemTitle,
        creator: record.file.creator,
        cover: record.file.cover,
        sizeBytes: record.sizeBytes,
        records: [record],
      });
    }
  }
  return groups;
}

/** URLs of the extras (subtitles, cover) a file brings with it. */
function extrasOf(file: MediaFile): string[] {
  const urls = file.subtitles.map((subtitle) => subtitle.url);
  if (file.cover) urls.push(file.cover);
  return urls;
}

const NEEDS_SECURE = 'Downloads need the Player to be opened over https (or on this computer).';

export function createDownloadStore(deps: DownloadDeps) {
  const now = deps.now ?? Date.now;
  const caches = deps.caches;

  async function requestPersistenceOnce(database: IDBPDatabase<PlayerDb>) {
    if (await kvGet<boolean>(database, 'persistRequested')) return;
    await kvSet(database, 'persistRequested', true);
    try {
      if (deps.storage?.persisted && (await deps.storage.persisted())) return;
      await deps.storage?.persist?.();
    } catch {
      // Not supported or refused: downloads still work, the browser may just evict them under pressure.
    }
  }

  async function cacheExtras(file: MediaFile) {
    if (!caches) return;
    const cache = await caches.open(EXTRAS_CACHE);
    await Promise.all(
      extrasOf(file).map(async (url) => {
        try {
          if (await cache.match(url)) return;
          const response = await deps.fetch(url, { credentials: 'same-origin' });
          if (response.ok) await cache.put(url, response);
        } catch {
          // Extras are a nicety; the download itself still counts.
        }
      }),
    );
  }

  /** Removes cached extras that no remaining download refers to. */
  async function dropUnusedExtras(removed: readonly MediaFile[]) {
    const database = await deps.db();
    const remaining = await database.getAll('downloads');
    const stillUsed = new Set(remaining.flatMap((record) => extrasOf(record.file)));
    if (!caches) return;
    const cache = await caches.open(EXTRAS_CACHE);
    await Promise.all(
      removed
        .flatMap(extrasOf)
        .filter((url) => !stillUsed.has(url))
        .map((url) => cache.delete(url)),
    );
  }

  const store = {
    async list(): Promise<DownloadRecord[]> {
      const records = await (await deps.db()).getAll('downloads');
      return records.sort(compareDownloads);
    },

    async get(id: number): Promise<DownloadRecord | undefined> {
      return (await deps.db()).get('downloads', id);
    },

    async forItem(itemId: number): Promise<DownloadRecord[]> {
      const records = await (await deps.db()).getAllFromIndex('downloads', 'itemId', itemId);
      return records.sort(compareDownloads);
    },

    /** Downloads `file` in full, caches it and remembers it. Resolves with the saved record. */
    async download(
      file: MediaFile,
      options: { onProgress?: DownloadProgress; signal?: AbortSignal } = {},
    ): Promise<DownloadRecord> {
      if (!caches) throw new DownloadError(NEEDS_SECURE);
      const database = await deps.db();
      await requestPersistenceOnce(database);
      let response: Response;
      try {
        response = await deps.fetch(file.streamUrl, {
          credentials: 'same-origin',
          ...(options.signal ? { signal: options.signal } : {}),
        });
      } catch {
        throw new DownloadError(`Couldn't download “${file.title}”. Check your connection and try again.`);
      }
      if (response.status === 401) throw new DownloadError('Your session has ended. Sign in again to download.');
      if (!response.ok)
        throw new DownloadError(`Couldn't download “${file.title}” (the server said ${response.status}).`);
      const blob = await readWithProgress(response, file.sizeBytes, options.onProgress, options.signal);
      const contentType = file.mime || blob.type || 'application/octet-stream';
      const cache = await caches.open(DOWNLOAD_CACHE);
      try {
        await cache.put(
          file.streamUrl,
          new Response(blob, {
            status: 200,
            headers: { 'Content-Type': contentType, 'Content-Length': String(blob.size), 'Accept-Ranges': 'bytes' },
          }),
        );
      } catch {
        throw new DownloadError("There isn't enough space on this device. Free some up on the Downloads screen.");
      }
      await cacheExtras(file);
      const record: DownloadRecord = {
        id: file.id,
        itemId: file.itemId,
        file,
        sizeBytes: blob.size,
        downloadedAt: now(),
      };
      await database.put('downloads', record);
      return record;
    },

    async remove(id: number): Promise<void> {
      const database = await deps.db();
      const record = await database.get('downloads', id);
      if (!record) return;
      if (caches) await (await caches.open(DOWNLOAD_CACHE)).delete(record.file.streamUrl);
      await database.delete('downloads', id);
      await dropUnusedExtras([record.file]);
    },

    async removeItem(itemId: number): Promise<void> {
      for (const record of await store.forItem(itemId)) await store.remove(record.id);
    },

    async clearAll(): Promise<void> {
      await caches?.delete(DOWNLOAD_CACHE);
      await caches?.delete(EXTRAS_CACHE);
      await (await deps.db()).clear('downloads');
    },

    /**
     * Removes downloads the server no longer lets this account open (or that
     * were deleted). Returns how many went.
     */
    async purge(check: (ids: number[]) => Promise<{ allowed: number[] }>): Promise<number> {
      const records = await store.list();
      if (records.length === 0) return 0;
      const { allowed } = await check(records.map((record) => record.id));
      const keep = new Set(allowed);
      const gone = records.filter((record) => !keep.has(record.id));
      for (const record of gone) await store.remove(record.id);
      return gone.length;
    },

    /** Deletes other caches (pictures, fonts) but keeps downloads and the app itself. */
    async clearOtherCaches(): Promise<number> {
      if (!caches) return 0;
      const names = await caches.keys();
      const doomed = names.filter(
        (name) =>
          name !== DOWNLOAD_CACHE &&
          name !== EXTRAS_CACHE &&
          !KEEP_CACHE_PREFIXES.some((prefix) => name.startsWith(prefix)),
      );
      await Promise.all(doomed.map((name) => caches.delete(name)));
      return doomed.length;
    },

    /** Storage used and available on this device, if the browser says. */
    async estimate(): Promise<{ usage: number; quota: number } | null> {
      try {
        const estimate = await deps.storage?.estimate?.();
        if (!estimate) return null;
        return { usage: estimate.usage ?? 0, quota: estimate.quota ?? 0 };
      } catch {
        return null;
      }
    },
  };
  return store;
}

export type DownloadStore = ReturnType<typeof createDownloadStore>;
