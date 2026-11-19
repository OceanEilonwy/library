import type { Account, ItemDetail, MediaFile, PlaylistSummary } from '../api';

export function mediaFile(id: number, overrides: Partial<MediaFile> = {}): MediaFile {
  return {
    id,
    itemId: 1,
    kind: 'audio',
    title: `Track ${id}`,
    disc: 1,
    track: id,
    mime: 'audio/flac',
    sizeBytes: 1000 * id,
    durationMs: 180_000,
    streamUrl: `/api/media/${id}/stream`,
    subtitles: [],
    itemTitle: 'Blue Train',
    creator: 'John Coltrane',
    year: 1957,
    cover: '/photos/blue-train.jpg',
    ...overrides,
  };
}

export function playlistSummary(id: number, overrides: Partial<PlaylistSummary> = {}): PlaylistSummary {
  return {
    id,
    name: `Playlist ${id}`,
    owner: 'alice',
    mine: false,
    entryCount: 3,
    totalDurationMs: 600_000,
    covers: [],
    updatedAt: 1_700_000_000,
    ...overrides,
  };
}

export function itemDetail(id: number, media: MediaFile[], overrides: Partial<ItemDetail> = {}): ItemDetail {
  return {
    id,
    title: 'Blue Train',
    creator: 'John Coltrane',
    year: 1957,
    type: 'Music',
    typeSlug: 'music',
    restricted: false,
    cover: '/photos/blue-train.jpg',
    kinds: ['audio'],
    fileCount: media.length,
    totalDurationMs: 600_000,
    description: '',
    media,
    ...overrides,
  };
}

export const alice: Account = { id: 1, username: 'alice', email: 'alice@example.org', access: 'adult' };
