// The actions every track offers, shared by rows, Now Playing and Up Next.

import type { MediaFile } from '../api';
import type { Services } from '../services';
import type { MenuItem } from './Menu';

export type TrackMenuOptions = {
  /** Offer "Go to album" (not on the album's own page). */
  goToItem?: (() => void) | undefined;
  /** Extra items, shown after the standard ones (e.g. "Remove from playlist"). */
  extra?: readonly MenuItem[] | undefined;
  /** Hide queue actions (e.g. inside Up Next, which supplies its own). */
  queueActions?: boolean;
};

export function trackMenuItems(services: Services, file: MediaFile, options: TrackMenuOptions = {}): MenuItem[] {
  const { player, ui, downloads, favourites } = services;
  const items: MenuItem[] = [];
  if (file.kind === 'audio' && options.queueActions !== false) {
    items.push(
      {
        label: 'Play next',
        icon: 'queue',
        onSelect: () => {
          player.playNext([file]);
          ui.toast(`“${file.title}” will play next.`);
        },
      },
      {
        label: 'Add to queue',
        icon: 'plus',
        onSelect: () => {
          player.addToQueue([file]);
          ui.toast(`Added “${file.title}” to the queue.`);
        },
      },
    );
  }
  items.push({ label: 'Add to playlist…', icon: 'playlists', onSelect: () => ui.openPlaylistSheet([file]) });
  const state = downloads.state(file.id);
  if (state === 'done')
    items.push({ label: 'Remove download', icon: 'trash', onSelect: () => void downloads.remove(file.id) });
  else if (state === 'none')
    items.push({ label: 'Download', icon: 'download', onSelect: () => downloads.download([file]) });
  items.push(
    favourites.has(file.id)
      ? { label: 'Remove from favourites', icon: 'heartFilled', onSelect: () => void favourites.toggle(file) }
      : { label: 'Add to favourites', icon: 'heart', onSelect: () => void favourites.toggle(file) },
  );
  if (options.goToItem) items.push({ label: 'Go to album', icon: 'album', onSelect: options.goToItem });
  if (options.extra) items.push(...options.extra);
  return items;
}
