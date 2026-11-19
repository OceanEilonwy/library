import { expect, test } from '@playwright/test';
import { itemPath, signIn } from './helpers';
import { seed } from './state';

test('the main screens load without errors or security-policy violations', async ({ page }) => {
  const problems: string[] = [];
  page.on('pageerror', (error) => problems.push(`page error: ${error.message}`));
  page.on('console', (message) => {
    if (message.type() !== 'error' && message.type() !== 'warning') return;
    const text = message.text();
    // Signed out at first: the start-up account check is expected to get 401.
    if (text.includes('401')) return;
    problems.push(`${message.type()}: ${text}`);
  });

  await signIn(page, 'adult');
  for (const path of [
    '/player/',
    '/player/library',
    itemPath(seed().items.sineSongs),
    '/player/playlists',
    '/player/favourites',
    '/player/history',
    '/player/downloads',
    '/player/settings',
    `/player/video/${seed().media.film}`,
    `/player/document/${seed().media.pdf}`,
  ]) {
    await page.goto(path);
    await expect(page.getByRole('heading', { level: 1 }).first()).toBeVisible();
  }
  expect(problems).toEqual([]);
});
