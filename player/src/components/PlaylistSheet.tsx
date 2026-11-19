// "Add to playlist": pick one of your playlists, or make a new one on the spot.

import { For, Show, createSignal, onSettled, useContext } from 'solid-js';
import { errorMessage } from '../api';
import { plural } from '../format';
import { createRequest } from '../request';
import { ServicesContext } from '../services';
import { CoverMosaic } from './Cover';
import { Icon } from './Icon';
import { trapTab } from './focus';

export function PlaylistSheet() {
  const { api, ui } = useContext(ServicesContext);
  const files = () => ui.playlistSheet() ?? [];
  const mine = createRequest(
    () => true,
    () => api.playlists(true),
  );
  const [name, setName] = createSignal('');
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  let dialog: HTMLDivElement | undefined;

  const what = () => (files().length === 1 ? `“${files()[0]?.title ?? ''}”` : plural(files().length, 'file'));

  async function addTo(id: number, playlistName: string) {
    setBusy(true);
    setError(null);
    try {
      await api.addToPlaylist(
        id,
        files().map((file) => file.id),
      );
      ui.toast(`Added ${what()} to ${playlistName}.`);
      ui.closePlaylistSheet();
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setBusy(false);
    }
  }

  async function create(event: SubmitEvent) {
    event.preventDefault();
    const trimmed = name().trim();
    if (!trimmed) {
      setError('Give the playlist a name.');
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const playlist = await api.createPlaylist(trimmed);
      await addTo(playlist.id, playlist.name);
    } catch (failure) {
      setError(errorMessage(failure));
      setBusy(false);
    }
  }

  onSettled(() => {
    dialog?.querySelector<HTMLElement>('input, button')?.focus();
    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') ui.closePlaylistSheet();
      trapTab(dialog, event);
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  });

  return (
    <div class="sheet-layer">
      <div class="sheet-backdrop" onClick={() => ui.closePlaylistSheet()} />
      <div class="sheet" role="dialog" aria-modal="true" aria-labelledby="sheet-title" ref={(el) => (dialog = el)}>
        <header class="sheet-head">
          <h2 id="sheet-title">Add {what()} to a playlist</h2>
          <button type="button" class="icon-button" aria-label="Close" onClick={() => ui.closePlaylistSheet()}>
            <Icon name="close" />
          </button>
        </header>
        <form class="inline-form" onSubmit={create}>
          <label class="visually-hidden" for="new-playlist-name">
            New playlist name
          </label>
          <input
            id="new-playlist-name"
            type="text"
            placeholder="New playlist name"
            value={name()}
            onInput={(e) => setName(e.currentTarget.value)}
            maxlength="100"
            autocomplete="off"
          />
          <button type="submit" class="button" disabled={busy()}>
            <Icon name="plus" size={20} /> Create
          </button>
        </form>
        <Show when={error()}>
          <p class="form-error" role="alert">
            {error()}
          </p>
        </Show>
        <Show when={!mine.loading()} fallback={<p class="empty-note">Loading your playlists…</p>}>
          <Show
            when={(mine.data() ?? []).length > 0}
            fallback={<p class="empty-note">You haven't made any playlists yet. Name one above.</p>}
          >
            <ul class="sheet-list">
              <For each={mine.data() ?? []}>
                {(playlist) => (
                  <li>
                    <button
                      type="button"
                      class="sheet-choice"
                      disabled={busy()}
                      onClick={() => void addTo(playlist.id, playlist.name)}
                    >
                      <CoverMosaic covers={playlist.covers} />
                      <span class="track-text">
                        <span class="track-title">{playlist.name}</span>
                        <span class="track-sub">{plural(playlist.entryCount, 'entry', 'entries')}</span>
                      </span>
                    </button>
                  </li>
                )}
              </For>
            </ul>
          </Show>
        </Show>
      </div>
    </div>
  );
}
