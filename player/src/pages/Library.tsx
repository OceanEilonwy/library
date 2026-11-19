import { For, Show, createEffect, createMemo, createSignal, onSettled, useContext } from 'solid-js';
import type { ItemSummary, MediaKind } from '../api';
import { groupDownloads } from '../downloads';
import { formatBytes, plural } from '../format';
import { to } from '../links';
import { ServicesContext } from '../services';
import { Cover } from '../components/Cover';
import { EmptyNote, ErrorNote, ItemCard, LoadingNote, OfflineBanner, PageHeader } from '../components/Common';
import { Icon } from '../components/Icon';

const PAGE_SIZE = 40;
const kinds: { value: MediaKind | ''; label: string }[] = [
  { value: '', label: 'Everything' },
  { value: 'audio', label: 'Music' },
  { value: 'video', label: 'Films' },
  { value: 'document', label: 'Documents' },
];

export default function Library() {
  const { api, network, downloads } = useContext(ServicesContext);
  const [query, setQuery] = createSignal('');
  const [kind, setKind] = createSignal<MediaKind | ''>('');
  const [sort, setSort] = createSignal<'newest' | 'title'>('newest');
  const [items, setItems] = createSignal<ItemSummary[]>([]);
  const [total, setTotal] = createSignal(0);
  const [loading, setLoading] = createSignal(true);
  const [error, setError] = createSignal<unknown>(null);
  let ticket = 0;
  let debounce: ReturnType<typeof setTimeout> | undefined;

  async function load(append: boolean) {
    ticket += 1;
    const mine = ticket;
    setLoading(true);
    setError(null);
    try {
      const page = await api.items({
        q: query().trim() || undefined,
        kind: kind() || undefined,
        sort: sort(),
        offset: append ? items().length : 0,
        limit: PAGE_SIZE,
      });
      if (mine !== ticket) return;
      setItems((current) => (append ? [...current, ...page.items] : page.items));
      setTotal(page.total);
    } catch (failure) {
      if (mine === ticket) setError(() => failure);
    } finally {
      if (mine === ticket) setLoading(false);
    }
  }

  const reload = () => {
    clearTimeout(debounce);
    debounce = setTimeout(() => void load(false), 250);
  };

  onSettled(() => {
    void load(false);
    return () => clearTimeout(debounce);
  });

  // Back online after loading failed: try again.
  createEffect(
    () => network.offline(),
    (isOffline, wasOffline) => {
      if (wasOffline && !isOffline) void load(false);
    },
  );

  const offlineGroups = createMemo(() => groupDownloads(downloads.records()));

  return (
    <div class="page">
      <PageHeader title="Library" lede="Everything with a digital copy you can play, watch or read." />
      <OfflineBanner />

      <Show
        when={!network.offline()}
        fallback={
          <section>
            <h2 class="section-title">On this device</h2>
            <Show
              when={offlineGroups().length > 0}
              fallback={
                <EmptyNote title="Nothing downloaded yet">
                  <p>Download music and films while you're online to have them here.</p>
                </EmptyNote>
              }
            >
              <ul class="item-list">
                <For each={offlineGroups()}>
                  {(group) => (
                    <li>
                      <a class="item-line" href={to.item(group.itemId)}>
                        <Cover src={group.cover} size="small" />
                        <span class="track-text">
                          <span class="track-title">{group.itemTitle}</span>
                          <span class="track-sub">
                            {group.creator}, {plural(group.records.length, 'file')}, {formatBytes(group.sizeBytes)}
                          </span>
                        </span>
                      </a>
                    </li>
                  )}
                </For>
              </ul>
            </Show>
          </section>
        }
      >
        <form
          class="filters"
          role="search"
          onSubmit={(event) => {
            event.preventDefault();
            void load(false);
          }}
        >
          <label class="search-field">
            <Icon name="search" size={20} />
            <span class="visually-hidden">Search the library</span>
            <input
              type="search"
              placeholder="Search titles and artists"
              value={query()}
              onInput={(event) => {
                setQuery(event.currentTarget.value);
                reload();
              }}
            />
          </label>
          <div class="chips" role="group" aria-label="Show">
            <For each={kinds}>
              {(option) => (
                <button
                  type="button"
                  class="chip"
                  aria-pressed={kind() === option.value ? 'true' : 'false'}
                  onClick={() => {
                    setKind(option.value);
                    void load(false);
                  }}
                >
                  {option.label}
                </button>
              )}
            </For>
          </div>
          <label class="sort">
            <span>Sort</span>
            <select
              value={sort()}
              onChange={(event) => {
                setSort(event.currentTarget.value === 'title' ? 'title' : 'newest');
                void load(false);
              }}
            >
              <option value="newest">Newest first</option>
              <option value="title">A to Z</option>
            </select>
          </label>
        </form>

        <Show when={error()}>
          <ErrorNote error={error()} onRetry={() => void load(false)} />
        </Show>
        <Show when={!loading() || items().length > 0} fallback={<LoadingNote />}>
          <Show
            when={items().length > 0}
            fallback={
              <Show when={!error()}>
                <EmptyNote title={query() ? `Nothing matches “${query()}”` : 'Nothing here yet'}>
                  <p>
                    {query()
                      ? 'Try fewer words, or another spelling.'
                      : 'Digital copies the librarian adds will show up here.'}
                  </p>
                </EmptyNote>
              </Show>
            }
          >
            <p class="result-count">{plural(total(), 'item')}</p>
            <div class="grid">
              <For each={items()}>{(item) => <ItemCard item={item} />}</For>
            </div>
            <Show when={items().length < total()}>
              <button type="button" class="button ghost load-more" disabled={loading()} onClick={() => void load(true)}>
                {loading() ? 'Loading…' : 'Show more'}
              </button>
            </Show>
          </Show>
        </Show>
      </Show>
    </div>
  );
}
