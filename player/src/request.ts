// Loading data for a screen: a tiny wrapper that keeps the last answer,
// the error (if any) and a loading flag in signals, refetching whenever the
// source changes. Screens show their own loading and error states with it.

import { createEffect, createSignal, untrack } from 'solid-js';

export type Request<T> = {
  data: () => T | undefined;
  error: () => unknown;
  loading: () => boolean;
  /** Fetches again with the current source. */
  reload: () => void;
  /** Replaces the data locally (after a successful change). */
  mutate: (value: T | undefined) => void;
};

/**
 * Runs `fetcher(source())` now and whenever `source` changes. A source of
 * `false`, `null` or `undefined` means "don't fetch yet".
 */
export function createRequest<S, T>(
  source: () => S | false | null | undefined,
  fetcher: (source: S) => Promise<T>,
): Request<T> {
  const [data, setData] = createSignal<T | undefined>(undefined, { ownedWrite: true });
  const [error, setError] = createSignal<unknown>(null, { ownedWrite: true });
  const [loading, setLoading] = createSignal(true, { ownedWrite: true });
  let latest = 0;

  function run(value: S | false | null | undefined) {
    if (value === false || value === null || value === undefined) return;
    latest += 1;
    const ticket = latest;
    setLoading(true);
    setError(null);
    fetcher(value).then(
      (result) => {
        if (ticket !== latest) return;
        setData(() => result);
        setLoading(false);
      },
      (failure: unknown) => {
        if (ticket !== latest) return;
        setError(() => failure);
        setLoading(false);
      },
    );
  }

  createEffect(source, (value) => run(value));

  return {
    data,
    error,
    loading,
    reload: () => run(untrack(source)),
    mutate: (value) => setData(() => value),
  };
}
