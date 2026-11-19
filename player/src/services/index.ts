// Builds the app's services and shares them through context.

import { createContext } from 'solid-js';
import { api as sharedApi, apiHooks, type Account, type ApiClient } from '../api';
import { kvGet, kvSet, playerDb } from '../db';
import { createDownloadStore } from '../downloads';
import { createHistoryStore } from '../history';
import { browserMediaSession } from '../player/mediaSession';
import { createDownloads, type Downloads } from './downloads';
import { createFavourites, type Favourites } from './favourites';
import { createHistory, type History } from './history';
import { createInstall, type Install } from './install';
import { createMemory, type Memory } from './memory';
import { createNetwork, type Network } from './network';
import { createPlayer, type Player } from './player';
import { createSession, type Session } from './session';
import { createUi, type Ui } from './ui';
import { createUpdates, type Updates } from './updates';
import type { MediaElementLike } from '../player/engine';

export type Services = {
  api: ApiClient;
  session: Session;
  network: Network;
  ui: Ui;
  favourites: Favourites;
  downloads: Downloads;
  history: History;
  player: Player;
  updates: Updates;
  install: Install;
  memory: Memory;
};

export const ServicesContext = createContext<Services>();

function safeLocalStorage(): Storage | null {
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

/** Wires the real browser services together. */
export function createBrowserServices(audio: MediaElementLike): Services {
  const api = sharedApi;
  const ui = createUi();
  const network = createNetwork();
  const session = createSession(api, {
    async load() {
      try {
        return (await kvGet<Account>(await playerDb(), 'account')) ?? null;
      } catch {
        return null;
      }
    },
    async save(account) {
      try {
        await kvSet(await playerDb(), 'account', account);
      } catch {
        // Private browsing: offline sign-in just won't be remembered.
      }
    },
  });
  apiHooks.onUnauthorized = () => session.expire();
  apiHooks.onNetwork = (offline) => network.report(offline);

  const memory = createMemory(playerDb);
  const favourites = createFavourites(api, ui.toast, memory);
  const downloads = createDownloads(
    createDownloadStore({
      db: playerDb,
      caches: 'caches' in window ? window.caches : null,
      fetch: (input, init) => fetch(input, init),
      storage: navigator.storage,
    }),
    ui.toast,
  );
  const history = createHistory(createHistoryStore(playerDb));
  const player = createPlayer({
    element: audio,
    onListen: (file) => void history.record(file),
    mediaSession: browserMediaSession(),
    storage: safeLocalStorage(),
  });
  const updates = createUpdates({ busy: player.busy, downloading: downloads.active });
  const install = createInstall();

  session.onSignedIn(() => {
    void favourites.load();
    void downloads.purge((ids) => api.checkMedia(ids));
  });

  return { api, session, network, ui, favourites, downloads, history, player, updates, install, memory };
}
