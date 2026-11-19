// Reactive wrapper over the device's listen history.

import { createSignal } from 'solid-js';
import type { MediaFile } from '../api';
import type { HistoryStore } from '../history';

export function createHistory(store: HistoryStore) {
  /** Bumped on every change so screens re-read. */
  const [version, setVersion] = createSignal(0);
  const bump = () => setVersion((v) => v + 1);
  return {
    version,
    store,
    async record(file: MediaFile): Promise<void> {
      try {
        await store.record(file);
        bump();
      } catch {
        // Private browsing can refuse IndexedDB; history is a nicety.
      }
    },
    async clear(): Promise<void> {
      await store.clear();
      bump();
    },
    async remove(id: number): Promise<void> {
      await store.remove(id);
      bump();
    },
  };
}

export type History = ReturnType<typeof createHistory>;
