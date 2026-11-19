// The reactive face of the player engine, plus queue persistence and
// platform controls.

import { createSignal } from 'solid-js';
import type { MediaFile } from '../api';
import { PlayerEngine, type MediaElementLike, type PlayerSnapshot, type PlayerState } from '../player/engine';
import { bindMediaSession, createSessionUpdater, type MediaSessionLike } from '../player/mediaSession';

const SNAPSHOT_KEY = 'player.queue';

export type PlayerOptions = {
  element: MediaElementLike;
  onListen?: (file: MediaFile) => void;
  mediaSession?: MediaSessionLike | null;
  storage?: Pick<Storage, 'getItem' | 'setItem' | 'removeItem'> | null;
};

export function createPlayer(options: PlayerOptions) {
  const engine = new PlayerEngine(options.element, options.onListen ? { onListen: options.onListen } : {});
  const [state, setState] = createSignal<PlayerState>(engine.state, { equals: false });
  /** Set while a film plays in the video screen, so updates wait and music stays paused. */
  const [videoPlaying, setVideoPlaying] = createSignal(false);

  engine.subscribe((next) => setState(next));

  // Platform controls (lock screen, headset keys).
  const session = options.mediaSession;
  let sessionOwner: 'music' | 'video' = 'music';
  const updateSession = session ? createSessionUpdater(session) : null;
  const bindMusicSession = () =>
    session
      ? bindMediaSession(session, {
          play: () => engine.play(),
          pause: () => engine.pause(),
          stop: () => engine.stop(),
          seekBy: (seconds) => engine.seekBy(seconds),
          seekTo: (seconds) => engine.seek(seconds),
          previous: () => engine.previous(),
          next: () => engine.next(),
        })
      : () => {};
  bindMusicSession();
  let last = { fileId: null as number | null, playing: false, position: 0, at: 0 };
  engine.subscribe((next) => {
    if (!updateSession || sessionOwner !== 'music') return;
    const now = Date.now();
    const fileId = next.current?.id ?? null;
    const expected = last.position + (last.playing ? (now - last.at) / 1000 : 0);
    // Position ticks arrive several times a second; push only on real changes, seeks, or once a second.
    const changed = fileId !== last.fileId || next.playing !== last.playing || Math.abs(next.position - expected) > 1.5;
    if (!changed && now - last.at < 1000) return;
    last = { fileId, playing: next.playing, position: next.position, at: now };
    updateSession({ file: next.current, playing: next.playing, position: next.position, duration: next.duration });
  });

  // Keep the queue across reloads (an update reload must not lose it).
  const storage = options.storage;
  let saveTimer: ReturnType<typeof setTimeout> | undefined;
  engine.subscribe(() => {
    if (!storage || saveTimer) return;
    saveTimer = setTimeout(() => {
      saveTimer = undefined;
      try {
        const snapshot = engine.snapshot();
        if (snapshot) storage.setItem(SNAPSHOT_KEY, JSON.stringify(snapshot));
        else storage.removeItem(SNAPSHOT_KEY);
      } catch {
        // Storage full or blocked: not worth bothering the listener.
      }
    }, 1000);
  });

  return {
    engine,
    state,
    current: () => state().current,
    playing: () => state().playing,
    videoPlaying,
    /** True when anything audible is going on. */
    busy: () => state().playing || videoPlaying(),

    restore(): void {
      if (!storage) return;
      try {
        const saved = storage.getItem(SNAPSHOT_KEY);
        if (saved) engine.restore(JSON.parse(saved) as PlayerSnapshot);
      } catch {
        storage.removeItem(SNAPSHOT_KEY);
      }
    },

    /** Plays a list of files; non-audio files are left out of the queue. */
    playFiles(files: readonly MediaFile[], start: MediaFile | number = 0): void {
      const audio = files.filter((file) => file.kind === 'audio');
      const startIndex =
        typeof start === 'number'
          ? audio.indexOf(files[start] as MediaFile)
          : audio.findIndex((file) => file.id === start.id);
      engine.playList(audio, Math.max(0, startIndex));
    },

    shuffleFiles(files: readonly MediaFile[]): void {
      const audio = files.filter((file) => file.kind === 'audio');
      if (audio.length === 0) return;
      if (!engine.state.queue.shuffled) engine.toggleShuffle();
      engine.playList(audio, Math.floor(Math.random() * audio.length));
    },

    playNext: (files: readonly MediaFile[]) => engine.playNext(files.filter((file) => file.kind === 'audio')),
    addToQueue: (files: readonly MediaFile[]) => engine.addToQueue(files.filter((file) => file.kind === 'audio')),

    /** The video screen takes over the platform controls while a film is open. */
    claimSessionForVideo(): () => void {
      engine.pause();
      sessionOwner = 'video';
      return () => {
        sessionOwner = 'music';
        setVideoPlaying(false);
        bindMusicSession();
        const now = engine.state;
        updateSession?.({ file: now.current, playing: now.playing, position: now.position, duration: now.duration });
      };
    },
    setVideoPlaying,
    mediaSession: session ?? null,
  };
}

export type Player = ReturnType<typeof createPlayer>;
