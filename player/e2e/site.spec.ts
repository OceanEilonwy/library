import { expect, test } from '@playwright/test';
import { seed } from './state';

test('the Player button on the catalogue opens the Player', async ({ page }) => {
  await page.goto('/');
  await page
    .getByRole('link', { name: /Player/ })
    .first()
    .click();
  await expect(page).toHaveURL(/\/player\/$/);
  await expect(page.getByRole('heading', { name: 'Sign in' })).toBeVisible();

  // And from an item page's compact header.
  await page.goto(`/items/${seed().items.sineSongs}`);
  await page
    .getByRole('link', { name: /Player/ })
    .first()
    .click();
  await expect(page).toHaveURL(/\/player\/$/);
});

test('the catalogue and the upload studio load without security-policy violations', async ({ page }) => {
  const problems: string[] = [];
  page.on('pageerror', (error) => problems.push(`page error: ${error.message}`));
  page.on('console', (message) => {
    if (message.type() === 'error' || message.type() === 'warning') problems.push(message.text());
  });
  await page.goto('/unlock?next=/');
  const password = page.locator('#unlock-password-page');
  await password.fill('librarian');
  await password.press('Enter');
  await expect(page).toHaveURL(/\/$/);
  for (const path of [
    '/',
    `/items/${seed().items.sineSongs}`,
    `/admin/items/${seed().items.film}/media`,
    '/admin/accounts',
  ]) {
    await page.goto(path);
    await expect(page.getByRole('heading', { level: 1 }).first()).toBeVisible();
  }
  // An htmx swap: change an account's access and back.
  await page.goto('/admin/accounts');
  const access = page.getByLabel('Access for kid');
  const swapped = page.waitForResponse((response) => response.url().endsWith('/access'));
  await access.selectOption('adult');
  await swapped;
  await expect(page.getByLabel('Access for kid')).toHaveValue('adult');
  const back = page.waitForResponse((response) => response.url().endsWith('/access'));
  await page.getByLabel('Access for kid').selectOption('child');
  await back;
  await expect(page.getByLabel('Access for kid')).toHaveValue('child');
  expect(problems).toEqual([]);
});
