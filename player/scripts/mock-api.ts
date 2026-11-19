// A pretend Library server for trying the Player without the Rust server:
// `npm run dev:mock`. It follows docs/player.md closely enough to click
// through every screen. Audio is generated tones; covers are drawn SVGs.
// Nothing here ships in the build.

import { readFileSync, existsSync } from 'node:fs';
import { extname, join, normalize } from 'node:path';
import type { IncomingMessage, ServerResponse } from 'node:http';
import type { Plugin } from 'vite';

type Kind = 'audio' | 'video' | 'document';
type File = {
  id: number;
  itemId: number;
  kind: Kind;
  title: string;
  disc: number;
  track: number | null;
  mime: string;
  durationMs: number | null;
  subtitles: { id: number; language: string; label: string }[];
};
type Item = {
  id: number;
  title: string;
  creator: string;
  year: number;
  type: string;
  typeSlug: string;
  restricted: boolean;
  colour: string;
  description: string;
};

const items: Item[] = [
  {
    id: 1,
    title: 'Blue Train',
    creator: 'John Coltrane',
    year: 1957,
    type: 'Music',
    typeSlug: 'music',
    restricted: false,
    colour: '#1f4e79',
    description: 'Hard bop, recorded at Van Gelder Studio.',
  },
  {
    id: 2,
    title: 'Kind of Blue',
    creator: 'Miles Davis',
    year: 1959,
    type: 'Music',
    typeSlug: 'music',
    restricted: false,
    colour: '#24476b',
    description: '',
  },
  {
    id: 3,
    title: 'The Köln Concert',
    creator: 'Keith Jarrett',
    year: 1975,
    type: 'Music',
    typeSlug: 'music',
    restricted: false,
    colour: '#d8d2c4',
    description: 'Solo piano, improvised.',
  },
  {
    id: 4,
    title: 'Spirited Away',
    creator: 'Hayao Miyazaki',
    year: 2001,
    type: 'Film',
    typeSlug: 'film',
    restricted: false,
    colour: '#5b8c5a',
    description: '',
  },
  {
    id: 5,
    title: 'Garden Birds of Britain',
    creator: 'RSPB',
    year: 2019,
    type: 'Book',
    typeSlug: 'book',
    restricted: false,
    colour: '#a4553b',
    description: '',
  },
  {
    id: 6,
    title: 'Mezzanine',
    creator: 'Massive Attack',
    year: 1998,
    type: 'Music',
    typeSlug: 'music',
    restricted: true,
    colour: '#222222',
    description: '',
  },
];

const files: File[] = [];
let nextId = 1;
const add = (
  itemId: number,
  kind: Kind,
  title: string,
  track: number | null,
  seconds: number | null,
  disc = 1,
  subtitles: File['subtitles'] = [],
) => {
  files.push({
    id: nextId++,
    itemId,
    kind,
    title,
    disc,
    track,
    mime: kind === 'audio' ? 'audio/wav' : kind === 'video' ? 'video/mp4' : 'application/pdf',
    durationMs: seconds ? seconds * 1000 : null,
    subtitles,
  });
};
['Blue Train', "Moment's Notice", 'Locomotion', 'I’m Old Fashioned', 'Lazy Bird'].forEach((t, i) =>
  add(1, 'audio', t, i + 1, 30 + i * 7),
);
['So What', 'Freddie Freeloader', 'Blue in Green', 'All Blues', 'Flamenco Sketches'].forEach((t, i) =>
  add(2, 'audio', t, i + 1, 25 + i * 5),
);
add(3, 'audio', 'Part I', 1, 40, 1);
add(3, 'audio', 'Part II a', 1, 35, 2);
add(3, 'audio', 'Part II b', 2, 33, 2);
add(4, 'video', 'Spirited Away', null, 7500, 1, [
  { id: 1, language: 'en', label: 'English' },
  { id: 2, language: 'fr', label: 'Français' },
]);
add(5, 'document', 'Field guide (PDF)', null, null);
['Angel', 'Risingson', 'Teardrop'].forEach((t, i) => add(6, 'audio', t, i + 1, 30));

type Account = { id: number; username: string; email: string; access: 'child' | 'adult'; password: string };
const accounts: Account[] = [
  { id: 1, username: 'demo', email: 'demo@example.org', access: 'adult', password: 'password' },
];
const sessions = new Map<string, number>();
const favourites = new Map<number, number[]>();
type PlaylistRow = { id: number; accountId: number; name: string; entries: number[]; updatedAt: number };
const playlists: PlaylistRow[] = [
  { id: 1, accountId: 99, name: 'Sunday morning', entries: [3, 8, 11, 12], updatedAt: 1_763_000_000 },
];
let nextPlaylist = 2;

