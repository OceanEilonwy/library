// The bar above the tabs while something is loaded: what's playing, a thin
// progress line, and previous / play-pause / next. Tap it to open Now Playing.

import { Show, useContext } from 'solid-js';
import { ServicesContext } from '../services';
import { Cover } from './Cover';
import { Icon } from './Icon';

export function MiniPlayer() {
  const { player, ui } = useContext(ServicesContext);
  const state = player.state;
  const fraction = () => {
    const { position, duration } = state();
    return duration > 0 ? Math.min(1, position / duration) : 0;
  };

  return (
    <Show when={state().current}>
      {(file) => (
        <section class="mini-player" aria-label="Player">
          <div class="mini-progress" style={{ '--progress': String(fraction()) }} aria-hidden="true" />
          <button
            type="button"
            class="mini-info"
            onClick={() => ui.setNowPlayingOpen(true)}
            aria-label={`Open Now Playing: ${file().title} by ${file().creator}`}
          >
            <Cover src={file().cover} size="small" />
            <span class="mini-text">
              <span class="mini-title">{file().title}</span>
              <span class="mini-sub">{state().error ?? file().creator}</span>
            </span>
          </button>
          <div class="mini-controls">
            <button
              type="button"
              class="icon-button hide-narrow"
              aria-label="Previous"
              onClick={() => player.engine.previous()}
            >
              <Icon name="previous" />
            </button>
            <button
              type="button"
              class="icon-button play-small"
              aria-label={state().playing ? 'Pause' : 'Play'}
              onClick={() => player.engine.togglePlay()}
            >
              <Icon name={state().playing ? 'pause' : 'play'} size={26} />
            </button>
            <button type="button" class="icon-button" aria-label="Next" onClick={() => player.engine.next()}>
              <Icon name="next" />
            </button>
          </div>
        </section>
      )}
    </Show>
  );
}
