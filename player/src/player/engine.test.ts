import { PlayerEngine } from './engine';
import { FakeAudio } from '../test/fakeAudio';
import { mediaFile } from '../test/fixtures';

const album = [mediaFile(1), mediaFile(2), mediaFile(3)];

function setup() {
  const audio = new FakeAudio();
  const onListen = vi.fn();
  const engine = new PlayerEngine(audio, { onListen, random: () => 0.999 });
  return { audio, engine, onListen };
}

const flushPromises = () => new Promise((resolve) => setTimeout(resolve, 0));

test('playing a list loads and plays the chosen track', () => {
  const { audio, engine } = setup();
  engine.playList(album, 1);
  expect(audio.src).toBe('/api/media/2/stream');
  expect(audio.paused).toBe(false);
  expect(engine.state.current?.id).toBe(2);
  expect(engine.state.playing).toBe(true);
  expect(engine.state.duration).toBe(180);
});

test('notifies subscribers and can unsubscribe', () => {
  const { engine } = setup();
  const listener = vi.fn();
  const stop = engine.subscribe(listener);
  engine.playList(album);
  expect(listener).toHaveBeenCalled();
  listener.mockClear();
  stop();
  engine.pause();
  expect(listener).not.toHaveBeenCalled();
});

test('toggle play pauses and resumes', () => {
  const { audio, engine } = setup();
  engine.playList(album);
  engine.togglePlay();
  expect(audio.paused).toBe(true);
  expect(engine.state.playing).toBe(false);
  engine.togglePlay();
  expect(audio.paused).toBe(false);
});

test('play does nothing with an empty queue', () => {
  const { audio, engine } = setup();
  engine.play();
  expect(audio.playCalls).toBe(0);
});

test('seeks by ten seconds and clamps to the track', () => {
  const { audio, engine } = setup();
  engine.playList(album);
  audio.currentTime = 5;
  engine.seekBy(10);
  expect(audio.currentTime).toBe(15);
  engine.seekBy(-60);
  expect(audio.currentTime).toBe(0);
  engine.seek(9999);
  expect(audio.currentTime).toBe(180);
  expect(engine.state.position).toBe(180);
});

test('next and previous move through the queue', () => {
  const { audio, engine } = setup();
  engine.playList(album);
  engine.next();
  expect(audio.src).toBe('/api/media/2/stream');
  engine.previous();
  expect(audio.src).toBe('/api/media/1/stream');
});

test('previous restarts the track when more than 3 seconds in', () => {
  const { audio, engine } = setup();
  engine.playList(album, 1);
  audio.currentTime = 42;
  engine.previous();
  expect(audio.src).toBe('/api/media/2/stream');
  expect(audio.currentTime).toBe(0);
});

test('previous on the first track restarts it', () => {
  const { audio, engine } = setup();
  engine.playList(album, 0);
  audio.currentTime = 1;
  engine.previous();
  expect(audio.src).toBe('/api/media/1/stream');
  expect(audio.currentTime).toBe(0);
});

test('a finished track moves on to the next', () => {
  const { audio, engine } = setup();
  engine.playList(album);
  audio.finish();
  expect(audio.src).toBe('/api/media/2/stream');
  expect(engine.state.playing).toBe(true);
});

test('the end of the queue stops on the last track', () => {
  const { audio, engine } = setup();
  engine.playList(album, 2);
  audio.finish();
  expect(audio.src).toBe('/api/media/3/stream');
  expect(engine.state.playing).toBe(false);
  expect(engine.state.position).toBe(0);
});

test('repeat one replays the track; repeat all wraps', () => {
  const { audio, engine } = setup();
  engine.playList(album, 2);
  engine.cycleRepeat(); // all
  audio.finish();
  expect(audio.src).toBe('/api/media/1/stream');
  engine.cycleRepeat(); // one
  audio.currentTime = 100;
  audio.finish();
  expect(audio.src).toBe('/api/media/1/stream');
  expect(audio.currentTime).toBe(0);
  expect(audio.paused).toBe(false);
  expect(engine.state.queue.repeat).toBe('one');
});

test('play next and add to queue do not interrupt the current track', () => {
  const { audio, engine } = setup();
  engine.playList([mediaFile(1)]);
  const loads = audio.loads;
  engine.addToQueue([mediaFile(3)]);
  engine.playNext([mediaFile(2)]);
  expect(audio.loads).toBe(loads);
  expect(engine.state.queue.entries.map((e) => e.track.id)).toEqual([1, 2, 3]);
});

