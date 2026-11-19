// Typed client for the Library's JSON API (see docs/player.md, "JSON API").
//
// Every call resolves with the decoded body or rejects with an `ApiError`
// whose `kind` says what went wrong in terms the UI can act on: a 401 means
// the session has ended, `offline` means the network could not be reached.

export type Access = 'child' | 'adult';
export type Account = { id: number; username: string; email: string; access: Access };
/** The answer to registering: the account waits for a librarian to approve it (no session yet). */
export type RegistrationPending = { status: 'pending'; message: string };

export type MediaKind = 'audio' | 'video' | 'document';
export type Subtitle = { id: number; language: string; label: string; url: string };
export type MediaFile = {
  id: number;
  itemId: number;
  kind: MediaKind;
  title: string;
  disc: number;
  track: number | null;
  mime: string;
  sizeBytes: number;
  durationMs: number | null;
  streamUrl: string;
  subtitles: Subtitle[];
  itemTitle: string;
  creator: string;
  year: number | null;
  cover: string | null;
};
export type ItemSummary = {
  id: number;
  title: string;
  creator: string;
  year: number | null;
  type: string;
  typeSlug: string;
  restricted: boolean;
  cover: string | null;
  kinds: MediaKind[];
  fileCount: number;
  totalDurationMs: number | null;
};
export type ItemDetail = ItemSummary & { description: string; media: MediaFile[] };
export type PlaylistSummary = {
  id: number;
  name: string;
  owner: string;
  mine: boolean;
  entryCount: number;
  totalDurationMs: number | null;
  covers: string[];
  updatedAt: number;
};
export type Playlist = PlaylistSummary & { entries: MediaFile[] };

export type ItemQuery = {
  q?: string;
  kind?: MediaKind;
  type?: string;
  sort?: 'newest' | 'title';
  offset?: number;
  limit?: number;
};
export type ItemPage = { items: ItemSummary[]; total: number };
/** `GET /api/info`: the library's title, the server version and where its source code is. */
export type ServerInfo = { name: string; version: string; sourceUrl: string };

/** What went wrong, in terms the interface reacts to. */
export type ApiErrorKind =
  | 'offline' // the request never reached the server
  | 'unauthorized' // 401: signed out or session ended
  | 'forbidden' // 403
  | 'not-found' // 404
  | 'validation' // 422, with per-field messages
  | 'rate-limited' // 429
  | 'server'; // anything else

/** An error from the API, with the server's plain-English message. */
export class ApiError extends Error {
  readonly kind: ApiErrorKind;
  readonly status: number;
  readonly fields: Record<string, string>;

  constructor(kind: ApiErrorKind, status: number, message: string, fields: Record<string, string> = {}) {
    super(message);
    this.name = 'ApiError';
    this.kind = kind;
    this.status = status;
    this.fields = fields;
  }
}

/** Maps an HTTP status to the error kind the UI understands. */
export function errorKindForStatus(status: number): ApiErrorKind {
  switch (status) {
    case 401:
      return 'unauthorized';
    case 403:
      return 'forbidden';
    case 404:
      return 'not-found';
    case 422:
      return 'validation';
    case 429:
      return 'rate-limited';
    default:
      return 'server';
  }
}

const fallbackMessages: Record<ApiErrorKind, string> = {
  offline: "You're offline. Check your connection and try again.",
  unauthorized: 'Please sign in to carry on.',
  forbidden: "You can't do that.",
  'not-found': "That isn't here any more.",
  validation: 'Some details need another look.',
  'rate-limited': 'Too many tries. Wait a minute and try again.',
  server: 'Something went wrong on the server. Try again in a moment.',
};

export function isApiError(error: unknown): error is ApiError {
  return error instanceof ApiError;
}

/** A friendly message for any thrown value. */
export function errorMessage(error: unknown): string {
  if (isApiError(error)) return error.message;
  return fallbackMessages.server;
}

type Fetch = typeof fetch;

export type ApiClientOptions = {
  fetch?: Fetch;
  /** Called when a request that needs an account gets a 401. */
  onUnauthorized?: () => void;
  /** Called with `true` when a request fails to reach the network, `false` when one succeeds. */
  onNetwork?: (offline: boolean) => void;
};

