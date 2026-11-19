// The queue: what's playing and what comes next. Drag the handle (or use
// ↑/↓ on it) to reorder; each row has its own menu.

import { For, Show, useContext } from 'solid-js';
import type { MediaFile } from '../api';
import { formatTrackLength } from '../format';
import { upNext } from '../player/queue';
import { handleReorderKey, startDrag } from '../reorder';
import { ServicesContext } from '../services';
import { Cover } from './Cover';
import { Icon } from './Icon';
import { Menu } from './Menu';
import { trackMenuItems } from './TrackMenu';

export function UpNext() {
  const services = useContext(ServicesContext);
  const { player, ui } = services;
  const queue = () => player.state().queue;
  const upcoming = () => upNext(queue());
  let list: HTMLOListElement | undefined;

  /** Positions in Up Next are offsets after the current entry. */
  const toQueuePosition = (offset: number) => queue().index + 1 + offset;
  const move = (from: number, to: number) => player.engine.moveEntry(toQueuePosition(from), toQueuePosition(to));

  const menuFor = (key: number, offset: number, file: MediaFile) =>
    trackMenuItems(services, file, {
      queueActions: false,
      extra: [
        { label: 'Play next', icon: 'queue', onSelect: () => move(offset, 0), disabled: offset === 0 },
        { label: 'Add to queue again', icon: 'plus', onSelect: () => player.engine.addToQueue([file]) },
        { label: 'Remove from queue', icon: 'close', onSelect: () => player.engine.removeEntry(key), danger: true },
      ],
    });

  return (
    <div class="up-next">
      <Show when={player.current()}>
        {(file) => (
          <div class="up-next-current">
            <p class="list-heading">Now playing</p>
            <div class="track current">
              <span class="track-lead">
                <Cover src={file().cover} size="small" />
              </span>
              <span class="track-text">
                <span class="track-title">{file().title}</span>
                <span class="track-sub">{file().creator}</span>
              </span>
            </div>
          </div>
        )}
      </Show>
      <div class="list-heading-row">
        <p class="list-heading">Up next</p>
        <Show when={upcoming().length > 0}>
          <button
            type="button"
            class="text-button"
            onClick={() => {
              for (const entry of upcoming()) player.engine.removeEntry(entry.key);
              ui.toast('Cleared Up next.');
            }}
          >
            Clear
          </button>
        </Show>
      </div>
      <Show
        when={upcoming().length > 0}
        fallback={<p class="empty-note">Nothing queued. Use “Play next” or “Add to queue” on any track.</p>}
      >
        <ol class="track-list reorderable" ref={(el) => (list = el)}>
          <For each={upcoming()} keyed={(entry) => entry.key}>
            {(entry, index) => (
              <li class="track">
                <button
                  type="button"
                  class="drag-handle"
                  aria-label={`Move “${entry().track.title}”. Use the up and down arrow keys.`}
                  onPointerDown={(event) => startDrag(event, index(), { list: () => list, onMove: move })}
                  onKeyDown={(event) => {
                    if (handleReorderKey(event, index(), upcoming().length, move)) event.preventDefault();
                  }}
                >
                  <Icon name="drag" size={20} />
                </button>
                <button type="button" class="track-main" onClick={() => player.engine.jumpTo(toQueuePosition(index()))}>
                  <span class="track-lead">
                    <Cover src={entry().track.cover} size="small" />
                  </span>
                  <span class="track-text">
                    <span class="track-title">{entry().track.title}</span>
                    <span class="track-sub">{entry().track.creator}</span>
                  </span>
                </button>
                <span class="track-length">{formatTrackLength(entry().track.durationMs)}</span>
                <Menu
                  label={`More options for “${entry().track.title}”`}
                  items={menuFor(entry().key, index(), entry().track)}
                />
              </li>
            )}
          </For>
        </ol>
      </Show>
    </div>
  );
}
