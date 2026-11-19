// Listen history, kept on this device only.

import type { IDBPDatabase } from 'idb';
import type { MediaFile } from './api';
import type { HistoryEntry, PlayerDb } from './db';

/** Seconds of listening after which a play counts (or the track finishes first). */
export const LISTEN_THRESHOLD_SECONDS = 10;

/** How many plays we keep; older ones are dropped. */
export const HISTORY_LIMIT = 5000;

export function createHistoryStore(db: () => Promise<IDBPDatabase<PlayerDb>>, limit = HISTORY_LIMIT) {
  async function prune(database: IDBPDatabase<PlayerDb>) {
    const count = await database.count('history');
    if (count <= limit) return;
    let excess = count - limit;
    const tx = database.transaction('history', 'readwrite');
    let cursor = await tx.store.index('playedAt').openCursor();
    while (cursor && excess > 0) {
      await cursor.delete();
      excess -= 1;
      cursor = await cursor.continue();
    }
    await tx.done;
  }

  return {
    /** Records a play of `file` at `playedAt` (unix ms). */
    async record(file: MediaFile, playedAt: number = Date.now()): Promise<HistoryEntry> {
      const database = await db();
      const entry: HistoryEntry = { playedAt, file };
      entry.id = await database.add('history', entry);
      await prune(database);
      return entry;
    },

    /** All plays, newest first. */
    async list(): Promise<HistoryEntry[]> {
      const database = await db();
      const entries = await database.getAllFromIndex('history', 'playedAt');
      return entries.reverse();
    },

    /** Plays whose title, album or artist contains `query` (ignoring case), newest first. */
    async search(query: string): Promise<HistoryEntry[]> {
      return filterHistory(await this.list(), query);
    },

    /** Most recent distinct files, for "Continue listening". */
    async recent(count: number): Promise<MediaFile[]> {
      const seen = new Set<number>();
      const files: MediaFile[] = [];
      for (const entry of await this.list()) {
        if (seen.has(entry.file.id)) continue;
        seen.add(entry.file.id);
        files.push(entry.file);
        if (files.length >= count) break;
      }
      return files;
    },

    async remove(id: number): Promise<void> {
      await (await db()).delete('history', id);
    },

    async clear(): Promise<void> {
      await (await db()).clear('history');
    },
  };
}

export type HistoryStore = ReturnType<typeof createHistoryStore>;

export function filterHistory(entries: readonly HistoryEntry[], query: string): HistoryEntry[] {
  const needle = query.trim().toLocaleLowerCase();
  if (!needle) return [...entries];
  return entries.filter(({ file }) =>
    [file.title, file.itemTitle, file.creator].some((text) => text.toLocaleLowerCase().includes(needle)),
  );
}
