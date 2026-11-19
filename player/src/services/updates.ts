// Background updates, applied only when nobody will notice (see ../update.ts).

import { createSignal } from 'solid-js';
import { shouldActivate, type UpdateStatus, type UpdateTrigger } from '../update';

/** The shape of `registerSW` from `virtual:pwa-register`. */
export type RegisterSW = (options: {
  immediate?: boolean;
  onNeedRefresh?: () => void;
  onOfflineReady?: () => void;
  onRegisteredSW?: (url: string, registration: ServiceWorkerRegistration | undefined) => void;
  onRegisterError?: (error: unknown) => void;
}) => (reloadPage?: boolean) => Promise<void>;

/** How often to look for a new version while the app stays open. */
const CHECK_EVERY_MS = 60 * 60 * 1000;

export function createUpdates(options: { busy: () => boolean; downloading: () => boolean }) {
  const supported = typeof navigator !== 'undefined' && 'serviceWorker' in navigator;
  const [status, setStatus] = createSignal<UpdateStatus>(supported ? 'checking' : 'unsupported');
  let registration: ServiceWorkerRegistration | undefined;
  let applyUpdate: ((reloadPage?: boolean) => Promise<void>) | undefined;
  let applying = false;

  return {
    status,

    /** Registers the service worker. Called once at start-up. */
    register(registerSW: RegisterSW): void {
      if (!supported) return;
      applyUpdate = registerSW({
        immediate: true,
        onNeedRefresh: () => setStatus('ready'),
        onOfflineReady: () => setStatus('current'),
        onRegisteredSW: (_url, reg) => {
          registration = reg;
          if (status() === 'checking') setStatus(reg?.waiting ? 'ready' : 'current');
          if (reg) setInterval(() => void reg.update().catch(() => {}), CHECK_EVERY_MS);
        },
        onRegisterError: () => setStatus('error'),
      });
    },

    /** Activates a waiting version now if this is a quiet moment. Returns true if it did. */
    maybeActivate(trigger: UpdateTrigger): boolean {
      if (applying || !applyUpdate) return false;
      const ok = shouldActivate({
        waiting: status() === 'ready',
        playing: options.busy(),
        downloading: options.downloading(),
        trigger,
      });
      if (!ok) return false;
      applying = true;
      void applyUpdate(true);
      return true;
    },

    /** In development the dev server serves fresh code and no service worker runs. */
    markDevelopment(): void {
      setStatus('current');
    },

    /** Looks for a new version straight away (from the settings screen). */
    async checkNow(): Promise<void> {
      if (!registration) return;
      setStatus('checking');
      try {
        await registration.update();
        setStatus(registration.waiting || registration.installing ? 'ready' : 'current');
      } catch {
        setStatus('error');
      }
    },
  };
}

export type Updates = ReturnType<typeof createUpdates>;
