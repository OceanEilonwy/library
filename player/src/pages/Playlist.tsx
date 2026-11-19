import { For, Show, createMemo, createSignal, useContext } from 'solid-js';
import { useNavigate, useParams } from '@solidjs/router';
import { errorMessage, isApiError, type Playlist as PlaylistData } from '../api';
import { formatSpan, plural } from '../format';
import { to } from '../links';
import { audioOnly, idsWithout, isAvailable, movedIds } from '../playlists';
import { handleReorderKey, startDrag } from '../reorder';
import { createRequest } from '../request';
import { ServicesContext } from '../services';
import { withMemory } from '../services/memory';
import { CoverMosaic } from '../components/Cover';
import { DownloadAllButton } from '../components/DownloadState';
import { ConfirmButton, EmptyNote, ErrorNote, LoadingNote, OfflineBanner } from '../components/Common';
import { Icon } from '../components/Icon';
import { TrackRow } from '../components/TrackRow';

export default function Playlist() {
  const { api, memory, player, ui, network, downloads } = useContext(ServicesContext);
  const params = useParams();
  const navigate = useNavigate();
  const id = () => Number(params.id);
  const playlist = createRequest(
    () => (Number.isInteger(id()) ? id() : null),
    (playlistId) =>
      withMemory(
        memory,
        `playlist:${playlistId}`,
        () => api.playlist(playlistId),
        (e) => isApiError(e) && e.kind === 'offline',
      ),
  );
  const [renaming, setRenaming] = createSignal(false);
  const [draftName, setDraftName] = createSignal('');
  const [saving, setSaving] = createSignal(false);
  let list: HTMLOListElement | undefined;

  const availability = () => ({ offline: network.offline(), downloaded: downloads.ids() });
  const entries = () => playlist.data()?.entries ?? [];
  const playable = createMemo(() => audioOnly(entries().filter((file) => isAvailable(file, availability()))));
  const canEdit = () => !!playlist.data()?.mine && !network.offline();

  async function save(change: Promise<PlaylistData>) {
    setSaving(true);
    try {
      const updated = await change;
      playlist.mutate(updated);
      void memory.save(`playlist:${updated.id}`, updated);
    } catch (error) {
      ui.toast(errorMessage(error));
    } finally {
      setSaving(false);
    }
  }

  const move = (from: number, to: number) => {
    const current = playlist.data();
    if (!current || from === to) return;
    // Show the new order at once; the server's answer replaces it.
    const reordered = [...current.entries];
    const [moved] = reordered.splice(from, 1);
    if (moved) reordered.splice(to, 0, moved);
    playlist.mutate({ ...current, entries: reordered });
    void save(api.setPlaylistEntries(current.id, movedIds(current.entries, from, to)));
  };

  const removeAt = (position: number) => {
    const current = playlist.data();
    if (current) void save(api.setPlaylistEntries(current.id, idsWithout(current.entries, position)));
  };

  async function rename(event: SubmitEvent) {
    event.preventDefault();
    const current = playlist.data();
    const name = draftName().trim();
    if (!current || !name) return;
    await save(api.renamePlaylist(current.id, name));
    setRenaming(false);
  }

  async function remove() {
    const current = playlist.data();
    if (!current) return;
    try {
      await api.deletePlaylist(current.id);
      ui.toast(`Deleted “${current.name}”.`);
      navigate(to.playlists(), { replace: true });
    } catch (error) {
      ui.toast(errorMessage(error));
    }
  }

  return (
    <div class="page">
      <OfflineBanner />
      <Show
        when={playlist.data()}
        fallback={
          <Show when={playlist.error()} fallback={<LoadingNote />}>
            <ErrorNote error={playlist.error()} onRetry={playlist.reload} />
          </Show>
        }
      >
        {(data) => (
          <>
            <header class="hero">
              <div class="hero-art">
                <CoverMosaic covers={data().covers} size="fill" />
              </div>
              <div class="hero-text">
                <p class="hero-kind">Playlist</p>
                <Show when={renaming()} fallback={<h1>{data().name}</h1>}>
                  <form class="inline-form" onSubmit={rename}>
                    <label class="visually-hidden" for="rename">
                      Playlist name
                    </label>
                    <input
                      id="rename"
                      type="text"
                      value={draftName()}
                      maxlength="100"
                      onInput={(e) => setDraftName(e.currentTarget.value)}
                    />
                    <button type="submit" class="button primary" disabled={saving()}>
                      Save
                    </button>
                    <button type="button" class="button ghost" onClick={() => setRenaming(false)}>
                      Cancel
                    </button>
                  </form>
                </Show>
                <p class="hero-creator">{data().mine ? 'Your playlist' : `By ${data().owner}`}</p>
                <p class="hero-meta">
                  {[plural(data().entryCount, 'entry', 'entries'), formatSpan(data().totalDurationMs)]
                    .filter(Boolean)
                    .join(', ')}
                </p>
                <div class="hero-actions">
                  <Show when={playable().length > 0}>
                    <button type="button" class="button primary" onClick={() => player.playFiles(playable())}>
                      <Icon name="play" size={20} /> Play
                    </button>
                    <button type="button" class="button" onClick={() => player.shuffleFiles(playable())}>
                      <Icon name="shuffle" size={20} /> Shuffle
                    </button>
                  </Show>
                  <DownloadAllButton files={data().entries} />
                  <Show when={canEdit()}>
                    <button
                      type="button"
                      class="button ghost"
                      onClick={() => {
                        setDraftName(data().name);
                        setRenaming(true);
                      }}
                    >
                      <Icon name="edit" size={20} /> Rename
                    </button>
                    <ConfirmButton
                      label="Delete"
                      confirmLabel="Tap again to delete"
                      icon="trash"
                      class="ghost"
                      onConfirm={remove}
                    />
                  </Show>
                </div>
              </div>
            </header>

            <Show
              when={entries().length > 0}
              fallback={
                <EmptyNote title="This playlist is empty">
                  <p>Use “Add to playlist” from any track's menu to fill it.</p>
                </EmptyNote>
              }
            >
              <ol class={['track-list', { reorderable: canEdit() }]} ref={(el) => (list = el)}>
                <For each={entries()}>
                  {(file, index) => (
                    <TrackRow
                      file={file}
                      showCover
                      showItem
                      unavailable={!isAvailable(file, availability())}
                      onPlay={() => player.playFiles(playable(), file)}
                      leading={
                        canEdit() ? (
                          <button
                            type="button"
                            class="drag-handle"
                            aria-label={`Move “${file.title}”. Use the up and down arrow keys.`}
                            onPointerDown={(event) => startDrag(event, index(), { list: () => list, onMove: move })}
                            onKeyDown={(event) => {
                              if (handleReorderKey(event, index(), entries().length, move)) event.preventDefault();
                            }}
                          >
                            <Icon name="drag" size={20} />
                          </button>
                        ) : undefined
                      }
                      extraMenu={
                        canEdit()
                          ? [
                              {
                                label: 'Remove from this playlist',
                                icon: 'close',
                                danger: true,
                                onSelect: () => removeAt(index()),
                              },
                            ]
                          : []
                      }
                    />
                  )}
                </For>
              </ol>
            </Show>
          </>
        )}
      </Show>
    </div>
  );
}
