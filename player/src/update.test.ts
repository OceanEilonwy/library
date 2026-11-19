import { shouldActivate, updateStatusText, type UpdateContext } from './update';

const base: UpdateContext = { waiting: true, playing: false, downloading: false, trigger: 'navigation' };

test('activates on navigation or hide when idle', () => {
  expect(shouldActivate(base)).toBe(true);
  expect(shouldActivate({ ...base, trigger: 'hidden' })).toBe(true);
});

test('never while playing or downloading', () => {
  expect(shouldActivate({ ...base, playing: true })).toBe(false);
  expect(shouldActivate({ ...base, downloading: true })).toBe(false);
  expect(shouldActivate({ ...base, playing: true, trigger: 'hidden' })).toBe(false);
});

test('nothing to do without a waiting worker', () => {
  expect(shouldActivate({ ...base, waiting: false })).toBe(false);
});

test('an idle check alone never reloads', () => {
  expect(shouldActivate({ ...base, trigger: 'idle-check' })).toBe(false);
});

test('status text is plain English', () => {
  expect(updateStatusText('ready')).toMatch(/new version is ready/);
  expect(updateStatusText('current')).toMatch(/latest/);
});
