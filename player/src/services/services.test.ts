import { flush } from 'solid-js';
import { createApiClient } from '../api';
import { alice, mediaFile } from '../test/fixtures';
import { jsonResponse, makeServices } from '../test/services';
import { FakeAudio } from '../test/fakeAudio';
import { createSession } from './session';
import { createFavourites } from './favourites';
import { createPlayer } from './player';
import { createUpdates, type RegisterSW } from './updates';
import { isIos } from './install';
import { withMemory, createMemory } from './memory';
import { openPlayerDb } from '../db';
import { ApiError } from '../api';

function apiWith(respond: (url: string, init?: RequestInit) => Response) {
  return createApiClient({
    fetch: (async (input: RequestInfo | URL, init?: RequestInit) => respond(String(input), init)) as typeof fetch,
  });
}

describe('session', () => {
  test('starts signed in when the server knows us, and remembers the account', async () => {
    const saved: unknown[] = [];
    const session = createSession(
      apiWith(() => jsonResponse(200, alice)),
      { load: async () => null, save: async (a) => void saved.push(a) },
    );
    await session.start();
    flush();
    expect(session.status()).toBe('signed-in');
    expect(saved).toEqual([alice]);
  });

  test('starts signed out on a 401 and forgets the account', async () => {
    const saved: unknown[] = [];
    const session = createSession(
      apiWith(() => jsonResponse(401, { error: 'no' })),
      { load: async () => alice, save: async (a) => void saved.push(a) },
    );
    await session.start();
    flush();
    expect(session.status()).toBe('signed-out');
    expect(session.expired()).toBe(false);
    expect(saved).toEqual([null]);
  });

  test('offline, opens with the remembered account so downloads still play', async () => {
    const session = createSession(
      createApiClient({
        fetch: (async () => {
          throw new TypeError('offline');
        }) as typeof fetch,
      }),
      { load: async () => alice, save: async () => {} },
    );
    await session.start();
    flush();
    expect(session.status()).toBe('signed-in');
    expect(session.fromMemory()).toBe(true);
  });

  test('offline with nothing remembered asks to sign in', async () => {
    const session = createSession(
      createApiClient({
        fetch: (async () => {
          throw new TypeError('offline');
        }) as typeof fetch,
      }),
      { load: async () => null, save: async () => {} },
    );
    await session.start();
    flush();
    expect(session.status()).toBe('signed-out');
  });

  test('a 401 elsewhere ends the session and says so', async () => {
    const services = makeServices((url) =>
      url === '/api/account/me' ? jsonResponse(200, alice) : jsonResponse(401, { error: 'Please sign in.' }),
    );
    await services.session.start();
    flush();
    await expect(services.api.favourites()).rejects.toBeInstanceOf(ApiError);
    flush();
    expect(services.session.status()).toBe('signed-out');
    expect(services.session.expired()).toBe(true);
  });

  test('signing in tells listeners; signing out clears the account', async () => {
    const services = makeServices((url) =>
      url === '/api/account/login' ? jsonResponse(200, alice) : jsonResponse(204),
    );
    const listener = vi.fn();
    services.session.onSignedIn(listener);
    await services.session.signIn('alice', 'pw');
    expect(listener).toHaveBeenCalledWith(alice);
    await services.session.signOut();
    flush();
    expect(services.session.account()).toBeNull();
    expect(services.session.status()).toBe('signed-out');
  });
});

describe('registration', () => {
  test('asking for an account does not sign in or tell listeners', async () => {
    const services = makeServices((url) =>
      url === '/api/account/register'
        ? jsonResponse(202, { status: 'pending', message: 'Waiting for a librarian.' })
        : jsonResponse(401, { error: 'Please sign in.' }),
    );
    await services.session.start();
    const listener = vi.fn();
    services.session.onSignedIn(listener);
    const result = await services.session.register('newbie', 'n@example.org', 'password123');
    expect(result).toEqual({ status: 'pending', message: 'Waiting for a librarian.' });
    expect(listener).not.toHaveBeenCalled();
    expect(services.session.status()).toBe('signed-out');
    expect(services.session.account()).toBeNull();
  });
});

