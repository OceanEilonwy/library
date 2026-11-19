// "Install app": the browser's install prompt where offered, a hint on iPhone and iPad.

import { createSignal } from 'solid-js';

/** Chromium's install prompt event (not in the DOM typings). */
export type InstallPromptEvent = Event & {
  prompt(): Promise<void>;
  userChoice: Promise<{ outcome: 'accepted' | 'dismissed' }>;
};

export function isIos(userAgent: string, maxTouchPoints = 0): boolean {
  return /iPad|iPhone|iPod/.test(userAgent) || (/Macintosh/.test(userAgent) && maxTouchPoints > 1);
}

export function createInstall(win: Window | null = typeof window === 'undefined' ? null : window) {
  const standalone =
    !!win &&
    (win.matchMedia?.('(display-mode: standalone)').matches ||
      (win.navigator as Navigator & { standalone?: boolean }).standalone === true);
  const [promptEvent, setPromptEvent] = createSignal<InstallPromptEvent | null>(null);
  const [installed, setInstalled] = createSignal(standalone);
  const ios = !!win && isIos(win.navigator.userAgent, win.navigator.maxTouchPoints);

  win?.addEventListener('beforeinstallprompt', (event) => {
    event.preventDefault();
    setPromptEvent(event as InstallPromptEvent);
  });
  win?.addEventListener('appinstalled', () => {
    setInstalled(true);
    setPromptEvent(null);
  });

  return {
    installed,
    canPrompt: () => promptEvent() !== null && !installed(),
    /** Show the "Add to Home Screen" hint instead of a button. */
    showIosHint: () => ios && !installed(),
    async install(): Promise<void> {
      const event = promptEvent();
      if (!event) return;
      await event.prompt();
      const choice = await event.userChoice;
      if (choice.outcome === 'accepted') setInstalled(true);
      setPromptEvent(null);
    },
  };
}

export type Install = ReturnType<typeof createInstall>;
