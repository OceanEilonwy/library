import { For, Show, createMemo, createSignal, onSettled, useContext } from 'solid-js';
import { groupDownloads } from '../downloads';
import { formatBytes, plural } from '../format';
import { to } from '../links';
import { ServicesContext } from '../services';
import { Cover } from '../components/Cover';
import { ConfirmButton, EmptyNote, PageHeader } from '../components/Common';
import { Icon } from '../components/Icon';
import { ProgressRing } from '../components/DownloadState';

export default function Downloads() {
  const { downloads, history, player, ui } = useContext(ServicesContext);
  const [estimate, setEstimate] = createSignal<{ usage: number; quota: number } | null>(null);
  const groups = createMemo(() => groupDownloads(downloads.records()));
  const totalBytes = createMemo(() => downloads.records().reduce((sum, record) => sum + record.sizeBytes, 0));
  const inProgress = createMemo(() => [...downloads.progress().entries()]);

  const refreshEstimate = async () => setEstimate(await downloads.estimate());
  onSettled(() => {
    void downloads.refresh();
    void refreshEstimate();
  });

  async function after<T>(work: Promise<T>, message: string) {
    await work;
    await refreshEstimate();
    ui.toast(message);
  }

  return (
    <div class="page">
      <PageHeader
        title="Downloads"
        lede="Music, films and documents saved on this device to play without a connection."
      />

      <section class="storage" aria-label="Storage on this device">
        <div class="storage-figures">
          <p>
            <span class="figure">{formatBytes(totalBytes())}</span> in {plural(downloads.records().length, 'download')}
          </p>
          <Show when={estimate()}>
            {(value) => (
              <p class="muted">
                The Player is using {formatBytes(value().usage)} of this device
                <Show when={value().quota > 0}>
                  {' '}
                  (about {formatBytes(Math.max(0, value().quota - value().usage))} free for it)
                </Show>
                .
              </p>
            )}
          </Show>
        </div>
        <Show when={estimate() && (estimate()?.quota ?? 0) > 0}>
          <div
            class="meter"
            role="meter"
            aria-label="Storage used"
            aria-valuemin="0"
            aria-valuemax={String(estimate()?.quota ?? 0)}
            aria-valuenow={String(estimate()?.usage ?? 0)}
            style={{ '--fill': String(Math.min(1, (estimate()?.usage ?? 0) / Math.max(1, estimate()?.quota ?? 1))) }}
          >
            <span />
          </div>
        </Show>
      </section>

      <Show when={inProgress().length > 0}>
        <section>
          <h2 class="section-title">Downloading</h2>
          <ul class="track-list">
            <For each={inProgress()}>
              {([, progress]) => (
                <li class="track">
                  <span class="track-lead">
                    <ProgressRing fraction={progress.total > 0 ? progress.received / progress.total : 0} size={22} />
                  </span>
                  <span class="track-text">
                    <span class="track-title">{progress.file.title}</span>
                    <span class="track-sub">
                      {progress.file.itemTitle}: {formatBytes(progress.received)} of {formatBytes(progress.total)}
                    </span>
                  </span>
                </li>
              )}
            </For>
          </ul>
        </section>
      </Show>

      <Show
        when={groups().length > 0}
        fallback={
          <EmptyNote title="Nothing downloaded yet">
            <p>
              Use <strong>Download</strong> on an album, a playlist, or any track's menu. Downloads play even with no
              connection.
            </p>
          </EmptyNote>
        }
      >
        <For each={groups()}>
          {(group) => (
            <section class="download-group">
              <header class="download-group-head">
                <a href={to.item(group.itemId)} class="item-line">
                  <Cover src={group.cover} size="small" />
                  <span class="track-text">
                    <span class="track-title">{group.itemTitle}</span>
                    <span class="track-sub">
                      {group.creator}, {plural(group.records.length, 'file')}, {formatBytes(group.sizeBytes)}
                    </span>
                  </span>
                </a>
                <button
                  type="button"
                  class="icon-button"
                  aria-label={`Play ${group.itemTitle}`}
                  onClick={() => player.playFiles(group.records.map((record) => record.file))}
                >
                  <Icon name="play" />
                </button>
                <button
                  type="button"
                  class="icon-button"
                  aria-label={`Remove all downloads of ${group.itemTitle}`}
                  onClick={() =>
                    void after(downloads.removeItem(group.itemId), `Removed ${group.itemTitle} from this device.`)
                  }
                >
                  <Icon name="trash" />
                </button>
              </header>
              <ul class="track-list compact">
                <For each={group.records}>
                  {(record) => (
                    <li class="track">
                      <span class="track-text">
                        <span class="track-title">{record.file.title}</span>
                      </span>
                      <span class="track-length">{formatBytes(record.sizeBytes)}</span>
                      <button
                        type="button"
                        class="icon-button"
                        aria-label={`Remove download of ${record.file.title}`}
                        onClick={() => void after(downloads.remove(record.id), `Removed “${record.file.title}”.`)}
                      >
                        <Icon name="close" size={20} />
                      </button>
                    </li>
                  )}
                </For>
              </ul>
            </section>
          )}
        </For>
      </Show>

      <section class="danger-zone" aria-label="Free up space">
        <h2 class="section-title">Free up space</h2>
        <div class="danger-row">
          <div>
            <p class="danger-title">Clear all downloads</p>
            <p class="muted">Removes every downloaded file from this device. You can download them again any time.</p>
          </div>
          <ConfirmButton
            label="Clear all downloads"
            confirmLabel="Tap again to clear"
            disabled={downloads.records().length === 0}
            onConfirm={() => after(downloads.clearAll(), 'Cleared all downloads.')}
          />
        </div>
        <div class="danger-row">
          <div>
            <p class="danger-title">Clear other cached data</p>
            <p class="muted">Pictures and fonts saved while browsing. Downloads and the app itself stay.</p>
          </div>
          <ConfirmButton
            label="Clear cached data"
            confirmLabel="Tap again to clear"
            onConfirm={() => after(downloads.clearOtherCaches(), 'Cleared cached pictures and fonts.')}
          />
        </div>
        <div class="danger-row">
          <div>
            <p class="danger-title">Clear listen history</p>
            <p class="muted">Forgets what you've played on this device.</p>
          </div>
          <ConfirmButton
            label="Clear history"
            confirmLabel="Tap again to clear"
            onConfirm={() => after(history.clear(), 'Cleared your listen history.')}
          />
        </div>
      </section>
    </div>
  );
}
