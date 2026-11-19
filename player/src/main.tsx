import { render } from '@solidjs/web';
import { registerSW } from 'virtual:pwa-register';
import { Router } from './router';
import { Shell } from './Shell';
import { ServicesContext, createBrowserServices } from './services';
import './styles.css';

// The site's fonts live outside the Player (at /static), so Vite must not
// rewrite this URL under /player/; adding the link here keeps it as written.
const fonts = document.createElement('link');
fonts.rel = 'stylesheet';
fonts.href = '/static/fonts/fonts.css';
document.head.append(fonts);

const root = document.getElementById('app');
const audio = document.getElementById('player-audio');

if (root && audio instanceof HTMLAudioElement) {
  const services = createBrowserServices(audio);
  services.player.restore();
  void services.downloads.refresh();
  void services.session.start().then(() => {
    // Restored offline from the remembered account: show remembered favourites too.
    if (services.session.fromMemory()) void services.favourites.load();
  });
  if (import.meta.env.PROD) services.updates.register(registerSW);
  else services.updates.markDevelopment();

  render(
    () => (
      <ServicesContext value={services}>
        <Router>{(props) => <Shell>{props.children}</Shell>}</Router>
      </ServicesContext>
    ),
    root,
  );
}
