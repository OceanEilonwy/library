// The music player: one persistent audio element driven by the pure queue.
//
// The engine owns the element and the queue and tells subscribers whenever
// anything they might show changes. It knows nothing about the UI framework,
// so tests drive it with a fake element.

import type { MediaFile } from '../api';
import { LISTEN_THRESHOLD_SECONDS } from '../history';
import * as Q from './queue';

/** The parts of an `HTMLMediaElement` the engine uses. */
export type MediaElementLike = {
  src: string;
  currentTime: number;
  readonly duration: number;
  readonly paused: boolean;
  play(): Promise<void>;
  pause(): void;
  load(): void;
  removeAttribute(name: string): void;
  addEventListener(type: string, listener: (event: Event) => void): void;
  removeEventListener(type: string, listener: (event: Event) => void): void;
};

export type PlayerState = {
  readonly queue: Q.Queue<MediaFile>;
  readonly current: MediaFile | null;
  readonly playing: boolean;
  readonly buffering: boolean;
  /** Seconds into the current track. */
  readonly position: number;
  /** Length of the current track in seconds (0 when unknown). */
  readonly duration: number;
  readonly error: string | null;
};

/** What we save so the queue survives a reload (for instance after an update). */
export type PlayerSnapshot = {
  tracks: MediaFile[];
  index: number;
  repeat: Q.RepeatMode;
  shuffled: boolean;
  position: number;
};

export type EngineOptions = {
  /** Called once per load when a track has been listened to (10 s, or it finished). */
  onListen?: (file: MediaFile) => void;
  random?: Q.Random;
};

/** Seconds that "back" and "forward" jump. */
export const SKIP_SECONDS = 10;

const initialState: PlayerState = {
  queue: Q.emptyQueue<MediaFile>(),
  current: null,
  playing: false,
  buffering: false,
  position: 0,
  duration: 0,
  error: null,
};

export class PlayerEngine {
  #element: MediaElementLike;
  #options: EngineOptions;
  #state: PlayerState = initialState;
  #listeners = new Set<(state: PlayerState) => void>();
  #loadedKey: number | null = null;
  #listened = 0;
  #lastTime = 0;
  #listenRecorded = false;
  #pendingSeek: number | null = null;
  #unbind: () => void;

