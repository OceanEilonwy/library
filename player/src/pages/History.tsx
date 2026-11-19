import { For, Show, createMemo, createSignal, useContext } from 'solid-js';
import { useNavigate } from '@solidjs/router';
import { groupByDay } from '../format';
import { filterHistory } from '../history';
import { openPath } from '../links';
import { createRequest } from '../request';
import { ServicesContext } from '../services';
import { Cover } from '../components/Cover';
import { ConfirmButton, EmptyNote, LoadingNote, PageHeader } from '../components/Common';
import { Icon } from '../components/Icon';
import { Menu } from '../components/Menu';
import { trackMenuItems } from '../components/TrackMenu';
import type { HistoryEntry } from '../db';

const timeFormat = new Intl.DateTimeFormat('en-GB', { hour: 'numeric', minute: '2-digit' });

export default function History() {
  const services = useContext(ServicesContext);
  const { history, player, network, downloads, ui } = services;
  const navigate = useNavigate();
  const entries = createRequest(
    () => history.version() + 1,
    () => history.store.list(),
  );
  const [query, setQuery] = createSignal('');
  const groups = createMemo(() => groupByDay(filterHistory(entries.data() ?? [], query()), (entry) => entry.playedAt));

  function play(entry: HistoryEntry) {
    if (network.offline() && !downloads.isDownloaded(entry.file.id)) {
      ui.toast("That isn't downloaded, so it can't play while you're offline.");
      return;
    }
    const path = openPath(entry.file);
    if (path) navigate(path);
    else player.playFiles([entry.file]);
  }

  return (
    <div class="page">
      <PageHeader title="History" lede="What you've played on this device. It stays here and is never sent anywhere.">
        <Show when={(entries.data() ?? []).length > 0}>
          <ConfirmButton
            label="Clear history"
            confirmLabel="Tap again to clear"
            icon="trash"
            class="ghost"
            onConfirm={() => history.clear()}
          />
        </Show>
      </PageHeader>

      <Show when={!entries.loading() || entries.data()} fallback={<LoadingNote />}>
        <Show
          when={(entries.data() ?? []).length > 0}
          fallback={
            <EmptyNote title="Nothing played yet">
              <p>Anything you listen to for more than ten seconds shows up here.</p>
            </EmptyNote>
          }
        >
          <label class="search-field">
            <Icon name="search" size={20} />
            <span class="visually-hidden">Search your history</span>
            <input
              type="search"
              placeholder="Search your history"
              value={query()}
              onInput={(e) => setQuery(e.currentTarget.value)}
            />
          </label>
          <Show
            when={groups().length > 0}
            fallback={<p class="empty-note">Nothing in your history matches “{query()}”.</p>}
          >
            <For each={groups()}>
              {(group) => (
                <section class="day">
                  <h2 class="list-heading">{group.label}</h2>
                  <ul class="track-list">
                    <For each={group.entries}>
                      {(entry) => (
                        <li class="track">
                          <button type="button" class="track-main" onClick={() => play(entry)}>
                            <span class="track-lead">
                              <Cover src={entry.file.cover} kind={entry.file.kind} size="small" />
                            </span>
                            <span class="track-text">
                              <span class="track-title">{entry.file.title}</span>
                              <span class="track-sub">
                                {entry.file.creator} — {entry.file.itemTitle}
                              </span>
                            </span>
                          </button>
                          <time class="track-length" datetime={new Date(entry.playedAt).toISOString()}>
                            {timeFormat.format(entry.playedAt)}
                          </time>
                          <Menu
                            label={`More options for “${entry.file.title}”`}
                            items={trackMenuItems(services, entry.file, {
                              extra: [
                                {
                                  label: 'Remove from history',
                                  icon: 'close',
                                  danger: true,
                                  onSelect: () => void history.remove(entry.id ?? -1),
                                },
                              ],
                            })}
                          />
                        </li>
                      )}
                    </For>
                  </ul>
                </section>
              )}
            </For>
          </Show>
        </Show>
      </Show>
    </div>
  );
}
