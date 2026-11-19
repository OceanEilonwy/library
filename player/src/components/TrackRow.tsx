// A row in any list of media files: tap to play (or open a film or document),
// heart it, see whether it's downloaded, and reach every other action from "⋮".

import { Show, useContext } from 'solid-js';
import { useNavigate } from '@solidjs/router';
import type { JSX } from '@solidjs/web';
import type { MediaFile } from '../api';
import { formatTrackLength } from '../format';
import { openPath, to } from '../links';
import { ServicesContext } from '../services';
import { Cover } from './Cover';
import { DownloadMark } from './DownloadState';
import { HeartButton } from './HeartButton';
import { Icon } from './Icon';
import { Menu, type MenuItem } from './Menu';
import { trackMenuItems } from './TrackMenu';

export type TrackRowProps = {
  file: MediaFile;
  /** Called to play an audio file (usually: play this list from here). */
  onPlay: () => void;
  /** Show the cover instead of the track number. */
  showCover?: boolean;
  /** Show album and artist under the title, with "Go to album" in the menu. */
  showItem?: boolean;
  /** Greyed out and not playable (e.g. offline and not downloaded). */
  unavailable?: boolean;
  extraMenu?: readonly MenuItem[];
  /** A leading control, such as a drag handle. */
  leading?: JSX.Element;
};

export function NowPlayingBars() {
  return (
    <span class="eq" aria-hidden="true">
      <span />
      <span />
      <span />
    </span>
  );
}

export function TrackRow(props: TrackRowProps) {
  const services = useContext(ServicesContext);
  const navigate = useNavigate();
  const isCurrent = () => services.player.current()?.id === props.file.id;
  const subtitle = () => {
    if (!props.showItem) return props.file.kind === 'audio' ? '' : props.file.kind === 'video' ? 'Film' : 'Document';
    return [props.file.creator, props.file.itemTitle].filter(Boolean).join(' — ');
  };

  function activate() {
    if (props.unavailable) {
      services.ui.toast("That isn't downloaded, so it can't play while you're offline.");
      return;
    }
    const path = openPath(props.file);
    if (path) navigate(path);
    else props.onPlay();
  }

  return (
    <li class={['track', { current: isCurrent(), unavailable: !!props.unavailable }]}>
      {props.leading}
      <button type="button" class="track-main" onClick={activate} aria-current={isCurrent() ? 'true' : undefined}>
        <span class="track-lead">
          <Show
            when={props.showCover}
            fallback={
              <Show
                when={isCurrent() && services.player.playing()}
                fallback={
                  <span class="track-number">
                    {props.file.kind === 'audio' ? (
                      (props.file.track ?? '·')
                    ) : (
                      <Icon name={props.file.kind === 'video' ? 'film' : 'document'} size={18} />
                    )}
                  </span>
                }
              >
                <NowPlayingBars />
              </Show>
            }
          >
            <Cover src={props.file.cover} kind={props.file.kind} size="small" />
          </Show>
        </span>
        <span class="track-text">
          <span class="track-title">{props.file.title}</span>
          <Show when={subtitle()}>
            <span class="track-sub">{subtitle()}</span>
          </Show>
        </span>
      </button>
      <DownloadMark file={props.file} />
      <span class="track-length">{formatTrackLength(props.file.durationMs)}</span>
      <HeartButton file={props.file} />
      <Menu
        label={`More options for “${props.file.title}”`}
        items={trackMenuItems(services, props.file, {
          goToItem: props.showItem ? () => navigate(to.item(props.file.itemId)) : undefined,
          extra: props.extraMenu,
        })}
      />
    </li>
  );
}
