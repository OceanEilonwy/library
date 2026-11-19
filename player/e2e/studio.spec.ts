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

test('the studio fills disc, track and title from MusicBrainz', async ({ page }) => {
  await page.goto('/unlock?next=/');
  const password = page.locator('#unlock-password-page');
  await password.fill('librarian');
  await password.press('Enter');
  await expect(page).toHaveURL(/\/$/);

  const created = await page.request.post('/admin/items', {
    multipart: { type_id: '2', title: 'Studio Session', creator: 'The Uploaders', keep_duplicate: 'yes' },
  });
  const id = /\/items\/(\d+)/.exec(created.url())?.[1];
  expect(id).toBeTruthy();

  await page.goto(`/admin/items/${id}/media`);
  const panel = page.locator('#mb-fill');
  await expect(panel).toBeHidden();
  await expect(page.locator('#mb-query')).toHaveValue('The Uploaders Studio Session');

  // A ripper that numbered the tracks wrongly: only the lengths tell.
  // The original pressing's tracks are 0:12, 0:25 and 0:40 long.
  await page.locator('#pick-files').setInputFiles([
    { name: 'Track 1.wav', mimeType: 'audio/wav', buffer: wav(330, 40) },
    { name: 'Track 2.wav', mimeType: 'audio/wav', buffer: wav(440, 12) },
    { name: 'Track 3.wav', mimeType: 'audio/wav', buffer: wav(550, 25) },
  ]);
  await expect(panel).toBeVisible();
  const titleOf = (file: string) =>
    page.locator('#pending-rows tr', { has: page.getByText(file, { exact: true }) }).locator('input[data-field="title"]');
  const trackOf = (file: string) =>
    page.locator('#pending-rows tr', { has: page.getByText(file, { exact: true }) }).locator('input[data-field="track"]');
  // A title typed by hand before filling in.
  await titleOf('Track 3.wav').fill('My Own Title');

  // Once the lengths are measured it searches by itself; the original
  // comes first although MusicBrainz listed the remaster first.
  const candidates = page.locator('#mb-candidates li');
  await expect(candidates).toHaveCount(3);
  await expect(candidates.first()).toContainText('Studio Session');
  await expect(candidates.first()).toContainText('Best fit');
  await expect(candidates.first().locator('.mb-fit')).toHaveText('Fits your 3 files');
  await expect(candidates.first()).toContainText('2001 · GB · CD · 3 tracks · 1:17');
  await expect(candidates.nth(1)).toContainText('Deluxe Edition');
  await expect(candidates.nth(1).locator('.mb-fit')).toHaveText('3 of your 3 files fit · 6 tracks');
  await expect(candidates.nth(2)).toContainText('Remastered');
  await expect(candidates.nth(2).locator('.mb-fit')).toHaveText('0 of your 3 files fit');

  // The remaster's preview leaves every file alone: no length fits.
  await candidates.nth(2).locator('button.mb-pick').click();
  const preview = page.locator('#mb-preview');
  await expect(preview.locator('tbody tr')).toHaveCount(3);
  await expect(preview.locator('.mb-conf.none')).toHaveCount(3);
  await expect(preview.getByRole('button', { name: 'Nothing to apply' })).toBeDisabled();

  // The original: each file to its track, by length.
  await candidates.first().locator('button.mb-pick').click();
  await expect(preview.locator('.mb-conf.good')).toHaveCount(3);
  await expect(preview.locator('tbody tr').first()).toContainText('Track 2.wav');
  await expect(preview.locator('tbody tr').first()).toContainText('Opening');
  await expect(preview.getByLabel("Also set this entry's year to 2001")).toBeChecked();
  await preview.getByRole('button', { name: 'Apply to 3 files' }).click();

  await expect(page.locator('#mb-status')).toContainText('Filled 3 files from “Studio Session”');
  await expect(page.locator('#mb-status')).toContainText("The entry's year was filled in too.");
  await expect(preview).toBeHidden();
  // Rows are now in track order; the hand-typed title was kept.
  await expect(page.locator('#pending-rows tr .path')).toHaveText(['Track 2.wav', 'Track 3.wav', 'Track 1.wav']);
  await expect(titleOf('Track 2.wav')).toHaveValue('Opening');
  await expect(titleOf('Track 3.wav')).toHaveValue('My Own Title');
  await expect(titleOf('Track 1.wav')).toHaveValue('Closing Time');
  await expect(trackOf('Track 1.wav')).toHaveValue('3');
  await expect(page.locator('#pending-rows .mb-mark.mb-good')).toHaveCount(3);

  // Filling again with "overwrite my edits" replaces the typed title.
  await candidates.first().locator('button.mb-pick').click();
  await preview.getByLabel("Overwrite titles and numbers I've typed myself").check();
  await preview.getByRole('button', { name: 'Apply to 3 files' }).click();
  await expect(titleOf('Track 3.wav')).toHaveValue('Long Middle');

  await page.getByRole('button', { name: 'Upload all' }).click();
  await expect(page.locator('#upload-status')).toHaveText('All 3 files uploaded.');
  await expect(panel).toBeHidden();

  const login = await page.request.post('/api/account/login', { data: { login: 'adult', password: 'password123' } });
  expect(login.status()).toBe(200);
  const item = (await (await page.request.get(`/api/items/${id}`)).json()) as {
    year: number | null;
    media: { title: string; disc: number; track: number }[];
  };
  expect(item.year).toBe(2001);
  expect(item.media.map((file) => [file.disc, file.track, file.title])).toEqual([
    [1, 1, 'Opening'],
    [1, 2, 'Long Middle'],
    [1, 3, 'Closing Time'],
  ]);
});