async function errorFromResponse(response: Response): Promise<ApiError> {
  const kind = errorKindForStatus(response.status);
  let message = fallbackMessages[kind];
  let fields: Record<string, string> = {};
  try {
    const body: unknown = await response.json();
    if (body && typeof body === 'object') {
      const record = body as { error?: unknown; fields?: unknown };
      if (typeof record.error === 'string' && record.error) message = record.error;
      if (record.fields && typeof record.fields === 'object') {
        fields = Object.fromEntries(
          Object.entries(record.fields as Record<string, unknown>).filter(
            (entry): entry is [string, string] => typeof entry[1] === 'string',
          ),
        );
      }
    }
  } catch {
    // Not JSON: keep the fallback message.
  }
  return new ApiError(kind, response.status, message, fields);
}

function queryString(params: Record<string, string | number | undefined>): string {
  const search = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined && value !== '') search.set(key, String(value));
  }
  const text = search.toString();
  return text ? `?${text}` : '';
}

/** Creates a client. The app uses one shared instance (`api`); tests make their own. */
export function createApiClient(options: ApiClientOptions = {}) {
  const doFetch: Fetch = options.fetch ?? ((input, init) => fetch(input, init));

  async function request<T>(method: string, path: string, body?: unknown, auth = true): Promise<T> {
    const init: RequestInit = {
      method,
      credentials: 'same-origin',
      headers:
        body === undefined
          ? { Accept: 'application/json' }
          : { Accept: 'application/json', 'Content-Type': 'application/json' },
    };
    if (body !== undefined) init.body = JSON.stringify(body);
    let response: Response;
    try {
      response = await doFetch(path, init);
    } catch {
      options.onNetwork?.(true);
      throw new ApiError('offline', 0, fallbackMessages.offline);
    }
    options.onNetwork?.(false);
    if (!response.ok) {
      const error = await errorFromResponse(response);
      if (error.kind === 'unauthorized' && auth) options.onUnauthorized?.();
      throw error;
    }
    if (response.status === 204) return undefined as T;
    const text = await response.text();
    return (text ? JSON.parse(text) : undefined) as T;
  }

  return {
    register: (input: { username: string; email: string; password: string }) =>
      request<RegistrationPending>('POST', '/api/account/register', input, false),
    login: (input: { login: string; password: string }) => request<Account>('POST', '/api/account/login', input, false),
    logout: () => request<void>('POST', '/api/account/logout', undefined, false),
    me: () => request<Account>('GET', '/api/account/me', undefined, false),
    info: () => request<ServerInfo>('GET', '/api/info', undefined, false),

    items: (query: ItemQuery = {}) =>
      request<ItemPage>(
        'GET',
        `/api/items${queryString({
          q: query.q,
          kind: query.kind,
          type: query.type,
          sort: query.sort === 'title' ? 'title' : undefined,
          offset: query.offset,
          limit: query.limit,
        })}`,
      ),
    item: (id: number) => request<ItemDetail>('GET', `/api/items/${id}`),
    media: (id: number) => request<MediaFile>('GET', `/api/media/${id}`),
    checkMedia: (ids: number[]) => request<{ allowed: number[] }>('POST', '/api/media/check', { ids }),

    playlists: (mine = false) => request<PlaylistSummary[]>('GET', `/api/playlists${mine ? '?mine=1' : ''}`),
    playlist: (id: number) => request<Playlist>('GET', `/api/playlists/${id}`),
    createPlaylist: (name: string) => request<Playlist>('POST', '/api/playlists', { name }),
    renamePlaylist: (id: number, name: string) => request<Playlist>('PATCH', `/api/playlists/${id}`, { name }),
    deletePlaylist: (id: number) => request<void>('DELETE', `/api/playlists/${id}`),
    addToPlaylist: (id: number, mediaIds: number[]) =>
      request<Playlist>('POST', `/api/playlists/${id}/entries`, { mediaIds }),
    setPlaylistEntries: (id: number, mediaIds: number[]) =>
      request<Playlist>('PUT', `/api/playlists/${id}/entries`, { mediaIds }),

    favourites: () => request<MediaFile[]>('GET', '/api/favourites'),
    addFavourite: (mediaId: number) => request<void>('PUT', `/api/favourites/${mediaId}`),
    removeFavourite: (mediaId: number) => request<void>('DELETE', `/api/favourites/${mediaId}`),
  };
}

export type ApiClient = ReturnType<typeof createApiClient>;

/** Hooks the shared client calls; the session module fills them in at start-up. */
export const apiHooks: { onUnauthorized: () => void; onNetwork: (offline: boolean) => void } = {
  onUnauthorized: () => {},
  onNetwork: () => {},
};

/** The app's shared client. */
export const api: ApiClient = createApiClient({
  onUnauthorized: () => apiHooks.onUnauthorized(),
  onNetwork: (offline) => apiHooks.onNetwork(offline),
});