const coverOf = (item: Item) => `/photos/mock-${item.id}.svg`;
const itemOf = (id: number) => items.find((item) => item.id === id);

function mediaJson(file: File) {
  const item = itemOf(file.itemId)!;
  return {
    id: file.id,
    itemId: file.itemId,
    kind: file.kind,
    title: file.title,
    disc: file.disc,
    track: file.track,
    mime: file.mime,
    sizeBytes: file.durationMs ? Math.round((file.durationMs / 1000) * 16000 * 2) + 44 : 50_000,
    durationMs: file.durationMs,
    streamUrl: `/api/media/${file.id}/stream`,
    subtitles: file.subtitles.map((s) => ({ ...s, url: `/api/subtitles/${s.id}` })),
    itemTitle: item.title,
    creator: item.creator,
    year: item.year,
    cover: coverOf(item),
  };
}

function canSee(account: Account, file: File) {
  return account.access === 'adult' || !itemOf(file.itemId)?.restricted;
}

function summary(item: Item) {
  const media = files.filter((f) => f.itemId === item.id);
  return {
    id: item.id,
    title: item.title,
    creator: item.creator,
    year: item.year,
    type: item.type,
    typeSlug: item.typeSlug,
    restricted: item.restricted,
    cover: coverOf(item),
    kinds: [...new Set(media.map((f) => f.kind))],
    fileCount: media.length,
    totalDurationMs: media.reduce((s, f) => s + (f.durationMs ?? 0), 0) || null,
  };
}

function playlistJson(row: PlaylistRow, viewer: Account, withEntries: boolean) {
  const visible = row.entries
    .map((id) => files.find((f) => f.id === id))
    .filter((f): f is File => !!f && canSee(viewer, f));
  const covers = [...new Set(visible.map((f) => coverOf(itemOf(f.itemId)!)))].slice(0, 4);
  const owner = accounts.find((a) => a.id === row.accountId)?.username ?? 'margaret';
  const base = {
    id: row.id,
    name: row.name,
    owner,
    mine: row.accountId === viewer.id,
    entryCount: visible.length,
    totalDurationMs: visible.reduce((s, f) => s + (f.durationMs ?? 0), 0) || null,
    covers,
    updatedAt: row.updatedAt,
  };
  return withEntries ? { ...base, entries: visible.map(mediaJson) } : base;
}

function tone(seconds: number, frequency: number): Buffer {
  const rate = 16000;
  const samples = Math.round(seconds * rate);
  const buffer = Buffer.alloc(44 + samples * 2);
  buffer.write('RIFF', 0);
  buffer.writeUInt32LE(36 + samples * 2, 4);
  buffer.write('WAVE', 8);
  buffer.write('fmt ', 12);
  buffer.writeUInt32LE(16, 16);
  buffer.writeUInt16LE(1, 20);
  buffer.writeUInt16LE(1, 22);
  buffer.writeUInt32LE(rate, 24);
  buffer.writeUInt32LE(rate * 2, 28);
  buffer.writeUInt16LE(2, 32);
  buffer.writeUInt16LE(16, 34);
  buffer.write('data', 36);
  buffer.writeUInt32LE(samples * 2, 40);
  for (let i = 0; i < samples; i += 1) {
    const t = i / rate;
    const envelope = Math.min(1, t * 4, (seconds - t) * 4);
    const value =
      Math.sin(2 * Math.PI * frequency * t) * 0.18 * envelope * (0.75 + 0.25 * Math.sin(2 * Math.PI * 0.5 * t));
    buffer.writeInt16LE(Math.round(value * 32767), 44 + i * 2);
  }
  return buffer;
}

function coverSvg(item: Item) {
  const initials = item.title
    .split(/\s+/)
    .map((w) => w[0])
    .join('')
    .slice(0, 3);
  const dark = item.colour !== '#d8d2c4';
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 400 400"><rect width="400" height="400" fill="${item.colour}"/><circle cx="300" cy="110" r="70" fill="${dark ? '#ffffff22' : '#00000018'}"/><text x="32" y="330" font-family="Georgia, serif" font-size="96" font-style="italic" fill="${dark ? '#f4efe2' : '#2a2a2a'}">${initials}</text><text x="34" y="370" font-family="sans-serif" font-size="20" fill="${dark ? '#f4efe2bb' : '#2a2a2aaa'}">${item.creator}</text></svg>`;
}

const json = (res: ServerResponse, status: number, body?: unknown) => {
  res.statusCode = status;
  if (body === undefined) return res.end();
  res.setHeader('Content-Type', 'application/json');
  res.end(JSON.stringify(body));
};

async function readBody(req: IncomingMessage): Promise<Record<string, unknown>> {
  const chunks: Buffer[] = [];
  for await (const chunk of req) chunks.push(chunk as Buffer);
  const text = Buffer.concat(chunks).toString('utf8');
  return text ? (JSON.parse(text) as Record<string, unknown>) : {};
}

