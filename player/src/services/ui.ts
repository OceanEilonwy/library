// Little bits of shared interface state: toasts and the "Add to playlist" sheet.

import { createSignal } from 'solid-js';
import type { MediaFile } from '../api';

export type Toast = { id: number; message: string };

export function createUi(toastMs = 4000) {
  const [toasts, setToasts] = createSignal<Toast[]>([]);
  const [playlistSheet, setPlaylistSheet] = createSignal<MediaFile[] | null>(null);
  const [nowPlayingOpen, setNowPlayingOpen] = createSignal(false);
  let nextId = 1;

  return {
    toasts,
    toast(message: string): void {
      const id = nextId;
      nextId += 1;
      setToasts((list) => [...list.slice(-2), { id, message }]);
      setTimeout(() => setToasts((list) => list.filter((toast) => toast.id !== id)), toastMs);
    },
    dismissToast(id: number): void {
      setToasts((list) => list.filter((toast) => toast.id !== id));
    },
    playlistSheet,
    openPlaylistSheet(files: MediaFile[]): void {
      if (files.length > 0) setPlaylistSheet(files);
    },
    closePlaylistSheet(): void {
      setPlaylistSheet(null);
    },
    nowPlayingOpen,
    setNowPlayingOpen,
  };
}

export type Ui = ReturnType<typeof createUi>;
