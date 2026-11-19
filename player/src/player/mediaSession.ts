// Platform playback controls (lock screen, notification, headset keys) via the Media Session API.

import type { MediaFile } from '../api';

/** The parts of `navigator.mediaSession` we use. */
export type MediaSessionLike = {
  metadata: MediaMetadata | null;
  playbackState: MediaSessionPlaybackState;
  setActionHandler(action: MediaSessionAction, handler: MediaSessionActionHandler | null): void;
  setPositionState?(state?: MediaPositionState): void;
};

export type SessionControls = {
  play: () => void;
  pause: () => void;
  stop: () => void;
  seekBy: (seconds: number) => void;
  seekTo: (seconds: number) => void;
  /** Omitted for video, which has no queue. */
  previous?: () => void;
  next?: () => void;
};

export type MetadataFactory = (init: MediaMetadataInit) => MediaMetadata;

export const defaultMetadataFactory: MetadataFactory = (init) =>
  typeof MediaMetadata === 'function' ? new MediaMetadata(init) : (init as unknown as MediaMetadata);

/** The session for this browser, or null when unsupported. */
export function browserMediaSession(): MediaSessionLike | null {
  return typeof navigator !== 'undefined' && 'mediaSession' in navigator ? navigator.mediaSession : null;
}

function artworkType(url: string): string | undefined {
  const extension = url.split('?')[0]?.split('.').pop()?.toLowerCase();
  switch (extension) {
    case 'jpg':
    case 'jpeg':
      return 'image/jpeg';
    case 'png':
      return 'image/png';
    case 'webp':
      return 'image/webp';
    default:
      return undefined;
  }
}

/** Title, artist (creator), album (item title) and artwork (cover) for a file. */
export function metadataFor(file: MediaFile): MediaMetadataInit {
  const artwork: MediaImage[] = [];
  if (file.cover) {
    const type = artworkType(file.cover);
    artwork.push(type ? { src: file.cover, type } : { src: file.cover });
  }
  return { title: file.title, artist: file.creator, album: file.itemTitle, artwork };
}

/**
 * Installs action handlers. Actions the browser doesn't know throw; those
 * are skipped. Returns a function that removes the handlers again.
 */
export function bindMediaSession(session: MediaSessionLike, controls: SessionControls): () => void {
  const handlers: [MediaSessionAction, MediaSessionActionHandler | null][] = [
    ['play', () => controls.play()],
    ['pause', () => controls.pause()],
    ['stop', () => controls.stop()],
    ['seekbackward', (details) => controls.seekBy(-(details.seekOffset ?? 10))],
    ['seekforward', (details) => controls.seekBy(details.seekOffset ?? 10)],
    [
      'seekto',
      (details) => {
        if (details.seekTime != null) controls.seekTo(details.seekTime);
      },
    ],
    ['previoustrack', controls.previous ? () => controls.previous?.() : null],
    ['nexttrack', controls.next ? () => controls.next?.() : null],
  ];
  const installed: MediaSessionAction[] = [];
  for (const [action, handler] of handlers) {
    try {
      session.setActionHandler(action, handler);
      installed.push(action);
    } catch {
      // Unsupported action in this browser.
    }
  }
  return () => {
    for (const action of installed) {
      try {
        session.setActionHandler(action, null);
      } catch {
        // Ignore.
      }
    }
  };
}

export type SessionInfo = {
  file: MediaFile | null;
  playing: boolean;
  position: number;
  duration: number;
  playbackRate?: number;
};

/** Keeps metadata, playback state and position in step with what is playing. */
export function createSessionUpdater(
  session: MediaSessionLike,
  makeMetadata: MetadataFactory = defaultMetadataFactory,
) {
  let lastFileId: number | null = null;
  return (info: SessionInfo) => {
    const id = info.file?.id ?? null;
    if (id !== lastFileId) {
      lastFileId = id;
      session.metadata = info.file ? makeMetadata(metadataFor(info.file)) : null;
    }
    session.playbackState = info.file ? (info.playing ? 'playing' : 'paused') : 'none';
    if (!session.setPositionState) return;
    try {
      if (info.file && Number.isFinite(info.duration) && info.duration > 0) {
        session.setPositionState({
          duration: info.duration,
          position: Math.min(Math.max(0, info.position), info.duration),
          playbackRate: info.playbackRate ?? 1,
        });
      } else {
        session.setPositionState();
      }
    } catch {
      // Some browsers reject position state while loading.
    }
  };
}