function viewer(req: IncomingMessage): Account | undefined {
  const token = /library_account=([^;]+)/.exec(req.headers.cookie ?? '')?.[1];
  const id = token ? sessions.get(token) : undefined;
  return accounts.find((a) => a.id === id);
}

function signIn(res: ServerResponse, account: Account) {
  const token = Math.random().toString(36).slice(2);
  sessions.set(token, account.id);
  res.setHeader('Set-Cookie', `library_account=${token}; HttpOnly; SameSite=Lax; Path=/; Max-Age=31536000`);
  json(res, 200, { id: account.id, username: account.username, email: account.email, access: account.access });
}

async function handleApi(req: IncomingMessage, res: ServerResponse, url: URL) {
  const path = url.pathname;
  const method = req.method ?? 'GET';
  if (path === '/api/account/login' && method === 'POST') {
    const body = await readBody(req);
    const login = String(body.login ?? '').toLowerCase();
    const account = accounts.find((a) => a.username.toLowerCase() === login || a.email.toLowerCase() === login);
    if (!account || account.password !== body.password) return json(res, 401, { error: 'Wrong username or password.' });
    return signIn(res, account);
  }
  if (path === '/api/account/register' && method === 'POST') {
    const body = await readBody(req);
    const username = String(body.username ?? '');
    if (accounts.some((a) => a.username.toLowerCase() === username.toLowerCase())) {
      return json(res, 422, {
        error: 'Check the highlighted fields.',
        fields: { username: 'That username is taken.' },
      });
    }
    const account: Account = {
      id: accounts.length + 1,
      username,
      email: String(body.email),
      access: 'child',
      password: String(body.password),
    };
    accounts.push(account);
    signIn(res, account);
    res.statusCode = 201;
    return;
  }
  if (path === '/api/account/logout') {
    res.setHeader('Set-Cookie', 'library_account=; Path=/; Max-Age=0');
    return json(res, 204);
  }
  const me = viewer(req);
  if (!me) return json(res, 401, { error: 'Please sign in.' });
  if (path === '/api/account/me')
    return json(res, 200, { id: me.id, username: me.username, email: me.email, access: me.access });

  if (path === '/api/items') {
    const q = (url.searchParams.get('q') ?? '').toLowerCase();
    const kind = url.searchParams.get('kind');
    let list = items.filter((item) => files.some((f) => f.itemId === item.id && canSee(me, f)));
    if (q) list = list.filter((item) => `${item.title} ${item.creator}`.toLowerCase().includes(q));
    if (kind) list = list.filter((item) => files.some((f) => f.itemId === item.id && f.kind === kind));
    list =
      url.searchParams.get('sort') === 'title'
        ? [...list].sort((a, b) => a.title.localeCompare(b.title))
        : [...list].reverse();
    const offset = Number(url.searchParams.get('offset') ?? 0);
    const limit = Number(url.searchParams.get('limit') ?? 40);
    return json(res, 200, { items: list.slice(offset, offset + limit).map(summary), total: list.length });
  }
  let match = /^\/api\/items\/(\d+)$/.exec(path);
  if (match) {
    const item = itemOf(Number(match[1]));
    if (!item || (item.restricted && me.access !== 'adult')) return json(res, 404, { error: 'No such item.' });
    return json(res, 200, {
      ...summary(item),
      description: item.description,
      media: files.filter((f) => f.itemId === item.id).map(mediaJson),
    });
  }
  if (path === '/api/media/check') {
    const body = await readBody(req);
    const ids = (body.ids as number[]) ?? [];
    return json(res, 200, { allowed: ids.filter((id) => files.some((f) => f.id === id && canSee(me, f))) });
  }
  match = /^\/api\/media\/(\d+)(\/stream)?$/.exec(path);
  if (match) {
    const file = files.find((f) => f.id === Number(match![1]));
    if (!file || !canSee(me, file)) return json(res, 404, { error: 'No such file.' });
    if (!match[2]) return json(res, 200, mediaJson(file));
    if (file.kind === 'document') {
      res.setHeader('Content-Type', 'text/html; charset=utf-8');
      return res.end(
        '<!doctype html><body style="font-family:Georgia,serif;padding:3rem;background:#fff;color:#222"><h1>Garden Birds of Britain</h1><p>(The mock server shows this page in place of a PDF.)</p></body>',
      );
    }
    if (file.kind === 'video') return json(res, 404, { error: 'The mock server has no film to stream.' });
    const data = tone((file.durationMs ?? 30000) / 1000, 220 + (file.id % 7) * 55);
    const range = /bytes=(\d*)-(\d*)/.exec(String(req.headers.range ?? ''));
    res.setHeader('Accept-Ranges', 'bytes');
    res.setHeader('Content-Type', 'audio/wav');
    if (range) {
      const start = Number(range[1] || 0);
      const end = range[2] ? Number(range[2]) : data.length - 1;
      res.statusCode = 206;
      res.setHeader('Content-Range', `bytes ${start}-${end}/${data.length}`);
      res.setHeader('Content-Length', String(end - start + 1));
      return res.end(data.subarray(start, end + 1));
    }
    res.setHeader('Content-Length', String(data.length));
    return res.end(data);
  }
  if (/^\/api\/subtitles\/\d+$/.test(path)) {
    res.setHeader('Content-Type', 'text/vtt; charset=utf-8');
    return res.end('WEBVTT\n\n00:00.000 --> 00:05.000\nHello from the mock server.\n');
  }
  if (path === '/api/playlists' && method === 'GET') {
    const mineOnly = url.searchParams.get('mine') === '1';
    const list = playlists
      .filter((row) => (mineOnly ? row.accountId === me.id : true))
      .map((row) => playlistJson(row, me, false) as { entryCount: number; mine: boolean })
      .filter((p) => p.mine || p.entryCount > 0);
    return json(res, 200, list);
  }
  if (path === '/api/playlists' && method === 'POST') {
    const body = await readBody(req);
    const row = {
      id: nextPlaylist++,
      accountId: me.id,
      name: String(body.name),
      entries: [],
      updatedAt: Math.floor(Date.now() / 1000),
    };
    playlists.unshift(row);
    return json(res, 201, playlistJson(row, me, true));
  }
  match = /^\/api\/playlists\/(\d+)(\/entries)?$/.exec(path);
  if (match) {
    const row = playlists.find((p) => p.id === Number(match![1]));
    if (!row) return json(res, 404, { error: 'No such playlist.' });
    if (method === 'GET') return json(res, 200, playlistJson(row, me, true));
    if (row.accountId !== me.id) return json(res, 403, { error: 'Only the owner can change this playlist.' });
    const body = method === 'DELETE' ? {} : await readBody(req);
    if (method === 'DELETE') {
      playlists.splice(playlists.indexOf(row), 1);
      return json(res, 204);
    }
    if (method === 'PATCH') row.name = String(body.name);
    if (method === 'POST') row.entries.push(...((body.mediaIds as number[]) ?? []));
    if (method === 'PUT') row.entries = (body.mediaIds as number[]) ?? [];
    row.updatedAt = Math.floor(Date.now() / 1000);
    return json(res, 200, playlistJson(row, me, true));
  }
  if (path === '/api/favourites') {
    return json(
      res,
      200,
      (favourites.get(me.id) ?? [])
        .map((id) => files.find((f) => f.id === id))
        .filter((f): f is File => !!f)
        .map(mediaJson),
    );
  }
  match = /^\/api\/favourites\/(\d+)$/.exec(path);
  if (match) {
    const id = Number(match[1]);
    const list = (favourites.get(me.id) ?? []).filter((x) => x !== id);
    if (method === 'PUT') list.unshift(id);
    favourites.set(me.id, list);
    return json(res, 204);
  }
  return json(res, 404, { error: 'Not found.' });
}

