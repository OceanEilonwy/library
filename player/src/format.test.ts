import { dayLabel, formatBytes, formatClock, formatSpan, formatTrackLength, groupByDay, plural } from './format';

test('formatClock', () => {
  expect(formatClock(0)).toBe('0:00');
  expect(formatClock(5.9)).toBe('0:05');
  expect(formatClock(185)).toBe('3:05');
  expect(formatClock(3723)).toBe('1:02:03');
  expect(formatClock(-4)).toBe('0:00');
  expect(formatClock(Number.NaN)).toBe('0:00');
  expect(formatClock(null)).toBe('0:00');
});

test('formatTrackLength', () => {
  expect(formatTrackLength(null)).toBe('');
  expect(formatTrackLength(241000)).toBe('4:01');
});

test('formatSpan', () => {
  expect(formatSpan(null)).toBe('');
  expect(formatSpan(20_000)).toBe('1 min');
  expect(formatSpan(42 * 60000)).toBe('42 min');
  expect(formatSpan(65 * 60000)).toBe('1 hr 5 min');
  expect(formatSpan(120 * 60000)).toBe('2 hr');
});

test('formatBytes', () => {
  expect(formatBytes(0)).toBe('0 B');
  expect(formatBytes(512)).toBe('512 B');
  expect(formatBytes(4_200_000)).toBe('4.2 MB');
  expect(formatBytes(1_500_000_000)).toBe('1.5 GB');
  expect(formatBytes(250_000_000)).toBe('250 MB');
});

test('plural', () => {
  expect(plural(1, 'track')).toBe('1 track');
  expect(plural(3, 'track')).toBe('3 tracks');
  expect(plural(2, 'copy', 'copies')).toBe('2 copies');
});

describe('days', () => {
  const now = new Date(2026, 10, 19, 15, 0).getTime();

  test('labels today, yesterday and older days', () => {
    expect(dayLabel(new Date(2026, 10, 19, 1, 0).getTime(), now)).toBe('Today');
    expect(dayLabel(new Date(2026, 10, 18, 23, 59).getTime(), now)).toBe('Yesterday');
    expect(dayLabel(new Date(2026, 10, 3, 12).getTime(), now)).toMatch(/3 November/);
    expect(dayLabel(new Date(2025, 0, 2, 12).getTime(), now)).toMatch(/2025/);
  });

  test('groups consecutive entries by day', () => {
    const times = [
      new Date(2026, 10, 19, 14).getTime(),
      new Date(2026, 10, 19, 9).getTime(),
      new Date(2026, 10, 18, 20).getTime(),
      new Date(2026, 10, 1, 20).getTime(),
    ];
    const groups = groupByDay(times, (time) => time, now);
    expect(groups.map((group) => group.label.split(' ')[0])).toEqual(['Today', 'Yesterday', 'Sunday']);
    expect(groups[0]?.entries).toHaveLength(2);
  });
});
