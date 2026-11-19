// @vitest-environment node
import { ApiError, createApiClient, errorKindForStatus, errorMessage } from './api';

function json(status: number, body: unknown) {
  return new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } });
}

function client(respond: (url: string, init?: RequestInit) => Response | Promise<Response>) {
  const fetch = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => respond(String(input), init));
  const onUnauthorized = vi.fn();
  const onNetwork = vi.fn();
  const api = createApiClient({ fetch: fetch as unknown as typeof globalThis.fetch, onUnauthorized, onNetwork });
  return { api, fetch, onUnauthorized, onNetwork };
}

test('maps statuses to kinds', () => {
  expect(errorKindForStatus(401)).toBe('unauthorized');
  expect(errorKindForStatus(403)).toBe('forbidden');
  expect(errorKindForStatus(404)).toBe('not-found');
  expect(errorKindForStatus(422)).toBe('validation');
  expect(errorKindForStatus(429)).toBe('rate-limited');
  expect(errorKindForStatus(500)).toBe('server');
});

test('decodes JSON bodies', async () => {
  const { api, fetch } = client(() => json(200, { items: [], total: 0 }));
  await expect(api.items({ q: 'blue', kind: 'audio', sort: 'title', limit: 20 })).resolves.toEqual({
    items: [],
    total: 0,
  });
  expect(fetch.mock.calls[0]?.[0]).toBe('/api/items?q=blue&kind=audio&sort=title&limit=20');
});

test('sends JSON bodies with the right method', async () => {
  const { api, fetch } = client(() => json(200, { id: 1 }));
  await api.setPlaylistEntries(4, [3, 2]);
  const [url, init] = fetch.mock.calls[0] ?? [];
  expect(url).toBe('/api/playlists/4/entries');
  expect(init?.method).toBe('PUT');
  expect(init?.body).toBe(JSON.stringify({ mediaIds: [3, 2] }));
  expect(init?.credentials).toBe('same-origin');
});

test('204 resolves with nothing', async () => {
  const { api } = client(() => new Response(null, { status: 204 }));
  await expect(api.addFavourite(3)).resolves.toBeUndefined();
});

test('a 401 on a media endpoint signals the session has ended', async () => {
  const { api, onUnauthorized } = client(() => json(401, { error: 'Please sign in.' }));
  const error = await api.favourites().catch((e: unknown) => e);
  expect(error).toBeInstanceOf(ApiError);
  expect((error as ApiError).kind).toBe('unauthorized');
  expect((error as ApiError).message).toBe('Please sign in.');
  expect(onUnauthorized).toHaveBeenCalledOnce();
});

test('a 401 from login or me is just an answer, not an expired session', async () => {
  const { api, onUnauthorized } = client(() => json(401, { error: 'Wrong username or password.' }));
  await expect(api.login({ login: 'a', password: 'b' })).rejects.toMatchObject({
    kind: 'unauthorized',
    message: 'Wrong username or password.',
  });
  await expect(api.me()).rejects.toMatchObject({ kind: 'unauthorized' });
  expect(onUnauthorized).not.toHaveBeenCalled();
});

test('registering answers that the account waits for a librarian', async () => {
  const pending = { status: 'pending', message: 'A librarian needs to approve your account.' };
  const { api, fetch } = client(() => json(202, pending));
  await expect(api.register({ username: 'newbie', email: 'n@example.org', password: 'password123' })).resolves.toEqual(
    pending,
  );
  expect(fetch.mock.calls[0]?.[0]).toBe('/api/account/register');
});

test('a 403 from login (waiting for approval) is an answer, not an expired session', async () => {
  const { api, onUnauthorized } = client(() =>
    json(403, { error: 'Your account is waiting for a librarian to approve it.' }),
  );
  await expect(api.login({ login: 'newbie', password: 'password123' })).rejects.toMatchObject({
    kind: 'forbidden',
    status: 403,
    message: 'Your account is waiting for a librarian to approve it.',
  });
  expect(onUnauthorized).not.toHaveBeenCalled();
});

test('validation errors carry field messages', async () => {
  const { api } = client(() => json(422, { error: 'Check the form.', fields: { username: 'Already taken', bad: 3 } }));
  await expect(api.register({ username: 'al', email: 'x', password: 'y' })).rejects.toMatchObject({
    kind: 'validation',
    fields: { username: 'Already taken' },
  });
});

test('non-JSON errors fall back to a friendly message', async () => {
  const { api } = client(() => new Response('<html>oops</html>', { status: 502 }));
  await expect(api.item(1)).rejects.toMatchObject({ kind: 'server', message: expect.stringMatching(/Try again/) });
});

test('network failures are reported as offline', async () => {
  const { api, onNetwork } = client(() => {
    throw new TypeError('Failed to fetch');
  });
  await expect(api.me()).rejects.toMatchObject({ kind: 'offline', status: 0 });
  expect(onNetwork).toHaveBeenCalledWith(true);
});

test('a successful request reports being online', async () => {
  const { api, onNetwork } = client(() => json(200, []));
  await api.playlists(true);
  expect(onNetwork).toHaveBeenCalledWith(false);
});

test('errorMessage handles anything thrown', () => {
  expect(errorMessage(new ApiError('forbidden', 403, 'Only the owner can do that.'))).toBe(
    'Only the owner can do that.',
  );
  expect(errorMessage(new Error('boom'))).toMatch(/Something went wrong/);
});
