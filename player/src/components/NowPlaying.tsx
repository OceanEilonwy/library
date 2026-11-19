// The full player: the record and its sleeve, the scrub bar, the transport
// controls, and an "Up next" tab with the queue.

import { Match, Show, Switch, createSignal, onSettled, useContext } from 'solid-js';
import { useNavigate } from '@solidjs/router';
import { to } from '../links';
import { ServicesContext } from '../services';
import { Cover } from './Cover';
import { HeartButton } from './HeartButton';
import { Icon } from './Icon';
import { Menu } from './Menu';
import { ScrubBar } from './ScrubBar';
import { trackMenuItems } from './TrackMenu';
import { UpNext } from './UpNext';
import { trapTab } from './focus';

type Tab = 'playing' | 'queue';

export function NowPlaying() {
  const services = useContext(ServicesContext);
  const { player, ui } = services;
  const navigate = useNavigate();
  const [tab, setTab] = createSignal<Tab>('playing');
  const state = player.state;
  let closeButton: HTMLButtonElement | undefined;
  let dialog: HTMLDivElement | undefined;

  const close = () => ui.setNowPlayingOpen(false);
  const repeatLabel = () => {
    switch (state().queue.repeat) {
      case 'off':
        return 'Repeat is off';
      case 'all':
        return 'Repeating all';
      case 'one':
        return 'Repeating this track';
    }
  };

  onSettled(() => {
    const previous = document.activeElement as HTMLElement | null;
    closeButton?.focus();
    const onKey = (event: KeyboardEvent) => {
      if (document.querySelector('.menu')) return;
      if (event.key === 'Escape') close();
      trapTab(dialog, event);
    };
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('keydown', onKey);
      previous?.focus?.();
    };
  });

  return (
    <div class="now-playing" role="dialog" aria-modal="true" aria-label="Now playing" ref={(el) => (dialog = el)}>
      <header class="now-playing-bar">
        <button
          ref={(el) => (closeButton = el)}
          type="button"
          class="icon-button"
          aria-label="Close Now Playing"
          onClick={close}
        >
          <Icon name="chevronDown" size={28} />
        </button>
        <div class="tabs" role="tablist" aria-label="Now playing views">
          <button
            type="button"
            role="tab"
            id="tab-playing"
            aria-selected={tab() === 'playing' ? 'true' : 'false'}
            aria-controls="panel-playing"
            onClick={() => setTab('playing')}
          >
            Now playing
          </button>
          <button
            type="button"
            role="tab"
            id="tab-queue"
            aria-selected={tab() === 'queue' ? 'true' : 'false'}
            aria-controls="panel-queue"
            onClick={() => setTab('queue')}
          >
            Up next
          </button>
        </div>
        <span class="now-playing-spacer" />
      </header>

      <Switch>
        <Match when={tab() === 'queue'}>
          <div class="now-playing-panel" role="tabpanel" id="panel-queue" aria-labelledby="tab-queue">
            <UpNext />
          </div>
        </Match>
        <Match when={state().current}>
          {(file) => (
            <div class="now-playing-panel stage" role="tabpanel" id="panel-playing" aria-labelledby="tab-playing">
              <div class={['record', { spinning: state().playing }]}>
                <div class="record-disc" aria-hidden="true">
                  <div class="record-label">
                    <Cover src={file().cover} size="fill" />
                  </div>
                </div>
                <div class="record-sleeve">
                  <Cover src={file().cover} size="fill" alt={`Cover of ${file().itemTitle}`} />
                </div>
              </div>

              <div class="now-meta">
                <div class="now-titles">
                  <h2 class="now-title">{file().title}</h2>
                  <p class="now-sub">
                    <a href={to.item(file().itemId)} onClick={close}>
                      {file().creator}
                      <Show when={file().itemTitle}> — {file().itemTitle}</Show>
                    </a>
                  </p>
                </div>
                <HeartButton file={file()} size={26} />
                <Menu
                  label={`More options for “${file().title}”`}
                  items={trackMenuItems(services, file(), {
                    queueActions: false,
                    goToItem: () => {
                      close();
                      navigate(to.item(file().itemId));
                    },
                  })}
                />
              </div>

              <ScrubBar
                position={state().position}
                duration={state().duration}
                onSeek={(seconds) => player.engine.seek(seconds)}
              />

              <Show when={state().error}>
                <p class="player-error" role="alert">
                  {state().error}
                </p>
              </Show>

              <div class="transport">
                <button
                  type="button"
                  class={['icon-button', 'toggle', { on: state().queue.shuffled }]}
                  aria-pressed={state().queue.shuffled ? 'true' : 'false'}
                  aria-label="Shuffle"
                  onClick={() => player.engine.toggleShuffle()}
                >
                  <Icon name="shuffle" />
                </button>
                <button
                  type="button"
                  class="icon-button"
                  aria-label="Back 10 seconds"
                  onClick={() => player.engine.seekBy(-10)}
                >
                  <Icon name="back10" size={28} />
                </button>
                <button
                  type="button"
                  class="icon-button"
                  aria-label="Previous"
                  onClick={() => player.engine.previous()}
                >
                  <Icon name="previous" size={30} />
                </button>
                <button
                  type="button"
                  class={['play-big', { buffering: state().buffering }]}
                  aria-label={state().playing ? 'Pause' : 'Play'}
                  onClick={() => player.engine.togglePlay()}
                >
                  <Icon name={state().playing ? 'pause' : 'play'} size={34} />
                </button>
                <button type="button" class="icon-button" aria-label="Next" onClick={() => player.engine.next()}>
                  <Icon name="next" size={30} />
                </button>
                <button
                  type="button"
                  class="icon-button"
                  aria-label="Forward 10 seconds"
                  onClick={() => player.engine.seekBy(10)}
                >
                  <Icon name="forward10" size={28} />
                </button>
                <button
                  type="button"
                  class={['icon-button', 'toggle', { on: state().queue.repeat !== 'off' }]}
                  aria-label={repeatLabel()}
                  onClick={() => player.engine.cycleRepeat()}
                >
                  <Icon name={state().queue.repeat === 'one' ? 'repeatOne' : 'repeat'} />
                </button>
              </div>
            </div>
          )}
        </Match>
      </Switch>
    </div>
  );
}
