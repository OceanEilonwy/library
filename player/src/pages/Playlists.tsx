import { For, Show, createMemo, createSignal, useContext } from 'solid-js';
import { useNavigate } from '@solidjs/router';
import { errorMessage, isApiError } from '../api';
import { to } from '../links';
import { filterPlaylists, partitionPlaylists } from '../playlists';
import { createRequest } from '../request';
import { ServicesContext } from '../services';
import { withMemory } from '../services/memory';
import { EmptyNote, ErrorNote, LoadingNote, OfflineBanner, PageHeader, PlaylistCard } from '../components/Common';
import { Icon } from '../components/Icon';

export default function Playlists() {
  const { api, memory, network } = useContext(ServicesContext);
  const navigate = useNavigate();
  const list = createRequest(
    () => true,
    () =>
      withMemory(
        memory,
        'playlists',
        () => api.playlists(),
        (e) => isApiError(e) && e.kind === 'offline',
      ),
  );
  const [filter, setFilter] = createSignal('');
  const [name, setName] = createSignal('');
  const [createError, setCreateError] = createSignal<string | null>(null);
  const [creating, setCreating] = createSignal(false);
  const parts = createMemo(() => partitionPlaylists(filterPlaylists(list.data() ?? [], filter())));

  async function create(event: SubmitEvent) {
    event.preventDefault();
    const trimmed = name().trim();
    if (!trimmed) {
      setCreateError('Give the playlist a name.');
      return;
    }
    setCreating(true);
    setCreateError(null);
    try {
      const playlist = await api.createPlaylist(trimmed);
      navigate(to.playlist(playlist.id));
    } catch (error) {
      setCreateError(errorMessage(error));
    } finally {
      setCreating(false);
    }
  }

  return (
    <div class="page">
      <PageHeader title="Playlists" lede="Everyone's playlists are open to all. Make your own from any track." />
      <OfflineBanner />

      <div class="toolbar">
        <form class="inline-form" onSubmit={create}>
          <label class="visually-hidden" for="playlist-name">
            New playlist name
          </label>
          <input
            id="playlist-name"
            type="text"
            placeholder="Name a new playlist"
            maxlength="100"
            value={name()}
            onInput={(e) => setName(e.currentTarget.value)}
            disabled={network.offline()}
          />
          <button type="submit" class="button primary" disabled={creating() || network.offline()}>
            <Icon name="plus" size={20} /> New playlist
          </button>
        </form>
        <label class="search-field compact">
          <Icon name="search" size={18} />
          <span class="visually-hidden">Filter playlists</span>
          <input
            type="search"
            placeholder="Filter"
            value={filter()}
            onInput={(e) => setFilter(e.currentTarget.value)}
          />
        </label>
      </div>
      <Show when={createError()}>
        <p class="form-error" role="alert">
          {createError()}
        </p>
      </Show>

      <Show
        when={list.data()}
        fallback={
          <Show when={list.error()} fallback={<LoadingNote />}>
            <ErrorNote error={list.error()} onRetry={list.reload} />
          </Show>
        }
      >
        <section>
          <h2 class="section-title">Yours</h2>
          <Show
            when={parts().mine.length > 0}
            fallback={
              <p class="empty-note">
                {filter()
                  ? 'None of yours match.'
                  : 'You have no playlists yet. Name one above, or use “Add to playlist” on any track.'}
              </p>
            }
          >
            <div class="grid">
              <For each={parts().mine}>{(playlist) => <PlaylistCard playlist={playlist} />}</For>
            </div>
          </Show>
        </section>
        <section>
          <h2 class="section-title">From everyone</h2>
          <Show
            when={parts().others.length > 0}
            fallback={<EmptyNote title={filter() ? 'No playlists match.' : 'No one else has shared a playlist yet.'} />}
          >
            <div class="grid">
              <For each={parts().others}>{(playlist) => <PlaylistCard playlist={playlist} />}</For>
            </div>
          </Show>
        </section>
      </Show>
    </div>
  );
}
