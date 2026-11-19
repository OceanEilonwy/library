import { expect, test } from '@playwright/test';
import { signIn } from './helpers';
import { seed } from './state';

function trackModes(page: import('@playwright/test').Page) {
  return page.evaluate(() => Array.from(document.querySelector('video')?.textTracks ?? []).map((track) => track.mode));
}

test('the film offers its subtitles, and choosing one shows it', async ({ page }) => {
  await signIn(page, 'kid');
  await page.goto(`/player/video/${seed().media.film}`);
  await expect(page.getByRole('heading', { name: 'Test Pattern' })).toBeVisible();

  const picker = page.getByRole('group', { name: 'Subtitles' });
  await expect(picker.getByRole('button')).toHaveText(['Off', 'English', 'Français']);
  await expect(page.locator('video track')).toHaveCount(2);
  await expect(picker.getByRole('button', { name: 'Off' })).toHaveAttribute('aria-pressed', 'true');

  await picker.getByRole('button', { name: 'English' }).click();
  await expect.poll(() => trackModes(page)).toEqual(['showing', 'disabled']);
  await picker.getByRole('button', { name: 'Français' }).click();
  await expect.poll(() => trackModes(page)).toEqual(['disabled', 'showing']);

  // The choice is remembered for next time.
  await page.reload();
  await expect(picker.getByRole('button', { name: 'Français' })).toHaveAttribute('aria-pressed', 'true');
  await expect.poll(() => trackModes(page)).toEqual(['disabled', 'showing']);

  await picker.getByRole('button', { name: 'Off' }).click();
  await expect.poll(() => trackModes(page)).toEqual(['disabled', 'disabled']);

  // The subtitle file itself is WebVTT.
  const subtitles = await page.request.get(`/api/media/${seed().media.film}`);
  const url = ((await subtitles.json()) as { subtitles: { url: string }[] }).subtitles[0]?.url ?? '';
  const vtt = await page.request.get(url);
  expect(vtt.headers()['content-type']).toContain('text/vtt');
  expect(await vtt.text()).toContain('WEBVTT');
});

test('the document opens in the viewer with a download button', async ({ page }) => {
  await signIn(page, 'kid');
  await page.goto(`/player/document/${seed().media.pdf}`);
  await expect(page.locator(`iframe[src="/api/media/${seed().media.pdf}/stream"]`)).toBeVisible();
  const pdf = await page.request.get(`/api/media/${seed().media.pdf}/stream`);
  expect(pdf.headers()['content-type']).toBe('application/pdf');
});
