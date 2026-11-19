import { Show, useContext } from 'solid-js';
import { useNavigate } from '@solidjs/router';
import { to } from '../links';
import { updateStatusText } from '../update';
import { ServicesContext } from '../services';
import { PageHeader } from '../components/Common';
import { Icon } from '../components/Icon';

const buildDate = new Date(__BUILD_TIME__).toLocaleDateString('en-GB', {
  day: 'numeric',
  month: 'long',
  year: 'numeric',
});

export default function Settings() {
  const { session, updates, install, network } = useContext(ServicesContext);
  const navigate = useNavigate();

  async function signOut() {
    try {
      await session.signOut();
    } finally {
      navigate(to.home(), { replace: true });
    }
  }

  return (
    <div class="page settings">
      <PageHeader title="You" />

      <section class="settings-group">
        <h2 class="section-title">Account</h2>
        <Show when={session.account()}>
          {(account) => (
            <div class="settings-row">
              <span class="settings-icon">
                <Icon name="account" />
              </span>
              <div>
                <p class="settings-title">{account().username}</p>
                <p class="muted">{account().email}</p>
                <p class="muted">
                  {account().access === 'adult'
                    ? 'Adult account: you can open everything in the library.'
                    : 'Child account: items marked for adults are hidden. The librarian can change this.'}
                </p>
              </div>
              <button type="button" class="button ghost" onClick={() => void signOut()} disabled={network.offline()}>
                Sign out
              </button>
            </div>
          )}
        </Show>
      </section>

      <section class="settings-group">
        <h2 class="section-title">Your things</h2>
        <nav class="settings-links" aria-label="Your things">
          <a href={to.favourites()}>
            <Icon name="heart" /> Favourites
          </a>
          <a href={to.history()}>
            <Icon name="history" /> History
          </a>
          <a href={to.downloads()}>
            <Icon name="downloads" /> Downloads and storage
          </a>
        </nav>
      </section>

      <section class="settings-group">
        <h2 class="section-title">This app</h2>
        <div class="settings-row">
          <span class="settings-icon">
            <Icon name="album" />
          </span>
          <div>
            <p class="settings-title">Library Player {__APP_VERSION__}</p>
            <p class="muted">Built {buildDate}.</p>
            <p class="muted" aria-live="polite">
              {updateStatusText(updates.status())}
            </p>
          </div>
          <Show when={updates.status() !== 'unsupported'}>
            <button
              type="button"
              class="button ghost"
              disabled={updates.status() === 'checking' || network.offline()}
              onClick={() => void updates.checkNow()}
            >
              Check for updates
            </button>
          </Show>
        </div>
        <Show when={install.canPrompt()}>
          <div class="settings-row">
            <span class="settings-icon">
              <Icon name="install" />
            </span>
            <div>
              <p class="settings-title">Install the Player</p>
              <p class="muted">Open it from your home screen or dock, like any other app.</p>
            </div>
            <button type="button" class="button primary" onClick={() => void install.install()}>
              Install app
            </button>
          </div>
        </Show>
        <Show when={install.showIosHint()}>
          <div class="settings-row">
            <span class="settings-icon">
              <Icon name="share" />
            </span>
            <div>
              <p class="settings-title">Install on iPhone or iPad</p>
              <p class="muted">In Safari, tap the Share button, then “Add to Home Screen”.</p>
            </div>
          </div>
        </Show>
      </section>

      <section class="settings-group hide-touch">
        <h2 class="section-title">Keyboard shortcuts</h2>
        <dl class="shortcuts">
          <dt>
            <kbd>Space</kbd>
          </dt>
          <dd>Play or pause</dd>
          <dt>
            <kbd>←</kbd> <kbd>→</kbd>
          </dt>
          <dd>Back or forward 10 seconds</dd>
          <dt>
            <kbd>Shift</kbd> + <kbd>N</kbd>
          </dt>
          <dd>Next track</dd>
          <dt>
            <kbd>Shift</kbd> + <kbd>P</kbd>
          </dt>
          <dd>Previous track</dd>
        </dl>
      </section>

      <p class="settings-foot">
        <a href="/" target="_self">
          Back to the catalogue
        </a>
      </p>
    </div>
  );
}
