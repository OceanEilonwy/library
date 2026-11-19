import { expect, test } from '@playwright/test';
import { audio, itemPath, navigate, playedPast, signIn, trackMenu } from './helpers';
import { seed } from './state';

test.beforeEach(async ({ page }) => {
  await signIn(page, 'adult');
  await page.goto(itemPath(seed().items.sineSongs));
  await expect(page.getByRole('heading', { name: 'Sine Songs' })).toBeVisible();
});

test('plays an album with next, previous and the 10-second jumps', async ({ page }) => {
  await page.getByRole('button', { name: 'Play', exact: true }).first().click();
  await playedPast(page, 0.5);
  await expect(page.locator('.mini-title')).toHaveText('Morning (A)');
  expect((await audio(page)).src).toContain(`/api/media/${seed().media.songs[0]}/stream`);

  await page.getByRole('button', { name: /^Open Now Playing/ }).click();
  const nowPlaying = page.getByRole('dialog', { name: 'Now playing' });
  await expect(nowPlaying.locator('.now-title')).toHaveText('Morning (A)');

  const before = (await audio(page)).time;
  await nowPlaying.getByRole('button', { name: 'Forward 10 seconds' }).click();
  await expect.poll(async () => (await audio(page)).time).toBeGreaterThan(before + 9);
  const later = (await audio(page)).time;
  await nowPlaying.getByRole('button', { name: 'Back 10 seconds' }).click();
  await expect.poll(async () => (await audio(page)).time).toBeLessThan(later - 8);

  // More than 3 seconds in, "Previous" starts the track again.
  await playedPast(page, 3.5);
  await nowPlaying.getByRole('button', { name: 'Previous' }).click();
  await expect.poll(async () => (await audio(page)).time).toBeLessThan(3);
  await expect(nowPlaying.locator('.now-title')).toHaveText('Morning (A)');

  await nowPlaying.getByRole('button', { name: 'Next' }).click();
  await expect(nowPlaying.locator('.now-title')).toHaveText('Noon (C)');
  await playedPast(page, 0.3);
  expect((await audio(page)).src).toContain(`/api/media/${seed().media.songs[1]}/stream`);

  // Near the start, "Previous" goes back a track.
  await nowPlaying.getByRole('button', { name: 'Previous' }).click();
  await expect(nowPlaying.locator('.now-title')).toHaveText('Morning (A)');

  await nowPlaying.getByRole('button', { name: 'Pause' }).click();
  await expect.poll(async () => (await audio(page)).paused).toBe(true);
});

test('"Play next" and "Add to queue" set the order of Up next, which can be rearranged', async ({ page }) => {
  await page.getByRole('button', { name: 'Play', exact: true }).first().click();
  await playedPast(page, 0.2);

  await trackMenu(page, 'Evening (E)', 'Play next');
  await trackMenu(page, 'Morning (A)', 'Add to queue');

  await page.getByRole('button', { name: /^Open Now Playing/ }).click();
  const nowPlaying = page.getByRole('dialog', { name: 'Now playing' });
  await nowPlaying.getByRole('tab', { name: 'Up next' }).click();
  const upNext = nowPlaying.locator('ol.reorderable .track-title');
  await expect(upNext).toHaveText(['Evening (E)', 'Noon (C)', 'Evening (E)', 'Morning (A)']);

  // Keyboard reordering on the drag handle.
  await nowPlaying
    .getByRole('button', { name: /^Move “Evening \(E\)”/ })
    .first()
    .focus();
  await page.keyboard.press('ArrowDown');
  await expect(upNext).toHaveText(['Noon (C)', 'Evening (E)', 'Evening (E)', 'Morning (A)']);

  // Next plays the head of Up next.
  await nowPlaying.getByRole('tab', { name: 'Now playing' }).click();
  await nowPlaying.getByRole('button', { name: 'Next' }).click();
  await expect(nowPlaying.locator('.now-title')).toHaveText('Noon (C)');
});

test('a track played for 10 seconds shows in History', async ({ page }) => {
  await page.getByRole('button', { name: 'Play', exact: true }).first().click();
  await playedPast(page, 11, 25_000);
  await navigate(page, 'History');
  await expect(page.getByRole('heading', { name: 'History' })).toBeVisible();
  await expect(page.locator('.track-title', { hasText: 'Morning (A)' })).toBeVisible();

  // History is searchable.
  await page.getByRole('searchbox').fill('noon');
  await expect(page.locator('.track-title', { hasText: 'Morning (A)' })).toHaveCount(0);
});

test('a favourite is kept on the server', async ({ page }) => {
  const add = page.getByRole('button', { name: 'Add “Noon (C)” to favourites' });
  await add.click();
  const remove = page.getByRole('button', { name: 'Remove “Noon (C)” from favourites' });
  await expect(remove).toHaveAttribute('aria-pressed', 'true');

  await page.reload();
  await expect(remove).toHaveAttribute('aria-pressed', 'true');
  await navigate(page, 'Favourites');
  await expect(page.locator('.track-title', { hasText: 'Noon (C)' })).toBeVisible();

  // Tidy up for the other tests.
  await page.getByRole('button', { name: 'Remove “Noon (C)” from favourites' }).click();
  await expect(page.locator('.track-title', { hasText: 'Noon (C)' })).toHaveCount(0);
});
