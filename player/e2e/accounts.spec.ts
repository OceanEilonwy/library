import { expect, test } from '@playwright/test';
import { PASSWORD, signIn } from './helpers';

test('a new visitor makes an account, signs out and signs back in', async ({ page }) => {
  await page.goto('/player/');
  await expect(page.getByRole('heading', { name: 'Sign in' })).toBeVisible();
  await page.getByRole('button', { name: 'Make an account' }).click();

  // The form checks the rules before sending.
  await page.getByLabel('Username').fill('no');
  await page.getByLabel('Email').fill('newbie@example.org');
  await page.getByLabel('Password').fill('short');
  await page.getByRole('button', { name: 'Make account' }).click();
  await expect(page.getByText('Use 3 to 32 characters.')).toBeVisible();
  await expect(page.getByText('Use at least 8 characters.')).toBeVisible();

  await page.getByLabel('Username').fill('newbie');
  await page.getByLabel('Password').fill(PASSWORD);
  await page.getByRole('button', { name: 'Make account' }).click();
  await expect(page.getByRole('navigation').first()).toBeVisible();

  const me = await page.request.get('/api/account/me');
  expect(me.status()).toBe(200);
  expect(await me.json()).toMatchObject({ username: 'newbie', access: 'child' });

  await page.goto('/player/settings');
  await page.getByRole('button', { name: 'Sign out' }).click();
  await expect(page.getByRole('heading', { name: 'Sign in' })).toBeVisible();
  expect((await page.request.get('/api/account/me')).status()).toBe(401);

  // A wrong password is turned away; the right one gets in.
  await page.getByLabel('Username or email').fill('newbie');
  await page.getByLabel('Password').fill('not-the-password');
  await page.getByRole('button', { name: 'Sign in' }).click();
  await expect(page.getByRole('alert')).toContainText("doesn't match");

  await signIn(page, 'newbie@example.org');
  expect((await page.request.get('/api/account/me')).status()).toBe(200);
});

test('a username that is taken is reported on its field', async ({ page }) => {
  await page.goto('/player/account/register');
  await page.getByLabel('Username').fill('kid');
  await page.getByLabel('Email').fill('another@example.org');
  await page.getByLabel('Password').fill(PASSWORD);
  await page.getByRole('button', { name: 'Make account' }).click();
  await expect(page.locator('#username-error')).toBeVisible();
});
