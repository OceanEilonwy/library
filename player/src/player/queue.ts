// The play queue as a pure, immutable value.
//
// `entries` is the order things will play in. Each entry has a `key` unique
// within the queue, so the same track can be queued twice and still be told
// apart. While shuffled, `unshuffled` remembers the original order so turning
// shuffle off puts things back (keeping anything added or removed meanwhile).

export type RepeatMode = 'off' | 'all' | 'one';

export type QueueEntry<T> = { readonly key: number; readonly track: T };

export type Queue<T> = {
  readonly entries: readonly QueueEntry<T>[];
  /** Position of the current entry in `entries`, or -1 when nothing is loaded. */
  readonly index: number;
  readonly repeat: RepeatMode;
  readonly shuffled: boolean;
  /** Original order while shuffled; `null` otherwise. */
  readonly unshuffled: readonly QueueEntry<T>[] | null;
  readonly nextKey: number;
};

/** A source of random numbers in [0, 1), injectable for tests. */
export type Random = () => number;

export function emptyQueue<T>(): Queue<T> {
  return { entries: [], index: -1, repeat: 'off', shuffled: false, unshuffled: null, nextKey: 1 };
}

export function current<T>(queue: Queue<T>): QueueEntry<T> | undefined {
  return queue.entries[queue.index];
}

/** Entries after the current one, in play order. */
export function upNext<T>(queue: Queue<T>): readonly QueueEntry<T>[] {
  return queue.entries.slice(queue.index + 1);
}

function makeEntries<T>(tracks: readonly T[], firstKey: number): QueueEntry<T>[] {
  return tracks.map((track, offset) => ({ key: firstKey + offset, track }));
}

function shuffleInPlace<T>(items: T[], random: Random): T[] {
  for (let i = items.length - 1; i > 0; i -= 1) {
    const j = Math.floor(random() * (i + 1));
    const a = items[i] as T;
    items[i] = items[j] as T;
    items[j] = a;
  }
  return items;
}

/** Shuffled copy of `entries` with the entry at `keep` (if any) first. */
function shuffledWithFirst<T>(entries: readonly QueueEntry<T>[], keep: number, random: Random): QueueEntry<T>[] {
  const first = entries[keep];
  const rest = entries.filter((_, position) => position !== keep);
  shuffleInPlace(rest, random);
  return first ? [first, ...rest] : rest;
}

/**
 * Replaces the queue with `tracks`, starting at `start`. Repeat mode is kept;
 * if shuffle is on, the new list is shuffled with the chosen track first.
 */
export function playList<T>(queue: Queue<T>, tracks: readonly T[], start = 0, random: Random = Math.random): Queue<T> {
  if (tracks.length === 0)
    return { ...emptyQueue<T>(), repeat: queue.repeat, shuffled: queue.shuffled, nextKey: queue.nextKey };
  const startAt = Math.min(Math.max(0, Math.trunc(start)), tracks.length - 1);
  const entries = makeEntries(tracks, queue.nextKey);
  const nextKey = queue.nextKey + tracks.length;
  if (queue.shuffled) {
    return { ...queue, entries: shuffledWithFirst(entries, startAt, random), index: 0, unshuffled: entries, nextKey };
  }
  return { ...queue, entries, index: startAt, unshuffled: null, nextKey };
}

/** Inserts `tracks` right after the current entry (or starts playing them if the queue is empty). */
export function playNext<T>(queue: Queue<T>, tracks: readonly T[]): Queue<T> {
  if (tracks.length === 0) return queue;
  const added = makeEntries(tracks, queue.nextKey);
  const nextKey = queue.nextKey + tracks.length;
  if (queue.index < 0) {
    return {
      ...queue,
      entries: [...queue.entries, ...added],
      index: queue.entries.length,
      nextKey,
      unshuffled: queue.unshuffled ? [...queue.unshuffled, ...added] : null,
    };
  }
  const at = queue.index + 1;
  const entries = [...queue.entries.slice(0, at), ...added, ...queue.entries.slice(at)];
  let unshuffled = queue.unshuffled;
  if (unshuffled) {
    const currentKey = current(queue)?.key;
    const position = unshuffled.findIndex((entry) => entry.key === currentKey);
    unshuffled = [...unshuffled.slice(0, position + 1), ...added, ...unshuffled.slice(position + 1)];
  }
  return { ...queue, entries, nextKey, unshuffled };
}

