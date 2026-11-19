import {
  addToEnd,
  current,
  emptyQueue,
  jumpTo,
  move,
  next,
  nextRepeatMode,
  playList,
  playNext,
  previous,
  remove,
  setRepeat,
  setShuffle,
  shouldRestart,
  upNext,
  type Queue,
} from './queue';

const tracks = (queue: Queue<string>) => queue.entries.map((entry) => entry.track);
const now = (queue: Queue<string> | null) => (queue ? current(queue)?.track : undefined);

/** A deterministic "random" that reverses order in Fisher–Yates. */
const reverseRandom = () => 0.999;

describe('playList', () => {
  test('starts at the chosen track', () => {
    const queue = playList(emptyQueue<string>(), ['a', 'b', 'c'], 1);
    expect(tracks(queue)).toEqual(['a', 'b', 'c']);
    expect(now(queue)).toBe('b');
    expect(upNext(queue).map((entry) => entry.track)).toEqual(['c']);
  });

  test('clamps an out-of-range start', () => {
    expect(now(playList(emptyQueue<string>(), ['a', 'b'], 9))).toBe('b');
    expect(now(playList(emptyQueue<string>(), ['a', 'b'], -3))).toBe('a');
  });

  test('an empty list empties the queue but keeps repeat', () => {
    const base = setRepeat(playList(emptyQueue<string>(), ['a']), 'all');
    const queue = playList(base, []);
    expect(queue.entries).toHaveLength(0);
    expect(queue.index).toBe(-1);
    expect(queue.repeat).toBe('all');
  });

  test('gives every entry a unique key, even repeats of the same track', () => {
    const queue = addToEnd(playList(emptyQueue<string>(), ['a', 'a']), ['a']);
    const keys = queue.entries.map((entry) => entry.key);
    expect(new Set(keys).size).toBe(3);
  });

  test('with shuffle on, keeps the chosen track first', () => {
    const shuffled = setShuffle(emptyQueue<string>(), true, reverseRandom);
    const queue = playList(shuffled, ['a', 'b', 'c', 'd'], 2, Math.random);
    expect(now(queue)).toBe('c');
    expect(queue.index).toBe(0);
    expect([...tracks(queue)].sort()).toEqual(['a', 'b', 'c', 'd']);
    expect(setShuffle(queue, false).entries.map((entry) => entry.track)).toEqual(['a', 'b', 'c', 'd']);
  });
});

describe('playNext and addToEnd', () => {
  test('play next inserts after the current track', () => {
    const queue = playNext(playList(emptyQueue<string>(), ['a', 'b', 'c'], 0), ['x', 'y']);
    expect(tracks(queue)).toEqual(['a', 'x', 'y', 'b', 'c']);
    expect(now(queue)).toBe('a');
  });

  test('add to queue appends', () => {
    const queue = addToEnd(playList(emptyQueue<string>(), ['a', 'b'], 0), ['z']);
    expect(tracks(queue)).toEqual(['a', 'b', 'z']);
    expect(now(queue)).toBe('a');
  });

  test('both start playback when the queue is empty', () => {
    expect(now(playNext(emptyQueue<string>(), ['x']))).toBe('x');
    expect(now(addToEnd(emptyQueue<string>(), ['x']))).toBe('x');
  });

  test('adding nothing changes nothing', () => {
    const queue = playList(emptyQueue<string>(), ['a']);
    expect(playNext(queue, [])).toBe(queue);
    expect(addToEnd(queue, [])).toBe(queue);
  });

  test('items added while shuffled survive turning shuffle off', () => {
    let queue = playList(emptyQueue<string>(), ['a', 'b', 'c'], 0);
    queue = setShuffle(queue, true, reverseRandom);
    queue = playNext(queue, ['n']);
    queue = addToEnd(queue, ['e']);
    expect(tracks(queue)[1]).toBe('n');
    queue = setShuffle(queue, false);
    expect(tracks(queue)).toEqual(['a', 'n', 'b', 'c', 'e']);
    expect(now(queue)).toBe('a');
  });
});

describe('remove', () => {
  const base = () => playList(emptyQueue<string>(), ['a', 'b', 'c', 'd'], 2);
  const keyOf = (queue: Queue<string>, track: string) =>
    queue.entries.find((entry) => entry.track === track)?.key ?? -1;

  test('removing an earlier entry keeps the current track', () => {
    const queue = base();
    const after = remove(queue, keyOf(queue, 'a'));
    expect(tracks(after)).toEqual(['b', 'c', 'd']);
    expect(now(after)).toBe('c');
  });

  test('removing a later entry keeps the current track', () => {
    const queue = base();
    expect(now(remove(queue, keyOf(queue, 'd')))).toBe('c');
  });

  test('removing the current entry moves to the next one', () => {
    const queue = base();
    expect(now(remove(queue, keyOf(queue, 'c')))).toBe('d');
  });

  test('removing the current last entry steps back', () => {
    const queue = jumpTo(base(), 3);
    expect(now(remove(queue, keyOf(queue, 'd')))).toBe('c');
  });

  test('removing the only entry empties the queue', () => {
    const queue = playList(emptyQueue<string>(), ['a']);
    const after = remove(queue, keyOf(queue, 'a'));
    expect(after.entries).toHaveLength(0);
    expect(after.index).toBe(-1);
  });

  test('an unknown key is ignored', () => {
    const queue = base();
    expect(remove(queue, 999)).toBe(queue);
  });

  test('removed entries do not come back when shuffle is turned off', () => {
    let queue = setShuffle(base(), true, reverseRandom);
    queue = remove(queue, keyOf(queue, 'a'));
    expect(tracks(setShuffle(queue, false))).toEqual(['b', 'c', 'd']);
  });
});

