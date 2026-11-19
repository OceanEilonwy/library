import { expect, test } from '@playwright/test';
import { audio, itemPath, playedPast, serviceWorkerReady, signIn, trackMenu } from './helpers';
import { seed } from './state';

test('a downloaded track plays offline from the Downloads screen, and Clear all removes it', async ({
  page,
  context,
}) => {
  await signIn(page, 'adult');
  await serviceWorkerReady(page);
  await page.goto(itemPath(seed().items.sineSongs));
  await trackMenu(page, 'Morning (A)', 'Download');
  await expect(page.locator('.track', { hasText: 'Morning (A)' }).locator('.download-mark.done')).toBeVisible();

  await page.goto('/player/downloads');
  await expect(page.locator('.download-group', { hasText: 'Sine Songs' })).toBeVisible();

  // Offline: the app itself, the account and the download all come from the device.
  await context.setOffline(true);
  await page.reload();
  await expect(page.getByRole('heading', { name: 'Downloads' })).toBeVisible();
  await page.getByRole('button', { name: 'Play Sine Songs' }).click();
  await playedPast(page, 1);
  expect((await audio(page)).src).toContain(`/api/media/${seed().media.songs[0]}/stream`);
  await expect(page.locator('.mini-title')).toHaveText('Morning (A)');

  // Seeking works from the cache too (the worker answers Range requests).
  await page.evaluate(() => {
    (document.getElementById('player-audio') as HTMLAudioElement).currentTime = 20;
  });
  await playedPast(page, 20.2);

  await context.setOffline(false);
  await page.getByRole('button', { name: 'Pause' }).click();
  await page.getByRole('button', { name: 'Clear all downloads' }).click();
  await page.getByRole('button', { name: 'Tap again to clear' }).first().click();
  await expect(page.getByText('Nothing downloaded yet')).toBeVisible();
  const cached = await page.evaluate(async () => (await (await caches.open('media-downloads')).keys()).length);
  expect(cached).toBe(0);
});