  constructor(element: MediaElementLike, options: EngineOptions = {}) {
    this.#element = element;
    this.#options = options;
    const handlers: Record<string, () => void> = {
      play: () => this.#set({ playing: true, error: null }),
      playing: () => this.#set({ playing: true, buffering: false }),
      pause: () => this.#set({ playing: false }),
      waiting: () => this.#set({ buffering: true }),
      canplay: () => this.#set({ buffering: false }),
      timeupdate: () => this.#onTimeUpdate(),
      durationchange: () => this.#set({ duration: this.#duration() }),
      loadedmetadata: () => this.#onLoadedMetadata(),
      ended: () => this.#onEnded(),
      error: () => this.#onError(),
    };
    for (const [type, handler] of Object.entries(handlers)) element.addEventListener(type, handler);
    this.#unbind = () => {
      for (const [type, handler] of Object.entries(handlers)) element.removeEventListener(type, handler);
    };
  }

  /** The current state. */
  get state(): PlayerState {
    return this.#state;
  }

  /** Calls `listener` on every change; returns a function that stops it. */
  subscribe(listener: (state: PlayerState) => void): () => void {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  /** Detaches from the element. */
  destroy(): void {
    this.#unbind();
    this.#listeners.clear();
  }

  // Queue commands ------------------------------------------------------

  /** Plays `files` from `start`, replacing the queue. */
  playList(files: readonly MediaFile[], start = 0): void {
    this.#applyQueue(Q.playList(this.#state.queue, files, start, this.#options.random), true);
  }

  playNext(files: readonly MediaFile[]): void {
    const wasEmpty = this.#state.queue.index < 0;
    this.#applyQueue(Q.playNext(this.#state.queue, files), wasEmpty);
  }

  addToQueue(files: readonly MediaFile[]): void {
    const wasEmpty = this.#state.queue.index < 0;
    this.#applyQueue(Q.addToEnd(this.#state.queue, files), wasEmpty);
  }

  /** Removes the queue entry with `key`. */
  removeEntry(key: number): void {
    this.#applyQueue(Q.remove(this.#state.queue, key), this.#state.playing);
  }

  /** Moves an entry from one play-order position to another. */
  moveEntry(from: number, to: number): void {
    this.#applyQueue(Q.move(this.#state.queue, from, to), this.#state.playing);
  }

  /** Plays the entry at a play-order position. */
  jumpTo(position: number): void {
    this.#applyQueue(Q.jumpTo(this.#state.queue, position), true, true);
  }

  toggleShuffle(): void {
    this.#applyQueue(
      Q.setShuffle(this.#state.queue, !this.#state.queue.shuffled, this.#options.random),
      this.#state.playing,
    );
  }

  cycleRepeat(): void {
    this.#applyQueue(Q.setRepeat(this.#state.queue, Q.nextRepeatMode(this.#state.queue.repeat)), this.#state.playing);
  }

  /** Empties the queue and stops. */
  clear(): void {
    this.#applyQueue(Q.playList(this.#state.queue, []), false);
  }

  // Transport -----------------------------------------------------------

  play(): void {
    if (!this.#state.current) return;
    this.#startPlayback();
  }

  pause(): void {
    this.#element.pause();
  }

  togglePlay(): void {
    if (this.#state.playing) this.pause();
    else this.play();
  }

  /** Pauses and goes back to the start of the track. */
  stop(): void {
    this.#element.pause();
    this.seek(0);
  }

  /** Jumps to `seconds` into the current track (clamped to the track). */
  seek(seconds: number): void {
    if (!this.#state.current) return;
    const duration = this.#duration();
    const clamped = Math.max(0, duration > 0 ? Math.min(seconds, duration) : seconds);
    this.#element.currentTime = clamped;
    this.#lastTime = clamped;
    this.#set({ position: clamped });
  }

  seekBy(seconds: number): void {
    this.seek(this.#element.currentTime + seconds);
  }

  /** Skips to the next track (repeat-one does not hold a skip back). */
  next(): void {
    const advanced = Q.next(this.#state.queue, 'skip');
    if (advanced) this.#applyQueue(advanced, true, true);
  }

  /** Restarts the track if more than 3 s in, otherwise goes to the previous one. */
  previous(): void {
    if (Q.shouldRestart(this.#element.currentTime)) {
      this.seek(0);
      return;
    }
    const back = Q.previous(this.#state.queue);
    if (back) this.#applyQueue(back, true, true);
    else this.seek(0);
  }

  // Persistence ---------------------------------------------------------

  snapshot(): PlayerSnapshot | null {
    const { queue } = this.#state;
    if (queue.index < 0) return null;
    // Saving the original order keeps the snapshot small and simple; shuffle is re-applied on restore.
    const ordered = queue.unshuffled ?? queue.entries;
    const currentKey = Q.current(queue)?.key;
    return {
      tracks: ordered.map((entry) => entry.track),
      index: Math.max(
        0,
        ordered.findIndex((entry) => entry.key === currentKey),
      ),
      repeat: queue.repeat,
      shuffled: queue.shuffled,
      position: this.#element.currentTime || this.#state.position,
    };
  }

  /** Loads a saved queue without starting playback. */
  restore(snapshot: PlayerSnapshot): void {
    if (snapshot.tracks.length === 0) return;
    let queue = Q.setRepeat(Q.playList(Q.emptyQueue<MediaFile>(), snapshot.tracks, snapshot.index), snapshot.repeat);
    if (snapshot.shuffled) queue = Q.setShuffle(queue, true, this.#options.random);
    this.#pendingSeek = snapshot.position > 0 ? snapshot.position : null;
    this.#applyQueue(queue, false);
    this.#set({ position: snapshot.position });
  }

  // Internals -----------------------------------------------------------

  #set(patch: Partial<PlayerState>): void {
    this.#state = { ...this.#state, ...patch };
    for (const listener of this.#listeners) listener(this.#state);
  }

  #duration(): number {
    const fromElement = this.#element.duration;
    if (Number.isFinite(fromElement) && fromElement > 0) return fromElement;
    const known = this.#state.current?.durationMs;
    return known ? known / 1000 : 0;
  }

  /** Commits a new queue; loads the current entry if it changed (or `reload`), playing if `autoplay`. */
  #applyQueue(queue: Q.Queue<MediaFile>, autoplay: boolean, reload = false): void {
    const entry = Q.current(queue);
    const changed = (entry?.key ?? null) !== this.#loadedKey;
    this.#set({ queue, current: entry?.track ?? null });
    if (!entry) {
      if (this.#loadedKey !== null) this.#unload();
      return;
    }
    if (changed || reload) this.#load(entry, autoplay);
  }

  #load(entry: Q.QueueEntry<MediaFile>, autoplay: boolean): void {
    this.#loadedKey = entry.key;
    this.#listened = 0;
    this.#lastTime = 0;
    this.#listenRecorded = false;
    const element = this.#element;
    element.src = entry.track.streamUrl;
    element.load();
    this.#set({
      position: 0,
      duration: entry.track.durationMs ? entry.track.durationMs / 1000 : 0,
      error: null,
      buffering: autoplay,
      playing: autoplay ? this.#state.playing : false,
    });
    if (autoplay) this.#startPlayback();
  }

  #unload(): void {
    this.#loadedKey = null;
    this.#element.pause();
    this.#element.removeAttribute('src');
    this.#element.load();
    this.#set({ playing: false, buffering: false, position: 0, duration: 0, current: null });
  }

  #startPlayback(): void {
    const current = this.#state.current;
    this.#element.play().catch((error: unknown) => {
      // AbortError: a newer load replaced this one; nothing to report.
      if (error instanceof DOMException && error.name === 'AbortError') return;
      if (error instanceof DOMException && error.name === 'NotAllowedError') {
        this.#set({ playing: false, buffering: false });
        return;
      }
      if (this.#state.current === current) {
        this.#set({ playing: false, buffering: false, error: current ? `Couldn't play “${current.title}”.` : null });
      }
    });
  }

  #onLoadedMetadata(): void {
    if (this.#pendingSeek !== null) {
      const target = this.#pendingSeek;
      this.#pendingSeek = null;
      this.seek(target);
    }
    this.#set({ duration: this.#duration() });
  }

  #onTimeUpdate(): void {
    const time = this.#element.currentTime;
    const delta = time - this.#lastTime;
    // Count only steady forward playback towards a listen, not seeks.
    if (delta > 0 && delta < 2 && !this.#element.paused) this.#listened += delta;
    this.#lastTime = time;
    if (!this.#listenRecorded && this.#listened >= LISTEN_THRESHOLD_SECONDS) this.#recordListen();
    this.#set({ position: time });
  }

  #recordListen(): void {
    const current = this.#state.current;
    if (!current || this.#listenRecorded) return;
    this.#listenRecorded = true;
    this.#options.onListen?.(current);
  }

  #onEnded(): void {
    this.#recordListen();
    const queue = this.#state.queue;
    if (queue.repeat === 'one') {
      this.seek(0);
      this.#listenRecorded = false;
      this.#listened = 0;
      this.#startPlayback();
      return;
    }
    const advanced = Q.next(queue, 'ended');
    if (advanced) {
      this.#applyQueue(advanced, true, true);
      return;
    }
    // End of the queue: stay on the last track, back at the start, paused.
    this.#element.currentTime = 0;
    this.#lastTime = 0;
    this.#set({ playing: false, position: 0 });
  }

  #onError(): void {
    const current = this.#state.current;
    if (!current || this.#loadedKey === null) return;
    const offline = typeof navigator !== 'undefined' && navigator.onLine === false;
    this.#set({
      playing: false,
      buffering: false,
      error: offline ? `“${current.title}” isn't downloaded, and you're offline.` : `Couldn't play “${current.title}”.`,
    });
  }
}