describe('favourites', () => {
  test('toggles straight away and puts it back if the server refuses', async () => {
    const notify = vi.fn();
    let fail = false;
    const favourites = createFavourites(
      apiWith(() => (fail ? jsonResponse(404, { error: "That isn't here any more." }) : jsonResponse(204))),
      notify,
    );
    const file = mediaFile(3);
    const pending = favourites.toggle(file);
    flush();
    expect(favourites.has(3)).toBe(true);
    await pending;
    fail = true;
    await favourites.toggle(file);
    flush();
    expect(favourites.has(3)).toBe(true);
    expect(notify).toHaveBeenCalledWith("That isn't here any more.");
  });

  test('loads from the server, and from the remembered copy when offline', async () => {
    const db = openPlayerDb('favourites-memory');
    const memory = createMemory(() => db);
    let offline = false;
    const api = createApiClient({
      fetch: (async () => {
        if (offline) throw new TypeError('offline');
        return jsonResponse(200, [mediaFile(1), mediaFile(2)]);
      }) as typeof fetch,
    });
    const first = createFavourites(api, vi.fn(), memory);
    await first.load();
    await new Promise((resolve) => setTimeout(resolve, 20));
    offline = true;
    const second = createFavourites(api, vi.fn(), memory);
    await second.load();
    flush();
    expect(second.files().map((file) => file.id)).toEqual([1, 2]);
  });
});

test('withMemory rethrows when offline and nothing is remembered', async () => {
  const memory = createMemory(() => openPlayerDb('memory-empty'));
  const offlineError = new ApiError('offline', 0, 'offline');
  await expect(
    withMemory(
      memory,
      'x',
      () => Promise.reject(offlineError),
      () => true,
    ),
  ).rejects.toBe(offlineError);
  const serverError = new ApiError('server', 500, 'boom');
  await expect(
    withMemory(
      memory,
      'x',
      () => Promise.reject(serverError),
      () => false,
    ),
  ).rejects.toBe(serverError);
});

describe('downloads controller', () => {
  test('shows progress, then the saved file, and announces batches', async () => {
    const services = makeServices(() => new Response(new Uint8Array(1000), { headers: { 'Content-Length': '1000' } }));
    services.downloads.download([mediaFile(1), mediaFile(2), mediaFile(1)]);
    flush();
    expect(services.downloads.state(1)).not.toBe('none');
    expect(services.downloads.active()).toBe(true);
    await vi.waitFor(() => expect(services.downloads.ids().size).toBe(2));
    await vi.waitFor(() => expect(services.downloads.active()).toBe(false));
    expect(services.downloads.state(1)).toBe('done');
    expect(services.ui.toasts().map((toast) => toast.message)).toContain('Downloaded 2 files.');
  });

  test('a failed download explains itself and is not marked saved', async () => {
    const services = makeServices(() => jsonResponse(500));
    services.downloads.download([mediaFile(4, { title: 'Teardrop' })]);
    flush();
    await vi.waitFor(() => expect(services.downloads.active()).toBe(false));
    expect(services.downloads.state(4)).toBe('none');
    expect(services.ui.toasts()[0]?.message).toMatch(/Teardrop/);
  });

  test('purging tells the listener what went', async () => {
    const services = makeServices((url) =>
      url === '/api/media/check' ? jsonResponse(200, { allowed: [] }) : new Response('xx'),
    );
    await services.downloads.store.download(mediaFile(1));
    await services.downloads.purge((ids) => services.api.checkMedia(ids));
    flush();
    expect(services.downloads.records()).toHaveLength(0);
    expect(services.ui.toasts()[0]?.message).toBe('Removed 1 download you can no longer open.');
  });
});

