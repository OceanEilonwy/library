// The device's own database (IndexedDB): listen history, downloaded files'
// details and a few remembered values. Nothing here is sent to the server.

import { openDB, type DBSchema, type IDBPDatabase } from 'idb';
import type { MediaFile } from './api';

export type HistoryEntry = {
  id?: number;
  /** Unix milliseconds. */
  playedAt: number;
  file: MediaFile;
};

export type DownloadRecord = {
  /** The media file id. */
  id: number;
  itemId: number;
  file: MediaFile;
  /** Bytes stored in the cache. */
  sizeBytes: number;
  /** Unix milliseconds. */
  downloadedAt: number;
};

export interface PlayerDb extends DBSchema {
  history: { key: number; value: HistoryEntry; indexes: { playedAt: number } };
  downloads: { key: number; value: DownloadRecord; indexes: { itemId: number } };
  kv: { key: string; value: unknown };
}

export const DB_NAME = 'library-player';

export function openPlayerDb(name: string = DB_NAME): Promise<IDBPDatabase<PlayerDb>> {
  return openDB<PlayerDb>(name, 1, {
    upgrade(db) {
      const history = db.createObjectStore('history', { keyPath: 'id', autoIncrement: true });
      history.createIndex('playedAt', 'playedAt');
      const downloads = db.createObjectStore('downloads', { keyPath: 'id' });
      downloads.createIndex('itemId', 'itemId');
      db.createObjectStore('kv');
    },
  });
}

let shared: Promise<IDBPDatabase<PlayerDb>> | undefined;

/** The app's database, opened once. */
export function playerDb(): Promise<IDBPDatabase<PlayerDb>> {
  shared ??= openPlayerDb();
  return shared;
}

/** Small key/value helpers over the `kv` store. */
export async function kvGet<T>(db: IDBPDatabase<PlayerDb>, key: string): Promise<T | undefined> {
  return (await db.get('kv', key)) as T | undefined;
}

export async function kvSet(db: IDBPDatabase<PlayerDb>, key: string, value: unknown): Promise<void> {
  await db.put('kv', value, key);
}

export async function kvDelete(db: IDBPDatabase<PlayerDb>, key: string): Promise<void> {
  await db.delete('kv', key);
}