describe('move', () => {
  test('moves an entry and keeps the current track current', () => {
    const queue = playList(emptyQueue<string>(), ['a', 'b', 'c', 'd'], 1);
    const moved = move(queue, 3, 0);
    expect(tracks(moved)).toEqual(['d', 'a', 'b', 'c']);
    expect(now(moved)).toBe('b');
    expect(moved.index).toBe(2);
  });

  test('can move the current entry itself', () => {
    const queue = playList(emptyQueue<string>(), ['a', 'b', 'c'], 0);
    const moved = move(queue, 0, 2);
    expect(tracks(moved)).toEqual(['b', 'c', 'a']);
    expect(now(moved)).toBe('a');
  });

  test('ignores out-of-range and no-op moves', () => {
    const queue = playList(emptyQueue<string>(), ['a', 'b']);
    expect(move(queue, 0, 0)).toBe(queue);
    expect(move(queue, -1, 1)).toBe(queue);
    expect(move(queue, 0, 5)).toBe(queue);
  });
});

describe('jumpTo', () => {
  test('changes the current entry', () => {
    expect(now(jumpTo(playList(emptyQueue<string>(), ['a', 'b', 'c']), 2))).toBe('c');
  });
  test('ignores positions outside the queue', () => {
    const queue = playList(emptyQueue<string>(), ['a']);
    expect(jumpTo(queue, 4)).toBe(queue);
  });
});

describe('shuffle', () => {
  test('keeps the current track first and every track once', () => {
    const queue = playList(emptyQueue<string>(), ['a', 'b', 'c', 'd', 'e'], 3);
    const shuffled = setShuffle(queue, true, reverseRandom);
    expect(shuffled.shuffled).toBe(true);
    expect(shuffled.index).toBe(0);
    expect(now(shuffled)).toBe('d');
    expect([...tracks(shuffled)].sort()).toEqual(['a', 'b', 'c', 'd', 'e']);
  });

  test('turning it off restores order and keeps the current track', () => {
    const queue = playList(emptyQueue<string>(), ['a', 'b', 'c', 'd'], 1);
    let shuffled = setShuffle(queue, true, reverseRandom);
    shuffled = next(shuffled, 'skip') ?? shuffled;
    const playing = now(shuffled);
    const restored = setShuffle(shuffled, false);
    expect(tracks(restored)).toEqual(['a', 'b', 'c', 'd']);
    expect(now(restored)).toBe(playing);
  });

  test('setting the same state is a no-op', () => {
    const queue = playList(emptyQueue<string>(), ['a']);
    expect(setShuffle(queue, false)).toBe(queue);
  });

  test('works on an empty queue', () => {
    const queue = setShuffle(emptyQueue<string>(), true);
    expect(queue.shuffled).toBe(true);
    expect(queue.index).toBe(-1);
  });
});

describe('next', () => {
  const base = () => playList(emptyQueue<string>(), ['a', 'b', 'c'], 0);

  test('moves forward', () => {
    expect(now(next(base(), 'skip'))).toBe('b');
    expect(now(next(base(), 'ended'))).toBe('b');
  });

  test('stops at the end with repeat off', () => {
    const last = jumpTo(base(), 2);
    expect(next(last, 'ended')).toBeNull();
    expect(next(last, 'skip')).toBeNull();
  });

  test('wraps with repeat all', () => {
    const last = setRepeat(jumpTo(base(), 2), 'all');
    expect(now(next(last, 'ended'))).toBe('a');
  });

  test('repeat one replays when a track ends but a skip still moves on', () => {
    const queue = setRepeat(base(), 'one');
    expect(now(next(queue, 'ended'))).toBe('a');
    expect(now(next(queue, 'skip'))).toBe('b');
    expect(next(jumpTo(queue, 2), 'skip')).toBeNull();
  });

  test('an empty queue has nothing next', () => {
    expect(next(emptyQueue<string>(), 'skip')).toBeNull();
  });
});

describe('previous', () => {
  test('moves back', () => {
    expect(now(previous(playList(emptyQueue<string>(), ['a', 'b'], 1)))).toBe('a');
  });
  test('has nothing before the first track', () => {
    expect(previous(playList(emptyQueue<string>(), ['a', 'b'], 0))).toBeNull();
    expect(previous(emptyQueue<string>())).toBeNull();
  });
  test('wraps to the end with repeat all', () => {
    const queue = setRepeat(playList(emptyQueue<string>(), ['a', 'b', 'c'], 0), 'all');
    expect(now(previous(queue))).toBe('c');
  });
  test('restarts the track when more than 3 seconds in', () => {
    expect(shouldRestart(3.5)).toBe(true);
    expect(shouldRestart(3)).toBe(false);
    expect(shouldRestart(0)).toBe(false);
  });
});

test('repeat cycles off, all, one', () => {
  expect(nextRepeatMode('off')).toBe('all');
  expect(nextRepeatMode('all')).toBe('one');
  expect(nextRepeatMode('one')).toBe('off');
});
