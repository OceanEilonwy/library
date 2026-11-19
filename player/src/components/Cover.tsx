// Cover art, or a quiet placeholder showing what kind of thing it is.

import { Show, createSignal } from 'solid-js';
import type { MediaKind } from '../api';
import { Icon, type IconName } from './Icon';

const kindIcon: Record<MediaKind, IconName> = { audio: 'album', video: 'film', document: 'document' };

export function Cover(props: {
  src: string | null;
  kind?: MediaKind | undefined;
  size?: 'small' | 'medium' | 'large' | 'fill';
  alt?: string;
}) {
  const [failed, setFailed] = createSignal(false);
  return (
    <span class={['cover', `cover-${props.size ?? 'medium'}`]}>
      <Show
        when={props.src && !failed()}
        fallback={<Icon name={kindIcon[props.kind ?? 'audio']} class="cover-glyph" />}
      >
        <img
          src={props.src ?? ''}
          alt={props.alt ?? ''}
          loading="lazy"
          decoding="async"
          onError={() => setFailed(true)}
        />
      </Show>
    </span>
  );
}

/** Up to four covers in a 2×2 mosaic, for playlists. */
export function CoverMosaic(props: { covers: readonly string[]; size?: 'medium' | 'large' | 'fill' }) {
  return (
    <Show
      when={props.covers.length >= 4}
      fallback={<Cover src={props.covers[0] ?? null} size={props.size ?? 'medium'} />}
    >
      <span class={['cover', 'cover-mosaic', `cover-${props.size ?? 'medium'}`]}>
        {props.covers.slice(0, 4).map((src) => (
          <img src={src} alt="" loading="lazy" decoding="async" />
        ))}
      </span>
    </Show>
  );
}
