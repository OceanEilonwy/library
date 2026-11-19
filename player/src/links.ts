// URLs inside the Player (served under Vite's base, /player/).

import type { MediaFile } from './api';

/** "/player" — the base without its trailing slash. */
export const BASE = import.meta.env.BASE_URL.replace(/\/$/, '');

export const to = {
  home: () => `${BASE}/`,
  library: () => `${BASE}/library`,
  item: (id: number) => `${BASE}/item/${id}`,
  playlists: () => `${BASE}/playlists`,
  playlist: (id: number) => `${BASE}/playlist/${id}`,
  favourites: () => `${BASE}/favourites`,
  history: () => `${BASE}/history`,
  downloads: () => `${BASE}/downloads`,
  video: (id: number) => `${BASE}/video/${id}`,
  document: (id: number) => `${BASE}/document/${id}`,
  signIn: () => `${BASE}/account/sign-in`,
  register: () => `${BASE}/account/register`,
  settings: () => `${BASE}/settings`,
};

/** Where opening a non-audio file goes (audio plays in place instead). */
export function openPath(file: MediaFile): string | null {
  switch (file.kind) {
    case 'audio':
      return null;
    case 'video':
      return to.video(file.id);
    case 'document':
      return to.document(file.id);
  }
}
