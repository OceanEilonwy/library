import { bindMediaSession, createSessionUpdater, metadataFor, type MediaSessionLike } from './mediaSession';
import { mediaFile } from '../test/fixtures';

class FakeMediaSession implements MediaSessionLike {
  metadata: MediaMetadata | null = null;
  playbackState: MediaSessionPlaybackState = 'none';
  handlers = new Map<MediaSessionAction, MediaSessionActionHandler | null>();
  positions: (MediaPositionState | undefined)[] = [];
  unsupported = new Set<MediaSessionAction>();

  setActionHandler(action: MediaSessionAction, handler: MediaSessionActionHandler | null) {
    if (this.unsupported.has(action)) throw new TypeError('unsupported');
    this.handlers.set(action, handler);
  }

  setPositionState(state?: MediaPositionState) {
    this.positions.push(state);
  }

  fire(action: MediaSessionAction, details: Partial<MediaSessionActionDetails> = {}) {
    this.handlers.get(action)?.({ action, ...details } as MediaSessionActionDetails);
  }
}

function controls() {
  return {
    play: vi.fn(),
    pause: vi.fn(),
    stop: vi.fn(),
    seekBy: vi.fn(),
    seekTo: vi.fn(),
    previous: vi.fn(),
    next: vi.fn(),
  };
}

test('metadata uses creator as artist and item title as album', () => {
  expect(metadataFor(mediaFile(1, { title: 'Locomotion' }))).toEqual({
    title: 'Locomotion',
    artist: 'John Coltrane',
    album: 'Blue Train',
    artwork: [{ src: '/photos/blue-train.jpg', type: 'image/jpeg' }],
  });
  expect(metadataFor(mediaFile(1, { cover: null })).artwork).toEqual([]);
});

test('every action reaches the player', () => {
  const session = new FakeMediaSession();
  const player = controls();
  bindMediaSession(session, player);
  session.fire('play');
  session.fire('pause');
  session.fire('stop');
  session.fire('previoustrack');
  session.fire('nexttrack');
  session.fire('seekbackward');
  session.fire('seekforward', { seekOffset: 30 });
  session.fire('seekto', { seekTime: 61 });
  expect(player.play).toHaveBeenCalledOnce();
  expect(player.pause).toHaveBeenCalledOnce();
  expect(player.stop).toHaveBeenCalledOnce();
  expect(player.previous).toHaveBeenCalledOnce();
  expect(player.next).toHaveBeenCalledOnce();
  expect(player.seekBy.mock.calls).toEqual([[-10], [30]]);
  expect(player.seekTo).toHaveBeenCalledWith(61);
});

test('video has no previous or next', () => {
  const session = new FakeMediaSession();
  const { previous: _p, next: _n, ...video } = controls();
  bindMediaSession(session, video);
  expect(session.handlers.get('nexttrack')).toBeNull();
  expect(session.handlers.get('previoustrack')).toBeNull();
});

test('unsupported actions are skipped and unbinding clears handlers', () => {
  const session = new FakeMediaSession();
  session.unsupported.add('stop');
  const unbind = bindMediaSession(session, controls());
  expect(session.handlers.has('stop')).toBe(false);
  unbind();
  expect([...session.handlers.values()].every((handler) => handler === null)).toBe(true);
});

test('the updater keeps metadata, state and position current', () => {
  const session = new FakeMediaSession();
  const make = vi.fn((init: MediaMetadataInit) => init as unknown as MediaMetadata);
  const update = createSessionUpdater(session, make);
  const file = mediaFile(1);
  update({ file, playing: true, position: 10, duration: 180 });
  update({ file, playing: false, position: 11, duration: 180 });
  expect(make).toHaveBeenCalledOnce();
  expect(session.playbackState).toBe('paused');
  expect(session.positions.at(-1)).toEqual({ duration: 180, position: 11, playbackRate: 1 });
  update({ file, playing: true, position: 500, duration: 180 });
  expect(session.positions.at(-1)?.position).toBe(180);
  update({ file: null, playing: false, position: 0, duration: 0 });
  expect(session.metadata).toBeNull();
  expect(session.playbackState).toBe('none');
  expect(session.positions.at(-1)).toBeUndefined();
});
