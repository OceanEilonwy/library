// Favourites (hearts), stored on the server per account.

import { createSignal } from 'solid-js';
import { errorMessage, isApiError, type ApiClient, type MediaFile } from '../api';
import { withMemory, type Memory } from './memory';

export function createFavourites(api: ApiClient, notify: (message: string) => void, memory?: Memory) {
  const [files, setFiles] = createSignal<MediaFile[]>([]);
  const [ids, setIds] = createSignal<ReadonlySet<number>>(new Set());
  const [loaded, setLoaded] = createSignal(false);

  function setList(list: MediaFile[]) {
    setFiles(list);
    setIds(new Set(list.map((file) => file.id)));
  }

  return {
    files,
    ids,
    loaded,
    has: (id: number) => ids().has(id),

    async load(): Promise<void> {
      try {
        const load = () => api.favourites();
        const offline = (error: unknown) => isApiError(error) && error.kind === 'offline';
        setList(memory ? await withMemory(memory, 'favourites', load, offline) : await load());
        setLoaded(true);
      } catch {
        // Offline or signed out: keep what we had.
      }
    },

    /** Hearts or un-hearts `file` straight away, putting it back if the server says no. */
    async toggle(file: MediaFile): Promise<void> {
      const before = files();
      const wasFavourite = ids().has(file.id);
      const next = wasFavourite ? before.filter((f) => f.id !== file.id) : [file, ...before];
      setList(next);
      try {
        if (wasFavourite) await api.removeFavourite(file.id);
        else await api.addFavourite(file.id);
        void memory?.save('favourites', next);
      } catch (error) {
        setList(before);
        notify(errorMessage(error));
      }
    },

    reset(): void {
      setList([]);
      setLoaded(false);
    },
  };
}

export type Favourites = ReturnType<typeof createFavourites>;
