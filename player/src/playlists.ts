// Display rules for playlists on the device.
//
// The server already hides entries a viewer may not open (docs/player.md,
// "Playlists"); these helpers cover what the device adds on top: being
// offline, filtering by name, and building the id list sent when an owner
// reorders or removes entries.

import type { MediaFile, PlaylistSummary } from './api';

/** Playlists worth showing: any with visible entries, plus all of your own (even empty ones). */
export function visiblePlaylists(playlists: readonly PlaylistSummary[]): PlaylistSummary[] {
  return playlists.filter((playlist) => playlist.mine || playlist.entryCount > 0);
}

/** Splits playlists into yours and everyone else's, keeping the server's order. */
export function partitionPlaylists(playlists: readonly PlaylistSummary[]): {
  mine: PlaylistSummary[];
  others: PlaylistSummary[];
} {
  const visible = visiblePlaylists(playlists);
  return { mine: visible.filter((p) => p.mine), others: visible.filter((p) => !p.mine) };
}

/** Case-insensitive match on the playlist name or owner. */
export function filterPlaylists(playlists: readonly PlaylistSummary[], query: string): PlaylistSummary[] {
  const needle = query.trim().toLocaleLowerCase();
  if (!needle) return [...playlists];
  return playlists.filter(
    (p) => p.name.toLocaleLowerCase().includes(needle) || p.owner.toLocaleLowerCase().includes(needle),
  );
}

export type EntryAvailability = { offline: boolean; downloaded: ReadonlySet<number> };

/** True when an entry can be opened right now (online, or downloaded). */
export function isAvailable(file: MediaFile, availability: EntryAvailability): boolean {
  return !availability.offline || availability.downloaded.has(file.id);
}

/** The entries that can play right now, in order. */
export function playableEntries(entries: readonly MediaFile[], availability: EntryAvailability): MediaFile[] {
  return entries.filter((file) => isAvailable(file, availability));
}

/** Audio entries only: what "Play all" queues (films and documents open on their own). */
export function audioOnly(entries: readonly MediaFile[]): MediaFile[] {
  return entries.filter((file) => file.kind === 'audio');
}

/** Entry ids with the entry at `from` moved to `to`. */
export function movedIds(entries: readonly MediaFile[], from: number, to: number): number[] {
  const ids = entries.map((file) => file.id);
  if (from < 0 || from >= ids.length || to < 0 || to >= ids.length || from === to) return ids;
  const [moved] = ids.splice(from, 1);
  if (moved !== undefined) ids.splice(to, 0, moved);
  return ids;
}

/** Entry ids without the entry at `position` (positions matter: a file can appear twice). */
export function idsWithout(entries: readonly MediaFile[], position: number): number[] {
  return entries.filter((_, index) => index !== position).map((file) => file.id);
}
