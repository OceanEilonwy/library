import { expect, test } from '@playwright/test';
import { itemPath, signIn } from './helpers';
import { seed } from './state';

test.describe('a child account', () => {
  test.beforeEach(async ({ page }) => signIn(page, 'kid'));

  test('never sees the restricted album', async ({ page }) => {
    await page.goto('/player/library');
    await expect(page.getByText('Sine Songs').first()).toBeVisible();
    await expect(page.getByText('A Tiny Book').first()).toBeVisible();
    await expect(page.getByText('Late Night Tones')).toHaveCount(0);

    // Not even when asking for it directly.
    await page.goto(itemPath(seed().items.lateNightTones));
    await expect(page.getByText("That isn't in the library.")).toBeVisible();
    expect((await page.request.get(`/api/items/${seed().items.lateNightTones}`)).status()).toBe(404);
    expect((await page.request.get(`/api/media/${seed().media.lateTracks[0]}/stream`)).status()).toBe(404);
  });

  test('sees only the allowed tracks of a mixed playlist, and not an adults-only one', async ({ page }) => {
    await page.goto('/player/playlists');
    await expect(page.getByText('Saturday mix')).toBeVisible();
    await expect(page.getByText("Kid's picks")).toBeVisible();
    await expect(page.getByText('After hours')).toHaveCount(0);

    await page.getByText('Saturday mix').click();
    await expect(page.getByRole('heading', { name: 'Saturday mix' })).toBeVisible();
    const tracks = page.locator('.track-title');
    await expect(tracks).toHaveText(['Morning (A)', 'Evening (E)', 'Test Pattern']);
    await expect(page.getByText('Low Hum')).toHaveCount(0);

    // Their own playlist keeps the hidden track, but they don't see it.
    await page.goto('/player/playlists');
    await page.getByText("Kid's picks").click();
    await expect(page.locator('.track-title')).toHaveText(['Noon (C)', 'A Tiny Book']);
  });
});

test('an adult account sees the restricted album and every playlist', async ({ page }) => {
  await signIn(page, 'adult');
  await page.goto(itemPath(seed().items.lateNightTones));
  await expect(page.getByRole('heading', { name: 'Late Night Tones' })).toBeVisible();
  await expect(page.getByText('Adults only')).toBeVisible();

  await page.goto('/player/playlists');
  await expect(page.getByText('After hours')).toBeVisible();
  await page.getByText('After hours').click();
  await expect(page.locator('.track-title')).toHaveText(['Low Hum', 'Lower Hum']);

  await page.goto('/player/playlists');
  await page.getByText("Kid's picks").click();
  await expect(page.locator('.track-title')).toHaveText(['Noon (C)', 'Lower Hum', 'A Tiny Book']);
});
