import type { MediaElementLike } from '../player/engine';

/** A scriptable stand-in for an audio element. */
export class FakeAudio implements MediaElementLike {
  src = '';
  currentTime = 0;
  duration = Number.NaN;
  paused = true;
  loads = 0;
  playCalls = 0;
  /** When set, the next play() rejects with this error. */
  rejectNextPlay: Error | null = null;
  #listeners = new Map<string, Set<(event: Event) => void>>();

  play(): Promise<void> {
    this.playCalls += 1;
    if (this.rejectNextPlay) {
      const error = this.rejectNextPlay;
      this.rejectNextPlay = null;
      return Promise.reject(error);
    }
    this.paused = false;
    this.emit('play');
    this.emit('playing');
    return Promise.resolve();
  }

  pause(): void {
    if (this.paused) return;
    this.paused = true;
    this.emit('pause');
  }

  load(): void {
    this.loads += 1;
    this.currentTime = 0;
  }

  removeAttribute(name: string): void {
    if (name === 'src') this.src = '';
  }

  addEventListener(type: string, listener: (event: Event) => void): void {
    let set = this.#listeners.get(type);
    if (!set) {
      set = new Set();
      this.#listeners.set(type, set);
    }
    set.add(listener);
  }

  removeEventListener(type: string, listener: (event: Event) => void): void {
    this.#listeners.get(type)?.delete(listener);
  }

  emit(type: string): void {
    for (const listener of this.#listeners.get(type) ?? []) listener(new Event(type));
  }

  listenerCount(): number {
    let count = 0;
    for (const set of this.#listeners.values()) count += set.size;
    return count;
  }

  /** Plays forward to `seconds` in steady half-second steps, firing timeupdate. */
  advanceTo(seconds: number): void {
    while (this.currentTime + 0.5 <= seconds) {
      this.currentTime += 0.5;
      this.emit('timeupdate');
    }
  }

  /** Finishes the track. */
  finish(): void {
    this.paused = true;
    this.emit('ended');
  }
}
