// Who is signed in. Every screen except sign-in and registration needs an account.
//
// The last known account is remembered on the device so that, offline, the
// app still opens and plays downloads. A 401 from the API ends the session.

import { createSignal } from 'solid-js';
import { isApiError, type Account, type ApiClient, type RegistrationPending } from '../api';

export type SessionStatus = 'loading' | 'signed-in' | 'signed-out';

/** Where the last known account is remembered between launches. */
export type AccountMemory = {
  load(): Promise<Account | null>;
  save(account: Account | null): Promise<void>;
};

export function createSession(api: ApiClient, memory: AccountMemory) {
  const [status, setStatus] = createSignal<SessionStatus>('loading');
  const [account, setAccount] = createSignal<Account | null>(null);
  const [expired, setExpired] = createSignal(false);
  /** True when we are signed in from the remembered account because the server can't be reached. */
  const [fromMemory, setFromMemory] = createSignal(false);
  const signedInListeners = new Set<(account: Account) => void>();

  function signedIn(next: Account, remembered = false) {
    setAccount(next);
    setStatus('signed-in');
    setExpired(false);
    setFromMemory(remembered);
    if (!remembered) {
      void memory.save(next);
      for (const listener of signedInListeners) listener(next);
    }
  }

  function signedOut(wasExpired: boolean) {
    setAccount(null);
    setStatus('signed-out');
    setExpired(wasExpired);
    setFromMemory(false);
    void memory.save(null);
  }

  return {
    status,
    account,
    expired,
    fromMemory,

    /** Asks the server who we are; falls back to the remembered account when offline. */
    async start(): Promise<void> {
      try {
        signedIn(await api.me());
      } catch (error) {
        if (isApiError(error) && error.kind === 'offline') {
          const remembered = await memory.load();
          if (remembered) signedIn(remembered, true);
          else signedOut(false);
          return;
        }
        signedOut(false);
      }
    },

    async signIn(login: string, password: string): Promise<Account> {
      const result = await api.login({ login, password });
      signedIn(result);
      return result;
    },

    /** Asks for an account. It waits for a librarian, so this never signs in. */
    async register(username: string, email: string, password: string): Promise<RegistrationPending> {
      return api.register({ username, email, password });
    },

    async signOut(): Promise<void> {
      try {
        await api.logout();
      } finally {
        signedOut(false);
      }
    },

    /** The server said 401 to a request that needed an account. */
    expire(): void {
      if (status() === 'signed-in' || status() === 'loading') signedOut(true);
    },

    /** Calls `listener` whenever an account signs in (not when restored offline). */
    onSignedIn(listener: (account: Account) => void): () => void {
      signedInListeners.add(listener);
      return () => signedInListeners.delete(listener);
    },
  };
}

export type Session = ReturnType<typeof createSession>;
