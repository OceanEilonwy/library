import { openPlayerDb } from './db';
import { createHistoryStore } from './history';
import { mediaFile } from './test/fixtures';

let counter = 0;
const freshStore = (limit?: number) => {
  counter += 1;
  const db = openPlayerDb(`history-test-${counter}`);
  return createHistoryStore(() => db, limit);
};

test('records plays and lists them newest first', async () => {
  const store = freshStore();
  await store.record(mediaFile(1), 1000);
  await store.record(mediaFile(2), 3000);
  await store.record(mediaFile(3), 2000);
  expect((await store.list()).map((entry) => entry.file.id)).toEqual([2, 3, 1]);
});

test('searches title, album and artist', async () => {
  const store = freshStore();
  await store.record(mediaFile(1, { title: "Moment's Notice" }), 1);
  await store.record(mediaFile(2, { title: 'So What', itemTitle: 'Kind of Blue', creator: 'Miles Davis' }), 2);
  expect((await store.search('notice')).map((e) => e.file.id)).toEqual([1]);
  expect((await store.search('MILES')).map((e) => e.file.id)).toEqual([2]);
  expect((await store.search('kind of')).map((e) => e.file.id)).toEqual([2]);
  expect(await store.search('')).toHaveLength(2);
});

test('recent gives distinct files', async () => {
  const store = freshStore();
  await store.record(mediaFile(1), 1);
  await store.record(mediaFile(2), 2);
  await store.record(mediaFile(1), 3);
  expect((await store.recent(5)).map((f) => f.id)).toEqual([1, 2]);
  expect(await store.recent(1)).toHaveLength(1);
});

test('drops the oldest plays past the limit', async () => {
  const store = freshStore(3);
  for (let i = 1; i <= 5; i += 1) await store.record(mediaFile(i), i);
  expect((await store.list()).map((e) => e.file.id)).toEqual([5, 4, 3]);
});

test('removes one play and clears all', async () => {
  const store = freshStore();
  const first = await store.record(mediaFile(1), 1);
  await store.record(mediaFile(2), 2);
  await store.remove(first.id ?? -1);
  expect(await store.list()).toHaveLength(1);
  await store.clear();
  expect(await store.list()).toHaveLength(0);
});
