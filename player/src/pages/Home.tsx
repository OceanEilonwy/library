import { For, Show, createMemo, useContext } from 'solid-js';
import { visiblePlaylists } from '../playlists';
import { to } from '../links';
import { createRequest } from '../request';
import { ServicesContext } from '../services';
import { withMemory } from '../services/memory';
import { isApiError, type MediaFile } from '../api';
import { Cover } from '../components/Cover';
import { EmptyNote, ItemCard, OfflineBanner, PlaylistCard, Shelf } from '../components/Common';
import { TrackRow } from '../components/TrackRow';

function greeting(hour: number): string {
  if (hour < 5) return 'Up late';
  if (hour < 12) return 'Good morning';
  if (hour < 18) return 'Good afternoon';
  return 'Good evening';
}

const offline = (error: unknown) => isApiError(error) && error.kind === 'offline';

export default function Home() {
  const { api, session, history, favourites, player, memory, downloads, network } = useContext(ServicesContext);
  const recent = createRequest(
    () => history.version() + 1,
    () => history.store.recent(10),
  );
  const added = createRequest(
    () => !network.offline(),
    () => api.items({ limit: 12 }),
  );
  const playlists = createRequest(
    () => true,
    () => withMemory(memory, 'playlists', () => api.playlists(), offline),
  );
  const shownPlaylists = createMemo(() => visiblePlaylists(playlists.data() ?? []).slice(0, 10));

  const playRecent = (file: MediaFile) => {
    const list = recent.data() ?? [];
    player.playFiles(list, list.indexOf(file));
  };

  return (
    <div class="page home">
      <header class="page-header home-header">
        <h1>
          {greeting(new Date().getHours())}
          <Show when={session.account()}>{(account) => <>, {account().username}</>}</Show>
        </h1>
      </header>
      <OfflineBanner>
        You're offline. <a href={to.downloads()}>Your downloads</a> are ready to play.
      </OfflineBanner>

      <Show when={(recent.data() ?? []).length > 0}>
        <Shelf title="Continue listening" href={to.history()} linkLabel="History">
          <For each={recent.data() ?? []}>
            {(file) => (
              <button
                type="button"
                class="card"
                onClick={() => playRecent(file)}
                disabled={network.offline() && !downloads.isDownloaded(file.id)}
              >
                <Cover src={file.cover} kind={file.kind} size="fill" />
                <span class="card-title">{file.title}</span>
                <span class="card-sub">{file.creator}</span>
              </button>
            )}
          </For>
        </Shelf>
      </Show>

      <Show when={(added.data()?.items ?? []).length > 0}>
        <Shelf title="Recently added" href={to.library()} linkLabel="Library">
          <For each={added.data()?.items ?? []}>{(item) => <ItemCard item={item} />}</For>
        </Shelf>
      </Show>

      <Show when={favourites.files().length > 0}>
        <section class="shelf">
          <div class="shelf-head">
            <h2>Your favourites</h2>
            <a class="text-button" href={to.favourites()}>
              See all
            </a>
          </div>
          <ul class="track-list">
            <For each={favourites.files().slice(0, 5)}>
              {(file) => (
                <TrackRow file={file} showCover showItem onPlay={() => player.playFiles(favourites.files(), file)} />
              )}
            </For>
          </ul>
        </section>
      </Show>

      <Show when={shownPlaylists().length > 0}>
        <Shelf title="Playlists" href={to.playlists()}>
          <For each={shownPlaylists()}>{(playlist) => <PlaylistCard playlist={playlist} />}</For>
        </Shelf>
      </Show>

      <Show
        when={
          !added.loading() &&
          !recent.loading() &&
          (added.data()?.items ?? []).length === 0 &&
          (recent.data() ?? []).length === 0 &&
          !network.offline()
        }
      >
        <EmptyNote title="Nothing to play yet">
          <p>When the librarian adds digital copies of records, films or documents, they'll appear here.</p>
        </EmptyNote>
      </Show>
    </div>
  );
}
