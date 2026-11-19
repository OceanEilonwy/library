// Small formatting helpers shared by the screens.

/** "3:05" or "1:02:03" for a number of seconds. Negative or missing values show "0:00". */
export function formatClock(seconds: number | null | undefined): string {
  const total = Number.isFinite(seconds) && seconds && seconds > 0 ? Math.floor(seconds) : 0;
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const secs = total % 60;
  const ss = String(secs).padStart(2, '0');
  return hours > 0 ? `${hours}:${String(minutes).padStart(2, '0')}:${ss}` : `${minutes}:${ss}`;
}

/** Clock format for a duration in milliseconds, or an empty string when unknown. */
export function formatTrackLength(ms: number | null | undefined): string {
  return ms == null ? '' : formatClock(ms / 1000);
}

/** "42 min" or "1 hr 5 min" for longer spans, used for albums and playlists. */
export function formatSpan(ms: number | null | undefined): string {
  if (ms == null || ms <= 0) return '';
  const minutes = Math.round(ms / 60000);
  if (minutes < 60) return `${Math.max(1, minutes)} min`;
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  return rest ? `${hours} hr ${rest} min` : `${hours} hr`;
}

/** "4.2 MB" style sizes. */
export function formatBytes(bytes: number | null | undefined): string {
  if (bytes == null || !Number.isFinite(bytes) || bytes <= 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let value = bytes;
  let unit = 0;
  while (value >= 1000 && unit < units.length - 1) {
    value /= 1000;
    unit += 1;
  }
  const digits = value >= 100 || unit === 0 ? 0 : 1;
  return `${value.toFixed(digits)} ${units[unit]}`;
}

/** "1 track", "3 tracks". */
export function plural(count: number, one: string, many = `${one}s`): string {
  return `${count} ${count === 1 ? one : many}`;
}

/** Local calendar day key, e.g. "2026-11-19". */
export function dayKey(time: number): string {
  const date = new Date(time);
  const month = String(date.getMonth() + 1).padStart(2, '0');
  const day = String(date.getDate()).padStart(2, '0');
  return `${date.getFullYear()}-${month}-${day}`;
}

/** "Today", "Yesterday", or a date like "Monday 3 November" (with the year if not this year). */
export function dayLabel(time: number, now: number = Date.now()): string {
  const key = dayKey(time);
  if (key === dayKey(now)) return 'Today';
  const yesterday = new Date(now);
  yesterday.setDate(yesterday.getDate() - 1);
  if (key === dayKey(yesterday.getTime())) return 'Yesterday';
  const date = new Date(time);
  const sameYear = date.getFullYear() === new Date(now).getFullYear();
  return date.toLocaleDateString('en-GB', {
    weekday: 'long',
    day: 'numeric',
    month: 'long',
    ...(sameYear ? {} : { year: 'numeric' }),
  });
}

export type DayGroup<T> = { key: string; label: string; entries: T[] };

/** Groups entries (already newest first) by local day, keeping their order. */
export function groupByDay<T>(
  entries: readonly T[],
  timeOf: (entry: T) => number,
  now: number = Date.now(),
): DayGroup<T>[] {
  const groups: DayGroup<T>[] = [];
  for (const entry of entries) {
    const time = timeOf(entry);
    const key = dayKey(time);
    const last = groups[groups.length - 1];
    if (last && last.key === key) last.entries.push(entry);
    else groups.push({ key, label: dayLabel(time, now), entries: [entry] });
  }
  return groups;
}
