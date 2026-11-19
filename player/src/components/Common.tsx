// Small shared pieces: page headings, loading / error / empty notes,
// a two-tap confirm button, and the cards used on shelves and grids.

import { Show, createSignal, useContext, type ParentProps } from 'solid-js';
import type { JSX } from '@solidjs/web';
import { errorMessage, isApiError, type ItemSummary, type PlaylistSummary } from '../api';
import { formatSpan, plural } from '../format';
import { to } from '../links';
import { ServicesContext } from '../services';
import { Cover, CoverMosaic } from './Cover';
import { Icon } from './Icon';

export function PageHeader(props: ParentProps<{ title: string; lede?: string | undefined }>) {
  return (
    <header class="page-header">
      <div>
        <h1>{props.title}</h1>
        <Show when={props.lede}>
          <p class="lede">{props.lede}</p>
        </Show>
      </div>
      <Show when={props.children}>
        <div class="page-actions">{props.children}</div>
      </Show>
    </header>
  );
}

export function LoadingNote(props: { label?: string }) {
  return (
    <p class="loading-note" role="status">
      <span class="loading-dot" aria-hidden="true" />
      {props.label ?? 'Loading…'}
    </p>
  );
}

export function ErrorNote(props: { error: unknown; onRetry?: () => void }) {
  const { network } = useContext(ServicesContext);
  const message = () => {
    if (isApiError(props.error) && props.error.kind === 'offline') {
      return "You're offline, so this can't load. Your downloads still play — find them under Downloads.";
    }
    return errorMessage(props.error);
  };
  return (
    <div class="error-note" role="alert">
      <p>{message()}</p>
      <div class="row-actions">
        <Show when={props.onRetry && !network.offline()}>
          <button type="button" class="button" onClick={() => props.onRetry?.()}>
            Try again
          </button>
        </Show>
        <Show when={isApiError(props.error) && props.error.kind === 'offline'}>
          <a class="button ghost" href={to.downloads()}>
            Go to Downloads
          </a>
        </Show>
      </div>
    </div>
  );
}

export function EmptyNote(props: ParentProps<{ title: string }>) {
  return (
    <div class="empty">
      <p class="empty-title">{props.title}</p>
      <Show when={props.children}>
        <div class="empty-body">{props.children}</div>
      </Show>
    </div>
  );
}

/** A button that asks for a second tap before doing something it can't undo. */
export function ConfirmButton(props: {
  label: string;
  confirmLabel: string;
  onConfirm: () => void | Promise<void>;
  icon?: Parameters<typeof Icon>[0]['name'];
  class?: string;
  disabled?: boolean;
}) {
  const [armed, setArmed] = createSignal(false);
  let timer: ReturnType<typeof setTimeout> | undefined;
  return (
    <button
      type="button"
      class={['button', 'danger', props.class, { armed: armed() }]}
      disabled={props.disabled}
      onClick={() => {
        if (armed()) {
          clearTimeout(timer);
          setArmed(false);
          void props.onConfirm();
          return;
        }
        setArmed(true);
        timer = setTimeout(() => setArmed(false), 4000);
      }}
      onBlur={() => setArmed(false)}
    >
      <Show when={props.icon}>{(icon) => <Icon name={icon()} size={20} />}</Show>
      {armed() ? props.confirmLabel : props.label}
    </button>
  );
}

export function Shelf(props: ParentProps<{ title: string; href?: string; linkLabel?: string }>) {
  return (
    <section class="shelf">
      <div class="shelf-head">
        <h2>{props.title}</h2>
        <Show when={props.href}>
          <a class="text-button" href={props.href}>
            {props.linkLabel ?? 'See all'}
          </a>
        </Show>
      </div>
      <div class="shelf-row">{props.children}</div>
    </section>
  );
}

export function ItemCard(props: { item: ItemSummary }) {
  const kind = () => props.item.kinds[0];
  return (
    <a class="card" href={to.item(props.item.id)}>
      <Cover src={props.item.cover} kind={kind()} size="fill" />
      <span class="card-title">{props.item.title}</span>
      <span class="card-sub">
        {props.item.creator}
        <Show when={props.item.year}> ({props.item.year})</Show>
      </span>
      <Show when={props.item.restricted}>
        <span class="badge">Adults only</span>
      </Show>
    </a>
  );
}

export function PlaylistCard(props: { playlist: PlaylistSummary }) {
  return (
    <a class="card" href={to.playlist(props.playlist.id)}>
      <CoverMosaic covers={props.playlist.covers} size="fill" />
      <span class="card-title">{props.playlist.name}</span>
      <span class="card-sub">
        {props.playlist.mine ? 'Yours' : `By ${props.playlist.owner}`},{' '}
        {plural(props.playlist.entryCount, 'entry', 'entries')}
        <Show when={formatSpan(props.playlist.totalDurationMs)}>, {formatSpan(props.playlist.totalDurationMs)}</Show>
      </span>
    </a>
  );
}

export function OfflineBanner(props: { children?: JSX.Element }) {
  const { network } = useContext(ServicesContext);
  return (
    <Show when={network.offline()}>
      <div class="offline-banner" role="status">
        <Icon name="offline" size={20} />
        <span>{props.children ?? "You're offline. Downloaded music and films still play."}</span>
      </div>
    </Show>
  );
}
