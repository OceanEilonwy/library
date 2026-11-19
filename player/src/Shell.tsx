// The frame around every screen: navigation (bottom tabs on phones, a rail
// on wide screens), the mini player, Now Playing, sheets and toasts. Anyone
// not signed in sees the account screen instead.

import { Match, Show, Switch, createEffect, onSettled, untrack, useContext, type ParentProps } from 'solid-js';
import { useLocation } from '@solidjs/router';
import { handleShortcut } from './player/keyboard';
import { to, BASE } from './links';
import { ServicesContext } from './services';
import { Icon, type IconName } from './components/Icon';
import { MiniPlayer } from './components/MiniPlayer';
import { NowPlaying } from './components/NowPlaying';
import { PlaylistSheet } from './components/PlaylistSheet';
import { Toasts } from './components/Toasts';
import { AccountGate } from './pages/Account';

type NavItem = { href: string; label: string; icon: IconName; phone: boolean };

const nav: NavItem[] = [
  { href: to.home(), label: 'Home', icon: 'home', phone: true },
  { href: to.library(), label: 'Library', icon: 'library', phone: true },
  { href: to.playlists(), label: 'Playlists', icon: 'playlists', phone: true },
  { href: to.favourites(), label: 'Favourites', icon: 'heart', phone: false },
  { href: to.history(), label: 'History', icon: 'history', phone: false },
  { href: to.downloads(), label: 'Downloads', icon: 'downloads', phone: true },
  { href: to.settings(), label: 'You', icon: 'account', phone: true },
];

export function Shell(props: ParentProps) {
  const { session, player, ui, updates, network } = useContext(ServicesContext);
  const location = useLocation();

  // Moving between screens: close Now Playing, and maybe apply a waiting update.
  createEffect(
    () => location.pathname,
    () => {
      ui.setNowPlayingOpen(false);
      // A one-off check of the current state, not something to track.
      untrack(() => updates.maybeActivate('navigation'));
    },
    { defer: true },
  );

  onSettled(() => {
    const onKey = (event: KeyboardEvent) => {
      if (session.status() !== 'signed-in' || location.pathname.startsWith(`${BASE}/video/`)) return;
      if (ui.playlistSheet()) return;
      const handled = handleShortcut(event, {
        togglePlay: () => player.engine.togglePlay(),
        seekBy: (seconds) => player.engine.seekBy(seconds),
        next: () => player.engine.next(),
        previous: () => player.engine.previous(),
      });
      if (handled) event.preventDefault();
    };
    const onVisibility = () => {
      if (document.visibilityState === 'hidden') updates.maybeActivate('hidden');
    };
    document.addEventListener('keydown', onKey);
    document.addEventListener('visibilitychange', onVisibility);
    return () => {
      document.removeEventListener('keydown', onKey);
      document.removeEventListener('visibilitychange', onVisibility);
    };
  });

  return (
    <Switch>
      <Match when={session.status() === 'loading'}>
        <div class="splash" role="status">
          <span class="brand-mark">
            <Icon name="album" size={40} />
          </span>
          <span class="visually-hidden">Opening the Player…</span>
        </div>
      </Match>
      <Match when={session.status() === 'signed-out'}>
        <AccountGate initial={location.pathname.endsWith('/register') ? 'register' : 'sign-in'} />
        <Toasts />
      </Match>
      <Match when={session.status() === 'signed-in'}>
        <div class={['app', { 'has-player': !!player.current() }]}>
          <a class="skip-link" href="#main">
            Skip to content
          </a>
          <nav class="nav" aria-label="Main">
            <a class="nav-brand" href={to.home()} aria-label="Library Player home">
              <span class="brand-mark">
                <Icon name="album" size={26} />
              </span>
              <span class="nav-brand-name">Library Player</span>
            </a>
            <ul>
              {nav.map((item) => (
                <li class={{ 'rail-only': !item.phone }}>
                  <a href={item.href}>
                    <Icon name={item.icon} />
                    <span>{item.label}</span>
                  </a>
                </li>
              ))}
            </ul>
            <Show when={network.offline()}>
              <p class="nav-offline">
                <Icon name="offline" size={18} /> Offline
              </p>
            </Show>
          </nav>
          <main class="main" id="main" tabindex="-1">
            {props.children}
          </main>
          <MiniPlayer />
          <Show when={ui.nowPlayingOpen() && player.current()}>
            <NowPlaying />
          </Show>
          <Show when={ui.playlistSheet()}>
            <PlaylistSheet />
          </Show>
          <Toasts />
        </div>
      </Match>
    </Switch>
  );
}