test('adding to an empty queue starts playing', () => {
  const { audio, engine } = setup();
  engine.addToQueue([mediaFile(7)]);
  expect(audio.src).toBe('/api/media/7/stream');
  expect(engine.state.playing).toBe(true);
});

test('removing the current track loads the next; removing the last stops', () => {
  const { audio, engine } = setup();
  engine.playList(album);
  const firstKey = engine.state.queue.entries[0]?.key ?? -1;
  engine.removeEntry(firstKey);
  expect(audio.src).toBe('/api/media/2/stream');
  for (const entry of [...engine.state.queue.entries]) engine.removeEntry(entry.key);
  expect(engine.state.current).toBeNull();
  expect(audio.src).toBe('');
  expect(engine.state.playing).toBe(false);
});

test('moving entries keeps the current track playing', () => {
  const { audio, engine } = setup();
  engine.playList(album);
  const loads = audio.loads;
  engine.moveEntry(2, 0);
  expect(audio.loads).toBe(loads);
  expect(engine.state.current?.id).toBe(1);
  expect(engine.state.queue.entries.map((e) => e.track.id)).toEqual([3, 1, 2]);
});

test('jumping to an entry plays it', () => {
  const { audio, engine } = setup();
  engine.playList(album);
  engine.jumpTo(2);
  expect(audio.src).toBe('/api/media/3/stream');
});

test('shuffle keeps the current track', () => {
  const { engine } = setup();
  engine.playList(album, 1);
  engine.toggleShuffle();
  expect(engine.state.queue.shuffled).toBe(true);
  expect(engine.state.current?.id).toBe(2);
  expect(engine.state.queue.index).toBe(0);
});

test('records a listen once after ten seconds of playback', () => {
  const { audio, engine, onListen } = setup();
  engine.playList(album);
  audio.advanceTo(9.5);
  expect(onListen).not.toHaveBeenCalled();
  audio.advanceTo(15);
  expect(onListen).toHaveBeenCalledOnce();
  expect(onListen).toHaveBeenCalledWith(expect.objectContaining({ id: 1 }));
});

test('seeking past ten seconds is not a listen', () => {
  const { audio, engine, onListen } = setup();
  engine.playList(album);
  audio.currentTime = 60;
  audio.emit('timeupdate');
  expect(onListen).not.toHaveBeenCalled();
});

test('a short track that finishes counts as a listen', () => {
  const { audio, engine, onListen } = setup();
  engine.playList(album);
  audio.advanceTo(4);
  audio.finish();
  expect(onListen).toHaveBeenCalledWith(expect.objectContaining({ id: 1 }));
});

test('a refused autoplay leaves the player paused without an error', async () => {
  const { audio, engine } = setup();
  audio.rejectNextPlay = new DOMException('no', 'NotAllowedError');
  engine.playList(album);
  await flushPromises();
  expect(engine.state.playing).toBe(false);
  expect(engine.state.error).toBeNull();
});

test('a playback error is reported in plain words', async () => {
  const { audio, engine } = setup();
  audio.rejectNextPlay = new DOMException('bad', 'NotSupportedError');
  engine.playList(album);
  await flushPromises();
  expect(engine.state.error).toBe("Couldn't play “Track 1”.");
});

test('media errors are reported', () => {
  const { audio, engine } = setup();
  engine.playList(album);
  audio.emit('error');
  expect(engine.state.error).toMatch(/Track 1/);
  expect(engine.state.playing).toBe(false);
});

test('snapshot and restore bring back the queue without playing', () => {
  const { audio, engine } = setup();
  engine.playList(album, 1);
  engine.cycleRepeat();
  audio.currentTime = 30;
  const snapshot = engine.snapshot();
  expect(snapshot).toMatchObject({ index: 1, repeat: 'all', position: 30 });

  const other = setup();
  if (!snapshot) throw new Error('expected a snapshot');
  other.engine.restore(snapshot);
  expect(other.engine.state.current?.id).toBe(2);
  expect(other.audio.paused).toBe(true);
  expect(other.engine.state.queue.repeat).toBe('all');
  other.audio.emit('loadedmetadata');
  expect(other.audio.currentTime).toBe(30);
});

test('an empty queue has no snapshot', () => {
  expect(setup().engine.snapshot()).toBeNull();
});

test('destroy removes every listener', () => {
  const { audio, engine } = setup();
  expect(audio.listenerCount()).toBeGreaterThan(0);
  engine.destroy();
  expect(audio.listenerCount()).toBe(0);
});
