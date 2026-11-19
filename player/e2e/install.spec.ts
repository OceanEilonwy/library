import { expect, test } from '@playwright/test';
import { serviceWorkerReady } from './helpers';

test('the Player is installable: manifest, icons and a service worker for /player/', async ({ page, baseURL }) => {
  await page.goto('/player/');
  await expect(page.locator('link[rel="manifest"]')).toHaveAttribute('href', '/player/manifest.webmanifest');

  const response = await page.request.get('/player/manifest.webmanifest');
  expect(response.headers()['content-type']).toContain('application/manifest+json');
  const manifest = (await response.json()) as {
    name: string;
    display: string;
    scope: string;
    start_url: string;
    icons: { src: string; sizes: string; purpose: string }[];
  };
  expect(manifest).toMatchObject({
    name: 'Library Player',
    display: 'standalone',
    scope: '/player/',
    start_url: '/player/',
  });
  expect(manifest.icons.some((icon) => icon.sizes === '192x192')).toBe(true);
  expect(manifest.icons.some((icon) => icon.sizes === '512x512' && icon.purpose === 'maskable')).toBe(true);
  for (const icon of manifest.icons) expect((await page.request.get(icon.src)).status()).toBe(200);

  await serviceWorkerReady(page);
  const scope = await page.evaluate(async () => (await navigator.serviceWorker.ready).scope);
  expect(scope).toBe(`${baseURL}/player/`);

  const worker = await page.request.get('/player/sw.js');
  expect(worker.headers()['cache-control']).toBe('no-cache');
  expect(worker.headers()['service-worker-allowed']).toBe('/player/');

  // Client routes fall back to the app; missing files don't.
  expect((await page.request.get('/player/library')).status()).toBe(200);
  expect((await page.request.get('/player/missing.js')).status()).toBe(404);
});