const staticRoot = new URL('../../static/', import.meta.url);

type Next = () => void;

function middleware(req: IncomingMessage, res: ServerResponse, next: Next) {
  const url = new URL(req.url ?? '/', 'http://localhost');
  if (url.pathname.startsWith('/api/')) {
    handleApi(req, res, url).catch((error: unknown) => json(res, 500, { error: String(error) }));
    return;
  }
  const cover = /^\/photos\/mock-(\d+)\.svg$/.exec(url.pathname);
  if (cover) {
    const item = itemOf(Number(cover[1]));
    if (!item) return json(res, 404);
    res.setHeader('Content-Type', 'image/svg+xml');
    return res.end(coverSvg(item));
  }
  if (url.pathname.startsWith('/static/')) {
    const relative = normalize(url.pathname.slice('/static/'.length)).replace(/^(\.\.[/\\])+/, '');
    const file = join(staticRoot.pathname, relative);
    if (!file.startsWith(staticRoot.pathname) || !existsSync(file)) return json(res, 404);
    const types: Record<string, string> = { '.css': 'text/css', '.woff2': 'font/woff2', '.js': 'text/javascript' };
    res.setHeader('Content-Type', types[extname(file)] ?? 'application/octet-stream');
    return res.end(readFileSync(file));
  }
  next();
}

/** Serves the pretend API under `vite --mode mock` and `vite preview --mode mock`. */
export function mockApi(): Plugin {
  return {
    name: 'library-mock-api',
    configureServer(server) {
      server.middlewares.use(middleware);
    },
    configurePreviewServer(server) {
      server.middlewares.use(middleware);
    },
  };
}
