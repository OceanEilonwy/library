// Seamless updates: when may a waiting service worker take over?
//
// A new version installs in the background. We switch to it (and reload)
// only at a moment the listener won't notice: nothing is playing, no
// download is running, and they are moving between screens or have just
// hidden the tab. Otherwise the browser activates it on the next launch.

/** What prompted the check. */
export type UpdateTrigger = 'navigation' | 'hidden' | 'idle-check';

export type UpdateContext = {
  /** A new service worker has installed and is waiting. */
  waiting: boolean;
  /** Music or a film is playing right now. */
  playing: boolean;
  /** A download is in progress (a reload would cancel it). */
  downloading: boolean;
  trigger: UpdateTrigger;
};

/** True when it is safe to activate the waiting worker and reload now. */
export function shouldActivate(context: UpdateContext): boolean {
  if (!context.waiting || context.playing || context.downloading) return false;
  switch (context.trigger) {
    case 'navigation':
    case 'hidden':
      return true;
    case 'idle-check':
      return false;
  }
}

/** What the settings screen says about updates. */
export type UpdateStatus = 'unsupported' | 'checking' | 'current' | 'ready' | 'error';

export function updateStatusText(status: UpdateStatus): string {
  switch (status) {
    case 'unsupported':
      return "This browser can't keep the Player up to date in the background.";
    case 'checking':
      return 'Checking for a new version…';
    case 'current':
      return "You're on the latest version.";
    case 'ready':
      return 'A new version is ready. It will switch over when nothing is playing.';
    case 'error':
      return "Couldn't check for a new version just now.";
  }
}
