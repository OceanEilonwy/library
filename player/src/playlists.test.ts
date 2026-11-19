import {
  audioOnly,
  filterPlaylists,
  idsWithout,
  movedIds,
  partitionPlaylists,
  playableEntries,
  visiblePlaylists,
} from './playlists';
import { mediaFile, playlistSummary } from './test/fixtures';

test("hides other people's empty playlists but keeps your own", () => {
  const list = [
    playlistSummary(1, { entryCount: 0 }),
    playlistSummary(2, { entryCount: 0, mine: true }),
    playlistSummary(3),
  ];
  expect(visiblePlaylists(list).map((p) => p.id)).toEqual([2, 3]);
});

test('partitions into mine and others in server order', () => {
  const list = [playlistSummary(1, { mine: true }), playlistSummary(2), playlistSummary(3, { mine: true })];
  const { mine, others } = partitionPlaylists(list);
  expect(mine.map((p) => p.id)).toEqual([1, 3]);
  expect(others.map((p) => p.id)).toEqual([2]);
});

test('filters by name or owner, ignoring case', () => {
  const list = [playlistSummary(1, { name: 'Sunday Jazz' }), playlistSummary(2, { name: 'Road trip', owner: 'Bob' })];
  expect(filterPlaylists(list, 'jazz').map((p) => p.id)).toEqual([1]);
  expect(filterPlaylists(list, 'bob').map((p) => p.id)).toEqual([2]);
  expect(filterPlaylists(list, '  ')).toHaveLength(2);
});

test('offline, only downloaded entries are playable', () => {
  const entries = [mediaFile(1), mediaFile(2), mediaFile(3)];
  expect(playableEntries(entries, { offline: false, downloaded: new Set() })).toHaveLength(3);
  expect(playableEntries(entries, { offline: true, downloaded: new Set([2]) }).map((f) => f.id)).toEqual([2]);
});

test('play all queues audio only', () => {
  const entries = [mediaFile(1), mediaFile(2, { kind: 'video' }), mediaFile(3, { kind: 'document' })];
  expect(audioOnly(entries).map((f) => f.id)).toEqual([1]);
});

test('reorder and remove build the id list for the server', () => {
  const entries = [mediaFile(1), mediaFile(2), mediaFile(3), mediaFile(2)];
  expect(movedIds(entries, 0, 2)).toEqual([2, 3, 1, 2]);
  expect(movedIds(entries, 0, 9)).toEqual([1, 2, 3, 2]);
  expect(idsWithout(entries, 3)).toEqual([1, 2, 3]);
});
