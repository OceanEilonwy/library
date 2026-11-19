import { For, Show, createMemo, useContext } from 'solid-js';
import { useParams } from '@solidjs/router';
import { isApiError, type ItemDetail, type MediaFile } from '../api';
import { formatSpan, plural } from '../format';
import { audioOnly, isAvailable } from '../playlists';
import { createRequest } from '../request';
import { ServicesContext, type Services } from '../services';
import { Cover } from '../components/Cover';
import { DownloadAllButton } from '../components/DownloadState';
import { ErrorNote, LoadingNote, OfflineBanner } from '../components/Common';
import { Icon } from '../components/Icon';
import { Menu } from '../components/Menu';
import { TrackRow } from '../components/TrackRow';

/** Builds what we can of an item from its downloaded files, for offline use. */
async function offlineItem(services: Services, id: number): Promise<ItemDetail | null> {
  const records = await services.downloads.store.forItem(id);
  const first = records[0]?.file;
  if (!first) return null;
  const media = records.map((record) => record.file);
  return {
    id,
    title: first.itemTitle,
    creator: first.creator,
    year: first.year,
    type: '',
    typeSlug: '',
    restricted: false,
    cover: first.cover,
    kinds: [...new Set(media.map((file) => file.kind))],
    fileCount: media.length,
    totalDurationMs: media.reduce((sum, file) => sum + (file.durationMs ?? 0), 0) || null,
    description: '',
    media,
  };
}

/** Files grouped by disc, when there is more than one disc. */
function discs(media: readonly MediaFile[]): { disc: number; files: MediaFile[] }[] {
  const groups: { disc: number; files: MediaFile[] }[] = [];
  for (const file of media) {
    const last = groups[groups.length - 1];
    if (last && last.disc === file.disc) last.files.push(file);
    else groups.push({ disc: file.disc, files: [file] });
  }
  return groups;
}

export default function Item() {
  const services = useContext(ServicesContext);
  const { api, player, ui, network, downloads } = services;
  const params = useParams();
  const id = () => Number(params.id);

  const item = createRequest(
    () => (Number.isInteger(id()) ? id() : null),
    async (itemId) => {
      try {
        return await api.item(itemId);
      } catch (error) {
        if (isApiError(error) && error.kind === 'offline') {
          const fromDevice = await offlineItem(services, itemId);
          if (fromDevice) return fromDevice;
        }
        throw error;
      }
    },
  );

  const availability = () => ({ offline: network.offline(), downloaded: downloads.ids() });
  const playable = createMemo(() => (item.data()?.media ?? []).filter((file) => isAvailable(file, availability())));
  const audio = createMemo(() => audioOnly(playable()));
  const groups = createMemo(() => discs(item.data()?.media ?? []));

  return (
    <div class="page">
      <OfflineBanner />
      <Show
        when={item.data()}
        fallback={
          <Show when={item.error()} fallback={<LoadingNote />}>
            <ErrorNote error={item.error()} onRetry={item.reload} />
          </Show>
        }
      >
        {(detail) => (
          <>
            <header class="hero">
              <div class="hero-art">
                <Cover src={detail().cover} kind={detail().kinds[0]} size="fill" alt={`Cover of ${detail().title}`} />
              </div>
              <div class="hero-text">
                <Show when={detail().type}>
                  <p class="hero-kind">{detail().type}</p>
                </Show>
                <h1>{detail().title}</h1>
                <p class="hero-creator">{detail().creator}</p>
                <p class="hero-meta">
                  {[
                    detail().year ? String(detail().year) : '',
                    plural(detail().fileCount, 'file'),
                    formatSpan(detail().totalDurationMs),
                  ]
                    .filter(Boolean)
                    .join(', ')}
                  <Show when={detail().restricted}>
                    {' '}
                    <span class="badge">Adults only</span>
                  </Show>
                </p>
                <div class="hero-actions">
                  <Show when={audio().length > 0}>
                    <button type="button" class="button primary" onClick={() => player.playFiles(audio())}>
                      <Icon name="play" size={20} /> Play
                    </button>
                    <button type="button" class="button" onClick={() => player.shuffleFiles(audio())}>
                      <Icon name="shuffle" size={20} /> Shuffle
                    </button>
                  </Show>
                  <DownloadAllButton files={detail().media} />
                  <Menu
                    label={`More options for “${detail().title}”`}
                    items={[
                      ...(audio().length > 0
                        ? [
                            {
                              label: 'Play next',
                              icon: 'queue' as const,
                              onSelect: () => {
                                player.playNext(audio());
                                ui.toast(`“${detail().title}” will play next.`);
                              },
                            },
                            {
                              label: 'Add to queue',
                              icon: 'plus' as const,
                              onSelect: () => {
                                player.addToQueue(audio());
                                ui.toast(`Added “${detail().title}” to the queue.`);
                              },
                            },
                          ]
                        : []),
                      {
                        label: 'Add to playlist…',
                        icon: 'playlists',
                        onSelect: () => ui.openPlaylistSheet(detail().media),
                      },
                    ]}
                  />
                </div>
              </div>
            </header>

            <Show when={detail().description}>
              <p class="description">{detail().description}</p>
            </Show>

            <For each={groups()}>
              {(group) => (
                <section class="disc">
                  <Show when={groups().length > 1}>
                    <h2 class="list-heading">Disc {group.disc}</h2>
                  </Show>
                  <ol class="track-list">
                    <For each={group.files}>
                      {(file) => (
                        <TrackRow
                          file={file}
                          unavailable={!isAvailable(file, availability())}
                          onPlay={() => player.playFiles(audio(), file)}
                        />
                      )}
                    </For>
                  </ol>
                </section>
              )}
            </For>
          </>
        )}
      </Show>
    </div>
  );
}
