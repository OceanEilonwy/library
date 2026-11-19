// Reactive downloads: what is saved, what is in progress, and the actions.

import { createSignal } from 'solid-js';
import type { MediaFile } from '../api';
import type { DownloadRecord } from '../db';
import { DownloadError, type DownloadStore } from '../downloads';
import { plural } from '../format';

export type DownloadState = 'none' | 'queued' | 'downloading' | 'done';
export type Progress = { file: MediaFile; received: number; total: number };

export function createDownloads(store: DownloadStore, notify: (message: string) => void) {
  const [records, setRecords] = createSignal<DownloadRecord[]>([]);
  const [ids, setIds] = createSignal<ReadonlySet<number>>(new Set());
  const [queued, setQueued] = createSignal<ReadonlySet<number>>(new Set());
  const [progress, setProgress] = createSignal<ReadonlyMap<number, Progress>>(new Map());
  const pending: MediaFile[] = [];
  /** Ids queued or downloading, kept outside signals so same-tick calls see each other. */
  const inFlight = new Set<number>();
  let running = false;

  async function refresh(): Promise<void> {
    const list = await store.list();
    setRecords(list);
    setIds(new Set(list.map((record) => record.id)));
  }

  function setQueuedIds(update: (set: Set<number>) => void) {
    setQueued((current) => {
      const next = new Set(current);
      update(next);
      return next;
    });
  }

  function setFileProgress(id: number, value: Progress | null) {
    setProgress((current) => {
      const next = new Map(current);
      if (value) next.set(id, value);
      else next.delete(id);
      return next;
    });
  }

  async function work(): Promise<void> {
    if (running) return;
    running = true;
    let done = 0;
    try {
      for (let file = pending.shift(); file; file = pending.shift()) {
        const current = file;
        try {
          setFileProgress(current.id, { file: current, received: 0, total: current.sizeBytes });
          await store.download(current, {
            onProgress: (received, total) => setFileProgress(current.id, { file: current, received, total }),
          });
          done += 1;
          await refresh();
        } catch (error) {
          notify(error instanceof DownloadError ? error.message : `Couldn't download “${current.title}”.`);
        } finally {
          setFileProgress(current.id, null);
          inFlight.delete(current.id);
          setQueuedIds((set) => set.delete(current.id));
        }
      }
    } finally {
      running = false;
    }
    if (done > 1) notify(`Downloaded ${plural(done, 'file')}.`);
  }

  const controller = {
    records,
    ids,
    progress,
    /** True while anything is queued or downloading. */
    active: () => queued().size > 0,
    isDownloaded: (id: number) => ids().has(id),

    state(id: number): DownloadState {
      if (ids().has(id)) return 'done';
      if (progress().has(id)) return 'downloading';
      if (queued().has(id)) return 'queued';
      return 'none';
    },

    /** Fraction done (0–1) for a file being downloaded, or null. */
    fraction(id: number): number | null {
      const value = progress().get(id);
      if (!value) return null;
      return value.total > 0 ? Math.min(1, value.received / value.total) : 0;
    },

    refresh,

    /** Queues files that aren't already saved or on their way. */
    download(files: readonly MediaFile[]): void {
      const saved = ids();
      const fresh: MediaFile[] = [];
      for (const file of files) {
        if (saved.has(file.id) || inFlight.has(file.id)) continue;
        inFlight.add(file.id);
        fresh.push(file);
      }
      if (fresh.length === 0) return;
      pending.push(...fresh);
      setQueuedIds((set) => {
        for (const file of fresh) set.add(file.id);
      });
      void work();
    },

    async remove(id: number): Promise<void> {
      await store.remove(id);
      await refresh();
    },

    async removeItem(itemId: number): Promise<void> {
      await store.removeItem(itemId);
      await refresh();
    },

    async clearAll(): Promise<void> {
      const dropped = pending.splice(0).map((file) => file.id);
      for (const id of dropped) inFlight.delete(id);
      setQueuedIds((set) => {
        for (const id of dropped) set.delete(id);
      });
      await store.clearAll();
      await refresh();
    },

    /** Drops downloads the server no longer allows. Quietly does nothing offline. */
    async purge(check: (ids: number[]) => Promise<{ allowed: number[] }>): Promise<void> {
      try {
        const removed = await store.purge(check);
        if (removed > 0) {
          await refresh();
          notify(`Removed ${plural(removed, 'download')} you can no longer open.`);
        }
      } catch {
        // Offline or signed out: try again next time.
      }
    },

    clearOtherCaches: () => store.clearOtherCaches(),
    estimate: () => store.estimate(),
    store,
  };
  return controller;
}

export type Downloads = ReturnType<typeof createDownloads>;
