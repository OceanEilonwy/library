import { expect, test } from '@playwright/test';

/** A short mono 16-bit WAV of a sine tone. */
function wav(frequency: number, seconds = 1, rate = 8000): Buffer {
  const samples = seconds * rate;
  const buffer = Buffer.alloc(44 + samples * 2);
  buffer.write('RIFF', 0);
  buffer.writeUInt32LE(36 + samples * 2, 4);
  buffer.write('WAVEfmt ', 8);
  buffer.writeUInt32LE(16, 16);
  buffer.writeUInt16LE(1, 20); // PCM
  buffer.writeUInt16LE(1, 22); // mono
  buffer.writeUInt32LE(rate, 24);
  buffer.writeUInt32LE(rate * 2, 28);
  buffer.writeUInt16LE(2, 32);
  buffer.writeUInt16LE(16, 34);
  buffer.write('data', 36);
  buffer.writeUInt32LE(samples * 2, 40);
  for (let n = 0; n < samples; n++) {
    buffer.writeInt16LE(Math.round(8000 * Math.sin((2 * Math.PI * frequency * n) / rate)), 44 + n * 2);
  }
  return buffer;
}

test('the librarian uploads ripped tracks in the studio', async ({ page }) => {
  await page.goto('/unlock?next=/');
  const password = page.locator('#unlock-password-page');
  await password.fill('librarian');
  await password.press('Enter');
  await expect(page).toHaveURL(/\/$/);

  // A new, empty music entry to upload into.
  const created = await page.request.post('/admin/items', {
    multipart: { type_id: '2', title: 'Studio Session', creator: 'The Uploaders', keep_duplicate: 'yes' },
  });
  const id = /\/items\/(\d+)/.exec(created.url())?.[1];
  expect(id).toBeTruthy();

  await page.goto(`/admin/items/${id}/media`);
  await expect(page.getByText('No files yet. Add some above.')).toBeVisible();
  await page.locator('#pick-files').setInputFiles([
    { name: '01 - First Light.wav', mimeType: 'audio/wav', buffer: wav(440) },
    { name: '02 - Second Wind.wav', mimeType: 'audio/wav', buffer: wav(550) },
  ]);

  // Titles and numbers are read from the file names.
  const rows = page.locator('#pending-rows tr');
  await expect(rows).toHaveCount(2);
  await expect(page.locator('#pending-rows input[value="First Light"]')).toHaveCount(1);
  await expect(page.locator('#pending-rows input[value="Second Wind"]')).toHaveCount(1);

  // Slow the uploads a little so their progress can be seen.
  await page.route('**/media/upload', async (route) => {
    await new Promise((wake) => setTimeout(wake, 400));
    await route.continue();
  });
  await page.getByRole('button', { name: 'Upload all' }).click();
  await expect(page.locator('#upload-status')).toHaveText(/Uploading [12] of 2/);
  // Each file goes up on its own request; finished rows leave the list.
  await expect(page.locator('#upload-status')).toHaveText('All 2 files uploaded.');
  await expect(rows).toHaveCount(0);
  await expect(page.locator('#media-list')).toHaveAttribute('data-count', '2');
  await expect(page.locator('#media-list input[name="title"][value="First Light"]')).toHaveCount(1);
  await expect(page.locator('#media-list input[name="title"][value="Second Wind"]')).toHaveCount(1);

  // Renaming a file through the htmx list.
  const first = page.locator('#media-list form', { has: page.locator('input[value="First Light"]') }).first();
  await first.locator('input[name="title"]').fill('First Light (Remastered)');
  await first.getByRole('button', { name: 'Save' }).click();
  await expect(page.locator('#media-list input[name="title"][value="First Light (Remastered)"]')).toHaveCount(1);

  // The files reach the Player, with their lengths measured in the browser.
  const login = await page.request.post('/api/account/login', { data: { login: 'adult', password: 'password123' } });
  expect(login.status()).toBe(200);
  const item = (await (await page.request.get(`/api/items/${id}`)).json()) as {
    media: { title: string; kind: string; track: number; durationMs: number | null }[];
  };
  expect(item.media.map((file) => [file.title, file.kind, file.track])).toEqual([
    ['First Light (Remastered)', 'audio', 1],
    ['Second Wind', 'audio', 2],
  ]);
  for (const file of item.media) expect(file.durationMs).toBeGreaterThan(900);
});
