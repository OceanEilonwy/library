import { expect, test } from '@playwright/test';
import { PASSWORD, itemPath, trackMenu } from './helpers';
import { seed } from './state';

test('make a playlist, fill it from an album, reorder, rename and delete it', async ({ page, browser }) => {
  // A fresh account, so the other tests' playlists stay as seeded.
  const made = await page.request.post('/api/account/register', {
    data: { username: 'listmaker', email: 'listmaker@example.org', password: PASSWORD },
  });
  expect(made.status()).toBe(201);

  await page.goto('/player/playlists');
  await page.getByPlaceholder('Name a new playlist').fill('Road trip');
  await page.getByRole('button', { name: 'New playlist' }).click();
  await expect(page.getByText('Road trip').first()).toBeVisible();

  await page.goto(itemPath(seed().items.sineSongs));
  for (const title of ['Noon (C)', 'Evening (E)']) {
    await trackMenu(page, title, 'Add to playlist…');
    const sheet = page.getByRole('dialog', { name: /to a playlist/ });
    await sheet.getByRole('button', { name: /Road trip/ }).click();
    await expect(sheet).toHaveCount(0);
  }

  await page.goto('/player/playlists');
  await page.getByText('Road trip').first().click();
  await expect(page.getByRole('heading', { name: 'Road trip' })).toBeVisible();
  const titles = page.locator('.track-title');
  await expect(titles).toHaveText(['Noon (C)', 'Evening (E)']);

  // Reordering is saved on the server.
  await page.getByRole('button', { name: /^Move “Noon \(C\)”/ }).focus();
  await page.keyboard.press('ArrowDown');
  await expect(titles).toHaveText(['Evening (E)', 'Noon (C)']);
  await expect(async () => {
    await page.reload();
    await expect(titles).toHaveText(['Evening (E)', 'Noon (C)'], { timeout: 2000 });
  }).toPass();

  await trackMenu(page, 'Evening (E)', 'Remove from this playlist');
  await expect(titles).toHaveText(['Noon (C)']);

  await page.getByRole('button', { name: 'Rename' }).click();
  await page.getByLabel('Playlist name').fill('Long road trip');
  await page.getByRole('button', { name: 'Save' }).click();
  await expect(page.getByRole('heading', { name: 'Long road trip' })).toBeVisible();

  // Someone else can see it, but not change it.
  const id = Number(/\/playlist\/(\d+)/.exec(page.url())?.[1]);
  const other = await browser.newContext({ baseURL: new URL(page.url()).origin });
  try {
    const login = await other.request.post('/api/account/login', { data: { login: 'kid', password: PASSWORD } });
    expect(login.status()).toBe(200);
    const seen = await other.request.get(`/api/playlists/${id}`);
    expect(seen.status()).toBe(200);
    const denied = await other.request.patch(`/api/playlists/${id}`, { data: { name: 'Mine now' } });
    expect(denied.status()).toBe(403);
  } finally {
    await other.close();
  }

  await page.getByRole('button', { name: 'Delete' }).click();
  await page.getByRole('button', { name: 'Tap again to delete' }).click();
  await page.goto('/player/playlists');
  await expect(page.getByText('Long road trip')).toHaveCount(0);
});
