// The app's own offline copies of a few API answers (favourites, playlists),
// kept in IndexedDB so those screens still show something with no network.
// The service worker never caches API JSON; this is the only place it lives.

import type { IDBPDatabase } from 'idb';
import { kvGet, kvSet, type PlayerDb } from '../db';

export function createMemory(db: () => Promise<IDBPDatabase<PlayerDb>>) {
  return {
    async save(key: string, value: unknown): Promise<void> {
      try {
        await kvSet(await db(), `cache:${key}`, value);
      } catch {
        // Not remembered: the screen just won't work offline.
      }
    },
    async load<T>(key: string): Promise<T | undefined> {
      try {
        return await kvGet<T>(await db(), `cache:${key}`);
      } catch {
        return undefined;
      }
    },
  };
}

export type Memory = ReturnType<typeof createMemory>;

/**
 * Fetches with `load`, remembering the answer under `key`; when offline,
 * answers from the remembered copy instead (rethrowing if there is none).
 */
export async function withMemory<T>(
  memory: Memory,
  key: string,
  load: () => Promise<T>,
  isOffline: (error: unknown) => boolean,
): Promise<T> {
  try {
    const value = await load();
    void memory.save(key, value);
    return value;
  } catch (error) {
    if (!isOffline(error)) throw error;
    const remembered = await memory.load<T>(key);
    if (remembered === undefined) throw error;
    return remembered;
  }
}
