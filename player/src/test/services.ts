// Real services wired to fakes, for component tests.

import { createApiClient, type ApiClient } from '../api';
import { openPlayerDb } from '../db';
import { createDownloadStore } from '../downloads';
import { createHistoryStore } from '../history';
import type { Services } from '../services';
import { createDownloads } from '../services/downloads';
import { createFavourites } from '../services/favourites';
import { createHistory } from '../services/history';
import { createInstall } from '../services/install';
import { createMemory } from '../services/memory';
import { createNetwork } from '../services/network';
import { createPlayer } from '../services/player';
import { createSession } from '../services/session';
import { createUi } from '../services/ui';
import { createUpdates } from '../services/updates';
import { FakeAudio } from './fakeAudio';
import { FakeCacheStorage } from './fakeCaches';

let counter = 0;

export type TestServices = Services & { audio: FakeAudio; caches: FakeCacheStorage; fetch: ReturnType<typeof vi.fn> };

export type Responder = (url: string, init?: RequestInit) => Response | Promise<Response>;

export const jsonResponse = (status: number, body?: unknown) =>
  body === undefined
    ? new Response(null, { status })
    : new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } });

export function makeServices(respond: Responder = () => jsonResponse(404, { error: 'Not found' })): TestServices {
  counter += 1;
  const fetch = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => respond(String(input), init));
  const typedFetch = fetch as unknown as typeof globalThis.fetch;
  const ui = createUi(60_000);
  const network = createNetwork(null);
  let session: Services['session'] | undefined;
  const api: ApiClient = createApiClient({
    fetch: typedFetch,
    onUnauthorized: () => session?.expire(),
    onNetwork: (offline) => network.report(offline),
  });
  const db = openPlayerDb(`services-test-${counter}`);
  const memory = createMemory(() => db);
  session = createSession(api, { load: async () => null, save: async () => {} });
  const caches = new FakeCacheStorage();
  const downloads = createDownloads(
    createDownloadStore({
      db: () => db,
      caches,
      fetch: typedFetch,
      storage: { estimate: async () => ({ usage: 5_000_000, quota: 1_000_000_000 }) },
    }),
    ui.toast,
  );
  const history = createHistory(createHistoryStore(() => db));
  const audio = new FakeAudio();
  const player = createPlayer({
    element: audio,
    onListen: (file) => void history.record(file),
    mediaSession: null,
    storage: null,
  });
  const favourites = createFavourites(api, ui.toast, memory);
  const updates = createUpdates({ busy: player.busy, downloading: downloads.active });
  const install = createInstall(null);
  return {
    api,
    session,
    network,
    ui,
    favourites,
    downloads,
    history,
    player,
    updates,
    install,
    memory,
    audio,
    caches,
    fetch,
  };
}
