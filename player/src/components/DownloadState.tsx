import { Match, Switch, createSignal, useContext } from 'solid-js';
import type { MediaFile } from '../api';
import { plural } from '../format';
import { ServicesContext } from '../services';
import { Icon } from './Icon';

/** A ring showing how far a download has got. */
export function ProgressRing(props: { fraction: number; size?: number; label?: string }) {
  const size = () => props.size ?? 20;
  const radius = () => size() / 2 - 2;
  const circumference = () => 2 * Math.PI * radius();
  return (
    <svg
      class="progress-ring"
      width={size()}
      height={size()}
      viewBox={`0 0 ${size()} ${size()}`}
      role="img"
      aria-label={props.label ?? `${Math.round(props.fraction * 100)}% downloaded`}
    >
      <circle cx={size() / 2} cy={size() / 2} r={radius()} class="progress-ring-track" />
      <circle
        cx={size() / 2}
        cy={size() / 2}
        r={radius()}
        class="progress-ring-fill"
        stroke-dasharray={String(circumference())}
        stroke-dashoffset={String(circumference() * (1 - props.fraction))}
      />
    </svg>
  );
}

/** The small status mark in a track row: saved, downloading, or nothing. */
export function DownloadMark(props: { file: MediaFile }) {
  const { downloads } = useContext(ServicesContext);
  return (
    <Switch>
      <Match when={downloads.state(props.file.id) === 'done'}>
        <span class="download-mark done" title="Downloaded">
          <Icon name="downloaded" size={16} />
          <span class="visually-hidden">Downloaded</span>
        </span>
      </Match>
      <Match when={downloads.state(props.file.id) === 'downloading'}>
        <span class="download-mark">
          <ProgressRing fraction={downloads.fraction(props.file.id) ?? 0} size={16} />
        </span>
      </Match>
      <Match when={downloads.state(props.file.id) === 'queued'}>
        <span class="download-mark">
          <ProgressRing fraction={0} size={16} label="Waiting to download" />
        </span>
      </Match>
    </Switch>
  );
}

/**
 * One tap downloads everything in a list (an item or a playlist). When it's
 * all saved, a first tap asks "Remove downloads?" and a second removes them.
 */
export function DownloadAllButton(props: { files: readonly MediaFile[]; label?: string }) {
  const { downloads } = useContext(ServicesContext);
  const [confirming, setConfirming] = createSignal(false);
  let timer: ReturnType<typeof setTimeout> | undefined;

  const total = () => props.files.length;
  const saved = () => props.files.filter((file) => downloads.state(file.id) === 'done').length;
  const busy = () =>
    props.files.some((file) => {
      const state = downloads.state(file.id);
      return state === 'downloading' || state === 'queued';
    });
  const fraction = () => {
    let sum = 0;
    for (const file of props.files) {
      if (downloads.state(file.id) === 'done') sum += 1;
      else sum += downloads.fraction(file.id) ?? 0;
    }
    return total() > 0 ? sum / total() : 0;
  };

  function onClick() {
    if (busy()) return;
    if (saved() === total()) {
      if (confirming()) {
        clearTimeout(timer);
        setConfirming(false);
        for (const file of props.files) void downloads.remove(file.id);
        return;
      }
      setConfirming(true);
      timer = setTimeout(() => setConfirming(false), 4000);
      return;
    }
    downloads.download(props.files);
  }

  return (
    <button
      type="button"
      class={['pill', { 'pill-done': saved() === total() && total() > 0 }]}
      onClick={onClick}
      disabled={total() === 0}
      aria-live="polite"
    >
      <Switch
        fallback={
          <>
            <Icon name="download" size={20} /> {props.label ?? 'Download'}
          </>
        }
      >
        <Match when={busy()}>
          <ProgressRing fraction={fraction()} size={18} label={`Downloading, ${Math.round(fraction() * 100)}% done`} />
          {` Downloading ${saved()} of ${total()}`}
        </Match>
        <Match when={confirming()}>
          <Icon name="trash" size={20} /> Remove {plural(total(), 'download')}?
        </Match>
        <Match when={saved() === total() && total() > 0}>
          <Icon name="downloaded" size={20} /> Downloaded
        </Match>
      </Switch>
    </button>
  );
}