describe('player service', () => {
  test('queues only audio and starts at the chosen file', () => {
    const player = createPlayer({ element: new FakeAudio() });
    const film = mediaFile(2, { kind: 'video' });
    player.playFiles([mediaFile(1), film, mediaFile(3)], mediaFile(3));
    expect(player.engine.state.queue.entries.map((entry) => entry.track.id)).toEqual([1, 3]);
    expect(player.engine.state.current?.id).toBe(3);
  });

  test('saves the queue and restores it after a reload', async () => {
    vi.useFakeTimers();
    const store = new Map<string, string>();
    const storage = {
      getItem: (k: string) => store.get(k) ?? null,
      setItem: (k: string, v: string) => void store.set(k, v),
      removeItem: (k: string) => void store.delete(k),
    };
    const first = createPlayer({ element: new FakeAudio(), storage });
    first.playFiles([mediaFile(1), mediaFile(2)], 1);
    vi.advanceTimersByTime(1500);
    vi.useRealTimers();
    expect(store.get('player.queue')).toBeDefined();
    const audio = new FakeAudio();
    const second = createPlayer({ element: audio, storage });
    second.restore();
    expect(second.engine.state.current?.id).toBe(2);
    expect(audio.paused).toBe(true);
  });

  test('a corrupt saved queue is discarded', () => {
    const store = new Map([['player.queue', '{nope']]);
    const storage = {
      getItem: (k: string) => store.get(k) ?? null,
      setItem: vi.fn(),
      removeItem: (k: string) => void store.delete(k),
    };
    createPlayer({ element: new FakeAudio(), storage }).restore();
    expect(store.has('player.queue')).toBe(false);
  });

  test('keeps the platform controls informed', () => {
    const session = {
      metadata: null as MediaMetadata | null,
      playbackState: 'none' as MediaSessionPlaybackState,
      handlers: new Map<string, unknown>(),
      setActionHandler(action: MediaSessionAction, handler: MediaSessionActionHandler | null) {
        this.handlers.set(action, handler);
      },
      setPositionState: vi.fn(),
    };
    const player = createPlayer({ element: new FakeAudio(), mediaSession: session });
    player.playFiles([mediaFile(1, { title: 'So What' })]);
    expect((session.metadata as unknown as MediaMetadataInit | null)?.title).toBe('So What');
    expect(session.playbackState).toBe('playing');
    (session.handlers.get('pause') as () => void)();
    expect(session.playbackState).toBe('paused');
  });

  test('a film takes the platform controls and gives them back', () => {
    const audio = new FakeAudio();
    const player = createPlayer({ element: audio });
    player.playFiles([mediaFile(1)]);
    const release = player.claimSessionForVideo();
    expect(audio.paused).toBe(true);
    player.setVideoPlaying(true);
    flush();
    expect(player.busy()).toBe(true);
    release();
    flush();
    expect(player.busy()).toBe(false);
  });
});

describe('updates', () => {
  function setup(busy = false) {
    let needRefresh: (() => void) | undefined;
    const apply = vi.fn(async () => {});
    const register: RegisterSW = (options) => {
      needRefresh = options.onNeedRefresh;
      options.onRegisteredSW?.('/player/sw.js', undefined);
      return apply;
    };
    Object.defineProperty(navigator, 'serviceWorker', { value: {}, configurable: true });
    const updates = createUpdates({ busy: () => busy, downloading: () => false });
    updates.register(register);
    return { updates, apply, ready: () => needRefresh?.() };
  }

  test('applies a waiting version on navigation when idle, without a prompt', () => {
    const { updates, apply, ready } = setup();
    flush();
    expect(updates.status()).toBe('current');
    expect(updates.maybeActivate('navigation')).toBe(false);
    ready();
    flush();
    expect(updates.status()).toBe('ready');
    expect(updates.maybeActivate('navigation')).toBe(true);
    expect(apply).toHaveBeenCalledWith(true);
    expect(updates.maybeActivate('hidden')).toBe(false);
    expect(apply).toHaveBeenCalledOnce();
  });

  test('waits while something is playing', () => {
    const { updates, apply, ready } = setup(true);
    ready();
    flush();
    expect(updates.maybeActivate('hidden')).toBe(false);
    expect(apply).not.toHaveBeenCalled();
  });
});

test('recognises iPhones and iPads (including desktop-mode iPads)', () => {
  expect(isIos('Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X)')).toBe(true);
  expect(isIos('Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)', 5)).toBe(true);
  expect(isIos('Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)', 0)).toBe(false);
  expect(isIos('Mozilla/5.0 (Linux; Android 14)')).toBe(false);
});
