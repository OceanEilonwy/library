// A pretend MusicBrainz for the end-to-end tests, so the studio's "Fill from
// MusicBrainz" panel can be driven without the internet. The global setup
// starts it and points `[lookup] musicbrainz_url` (and the other lookup
// sources, which answer 404) at it.
//
// Its search lists a remastered pressing first; only the original's track
// lengths fit the files studio.spec.ts uploads, so the ranking should put it
// on top.

import { createServer, type Server } from 'node:http';
import type { AddressInfo } from 'node:net';

export const ORIGINAL = '5a0e7f3c-1d2b-4c5e-8f90-a1b2c3d4e5f6';
export const REMASTER = '6b1f8a4d-2e3c-4d6f-9a01-b2c3d4e5f607';
export const DELUXE = '7c2a9b5e-3f4d-4e70-8b12-c3d4e5f60718';

/** Track titles and lengths (ms) of each release. */
const RELEASES: Record<string, { title: string; date: string; country: string; tracks: [string, number][] }> = {
  [ORIGINAL]: {
    title: 'Studio Session',
    date: '2001-05-14',
    country: 'GB',
    tracks: [
      ['Opening', 12_000],
      ['Long Middle', 25_000],
      ['Closing Time', 40_000],
    ],
  },
  [REMASTER]: {
    title: 'Studio Session (Remastered)',
    date: '2015',
    country: 'US',
    tracks: [
      ['Opening', 55_000],
      ['Long Middle', 70_000],
      ['Closing Time', 90_000],
    ],
  },
  [DELUXE]: {
    title: 'Studio Session (Deluxe Edition)',
    date: '2021',
    country: 'XE',
    tracks: [
      ['Opening', 12_000],
      ['Long Middle', 25_000],
      ['Closing Time', 40_000],
      ['Demo', 150_000],
      ['Live', 200_000],
      ['Interview', 300_000],
    ],
  },
};

const credit = [{ name: 'The Uploaders', joinphrase: '' }];

function summary(id: string) {
  const release = RELEASES[id]!;
  return {
    id,
    title: release.title,
    date: release.date,
    country: release.country,
    'artist-credit': credit,
    media: [{ format: 'CD', 'track-count': release.tracks.length }],
  };
}

function full(id: string) {
  const release = RELEASES[id]!;
  return {
    ...summary(id),
    media: [
      {
        position: 1,
        format: 'CD',
        title: '',
        tracks: release.tracks.map(([title, length], index) => ({
          position: index + 1,
          number: String(index + 1),
          title,
          length,
          'artist-credit': credit,
        })),
      },
    ],
  };
}

export type MockMusicBrainz = { url: string; requests: string[]; close: () => Promise<void> };

export function startMockMusicBrainz(): Promise<MockMusicBrainz> {
  const requests: string[] = [];
  const server: Server = createServer((request, response) => {
    const url = new URL(request.url ?? '/', 'http://localhost');
    requests.push(url.pathname + url.search);
    const send = (status: number, body: unknown) => {
      response.writeHead(status, { 'Content-Type': 'application/json' });
      response.end(JSON.stringify(body));
    };
    if (!(request.headers['user-agent'] ?? '').startsWith('home-library/')) {
      send(403, { error: 'Say who you are' });
      return;
    }
    if (url.pathname === '/ws/2/release/') {
      send(200, { releases: [REMASTER, ORIGINAL, DELUXE].map(summary) });
      return;
    }
    const id = /^\/ws\/2\/release\/([0-9a-f-]{36})$/.exec(url.pathname)?.[1];
    if (id && RELEASES[id]) {
      send(200, full(id));
      return;
    }
    send(404, { error: 'Not Found' });
  });
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => {
      const { port } = server.address() as AddressInfo;
      server.unref();
      resolve({
        url: `http://127.0.0.1:${port}`,
        requests,
        close: () =>
          new Promise((done) => {
            server.close(() => done());
            // The library server keeps connections alive; don't wait for them.
            server.closeAllConnections();
          }),
      });
    });
  });
}