/** Appends `tracks` to the end (or starts playing them if the queue is empty). */
export function addToEnd<T>(queue: Queue<T>, tracks: readonly T[]): Queue<T> {
  if (tracks.length === 0) return queue;
  const added = makeEntries(tracks, queue.nextKey);
  return {
    ...queue,
    entries: [...queue.entries, ...added],
    index: queue.index < 0 ? queue.entries.length : queue.index,
    unshuffled: queue.unshuffled ? [...queue.unshuffled, ...added] : null,
    nextKey: queue.nextKey + tracks.length,
  };
}

/**
 * Removes the entry with `key`. Removing the current entry makes the next one
 * current (or the previous one if it was last); the caller loads it.
 */
export function remove<T>(queue: Queue<T>, key: number): Queue<T> {
  const position = queue.entries.findIndex((entry) => entry.key === key);
  if (position < 0) return queue;
  const entries = queue.entries.filter((entry) => entry.key !== key);
  let index = queue.index;
  if (position < index) index -= 1;
  else if (position === index && index >= entries.length) index = entries.length - 1;
  return {
    ...queue,
    entries,
    index: entries.length === 0 ? -1 : index,
    unshuffled: queue.unshuffled ? queue.unshuffled.filter((entry) => entry.key !== key) : null,
  };
}

/** Moves the entry at position `from` to position `to` (both in play order). The current entry stays current. */
export function move<T>(queue: Queue<T>, from: number, to: number): Queue<T> {
  const count = queue.entries.length;
  if (from < 0 || from >= count || to < 0 || to >= count || from === to) return queue;
  const entries = [...queue.entries];
  const [moved] = entries.splice(from, 1);
  if (!moved) return queue;
  entries.splice(to, 0, moved);
  const currentKey = current(queue)?.key;
  const index = entries.findIndex((entry) => entry.key === currentKey);
  return { ...queue, entries, index };
}

/** Makes the entry at `position` current. */
export function jumpTo<T>(queue: Queue<T>, position: number): Queue<T> {
  if (position < 0 || position >= queue.entries.length) return queue;
  return { ...queue, index: position };
}

/** Turns shuffle on (current entry first, the rest shuffled) or off (original order restored). */
export function setShuffle<T>(queue: Queue<T>, on: boolean, random: Random = Math.random): Queue<T> {
  if (on === queue.shuffled) return queue;
  if (on) {
    const index = queue.index < 0 ? -1 : 0;
    return {
      ...queue,
      shuffled: true,
      unshuffled: queue.entries,
      entries: shuffledWithFirst(queue.entries, queue.index, random),
      index: queue.entries.length === 0 ? -1 : index,
    };
  }
  const restored = queue.unshuffled ?? queue.entries;
  const currentKey = current(queue)?.key;
  const index = restored.findIndex((entry) => entry.key === currentKey);
  return { ...queue, shuffled: false, unshuffled: null, entries: restored, index: restored.length === 0 ? -1 : index };
}

export function setRepeat<T>(queue: Queue<T>, repeat: RepeatMode): Queue<T> {
  return { ...queue, repeat };
}

/** off → all → one → off, as the repeat button cycles. */
export function nextRepeatMode(mode: RepeatMode): RepeatMode {
  switch (mode) {
    case 'off':
      return 'all';
    case 'all':
      return 'one';
    case 'one':
      return 'off';
  }
}

/** Why we are moving on: the track finished by itself, or the listener pressed "next". */
export type Advance = 'ended' | 'skip';

/**
 * The queue after moving forward, or `null` when playback should stop.
 * Repeat-one replays the current entry only when it ended by itself; a skip
 * still moves on. Repeat-all wraps round to the start.
 */
export function next<T>(queue: Queue<T>, reason: Advance): Queue<T> | null {
  if (queue.index < 0) return null;
  if (reason === 'ended' && queue.repeat === 'one') return queue;
  if (queue.index + 1 < queue.entries.length) return { ...queue, index: queue.index + 1 };
  return queue.repeat === 'all' ? { ...queue, index: 0 } : null;
}

/**
 * The queue after moving back, or `null` if there is nothing before (the
 * caller then restarts the current track). Repeat-all wraps to the end.
 * The "restart if more than 3 s in" rule lives in `shouldRestart`.
 */
export function previous<T>(queue: Queue<T>): Queue<T> | null {
  if (queue.index < 0) return null;
  if (queue.index > 0) return { ...queue, index: queue.index - 1 };
  if (queue.repeat === 'all' && queue.entries.length > 1) return { ...queue, index: queue.entries.length - 1 };
  return null;
}

/** Seconds into a track after which "previous" restarts it instead of going back. */
export const RESTART_THRESHOLD_SECONDS = 3;

export function shouldRestart(positionSeconds: number): boolean {
  return positionSeconds > RESTART_THRESHOLD_SECONDS;
}
