import { dropIndex } from './reorder';

const rows = [0, 1, 2, 3].map((i) => ({ top: i * 50, bottom: i * 50 + 50 }));

test('dragging down lands below the row whose middle we passed', () => {
  expect(dropIndex(130, rows, 0)).toBe(2);
  expect(dropIndex(80, rows, 0)).toBe(1);
  expect(dropIndex(20, rows, 0)).toBe(0);
});

test('dragging up lands at the row whose middle we are above', () => {
  expect(dropIndex(10, rows, 3)).toBe(0);
  expect(dropIndex(70, rows, 3)).toBe(1);
});

test('past the end lands last; empty lists stay put', () => {
  expect(dropIndex(999, rows, 1)).toBe(3);
  expect(dropIndex(5, [], 2)).toBe(2);
});
