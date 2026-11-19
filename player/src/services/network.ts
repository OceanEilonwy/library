// Are we online? Combines the browser's guess with what our own requests saw.

import { createSignal } from 'solid-js';

export function createNetwork(
  target: Pick<Window, 'addEventListener'> | null = typeof window === 'undefined' ? null : window,
) {
  const initial = typeof navigator !== 'undefined' ? !navigator.onLine : false;
  const [offline, setOffline] = createSignal(initial);
  target?.addEventListener('online', () => setOffline(false));
  target?.addEventListener('offline', () => setOffline(true));
  return {
    offline,
    /** Our request reached (false) or failed to reach (true) the server. */
    report(isOffline: boolean): void {
      if (isOffline !== offline()) setOffline(isOffline);
    },
  };
}

export type Network = ReturnType<typeof createNetwork>;
