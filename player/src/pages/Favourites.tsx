import { For, Show, createMemo, useContext } from 'solid-js';
import { audioOnly, isAvailable } from '../playlists';
import { plural } from '../format';
import { ServicesContext } from '../services';
import { DownloadAllButton } from '../components/DownloadState';
import { EmptyNote, LoadingNote, OfflineBanner, PageHeader } from '../components/Common';
import { Icon } from '../components/Icon';
import { TrackRow } from '../components/TrackRow';

export default function Favourites() {
  const { favourites, player, network, downloads } = useContext(ServicesContext);
  const availability = () => ({ offline: network.offline(), downloaded: downloads.ids() });
  const playable = createMemo(() => audioOnly(favourites.files().filter((file) => isAvailable(file, availability()))));

  return (
    <div class="page">
      <PageHeader
        title="Favourites"
        lede={favourites.files().length ? plural(favourites.files().length, 'favourite') : undefined}
      >
        <Show when={playable().length > 0}>
          <button type="button" class="button primary" onClick={() => player.playFiles(playable())}>
            <Icon name="play" size={20} /> Play
          </button>
          <button type="button" class="button" onClick={() => player.shuffleFiles(playable())}>
            <Icon name="shuffle" size={20} /> Shuffle
          </button>
        </Show>
        <Show when={favourites.files().length > 0}>
          <DownloadAllButton files={favourites.files()} />
        </Show>
      </PageHeader>
      <OfflineBanner />
      <Show
        when={favourites.loaded() || favourites.files().length > 0}
        fallback={
          <Show when={!network.offline()} fallback={<EmptyNote title="Favourites need a connection the first time" />}>
            <LoadingNote />
          </Show>
        }
      >
        <Show
          when={favourites.files().length > 0}
          fallback={
            <EmptyNote title="No favourites yet">
              <p>Tap the heart beside any track, film or document to keep it here.</p>
            </EmptyNote>
          }
        >
          <ul class="track-list">
            <For each={favourites.files()}>
              {(file) => (
                <TrackRow
                  file={file}
                  showCover
                  showItem
                  unavailable={!isAvailable(file, availability())}
                  onPlay={() => player.playFiles(playable(), file)}
                />
              )}
            </For>
          </ul>
        </Show>
      </Show>
    </div>
  );
}
