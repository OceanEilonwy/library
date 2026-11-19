import { expect, test } from '@playwright/test';
import { PASSWORD, approveAccount, signIn, unlockAsLibrarian } from './helpers';

test('a new visitor asks for an account, is approved by the librarian, then signs in', async ({ page, browser }) => {
  await page.goto('/player/');
  await expect(page.getByRole('heading', { name: 'Sign in' })).toBeVisible();
  await page.getByRole('button', { name: 'Make an account' }).click();

  // The form checks the rules before sending.
  await page.getByLabel('Username').fill('no');
  await page.getByLabel('Email').fill('newcomer@example.org');
  await page.getByLabel('Password').fill('short');
  await page.getByRole('button', { name: 'Make account' }).click();
  await expect(page.getByText('Use 3 to 32 characters.')).toBeVisible();
  await expect(page.getByText('Use at least 8 characters.')).toBeVisible();

  await page.getByLabel('Username').fill('newcomer');
  await page.getByLabel('Password').fill(PASSWORD);
  await page.getByRole('button', { name: 'Make account' }).click();

  // No sign-in yet: a librarian has to approve the account first.
  await expect(page.getByRole('heading', { name: 'Account requested' })).toBeVisible();
  await expect(page.getByText('A librarian needs to approve it before you can sign in.')).toBeVisible();
  expect((await page.request.get('/api/account/me')).status()).toBe(401);

  await page.getByRole('button', { name: 'Back to sign in' }).click();
  await page.getByLabel('Username or email').fill('newcomer');
  await page.getByLabel('Password').fill(PASSWORD);
  await page.getByRole('button', { name: 'Sign in' }).click();
  await expect(page.getByRole('alert')).toContainText('Your account is waiting for a librarian to approve it.');
  expect((await page.request.get('/api/account/me')).status()).toBe(401);

  await approveAccount(browser, 'newcomer');

  await signIn(page, 'newcomer');
  const me = await page.request.get('/api/account/me');
  expect(me.status()).toBe(200);
  expect(await me.json()).toMatchObject({ username: 'newcomer', access: 'child' });

  await page.goto('/player/settings');
  await page.getByRole('button', { name: 'Sign out' }).click();
  await expect(page.getByRole('heading', { name: 'Sign in' })).toBeVisible();
  expect((await page.request.get('/api/account/me')).status()).toBe(401);

  // A wrong password is turned away; the right one gets in.
  await page.getByLabel('Username or email').fill('newcomer');
  await page.getByLabel('Password').fill('not-the-password');
  await page.getByRole('button', { name: 'Sign in' }).click();
  await expect(page.getByRole('alert')).toContainText("doesn't match");

  await signIn(page, 'newcomer@example.org');
  expect((await page.request.get('/api/account/me')).status()).toBe(200);
});

test('the librarian sees waiting requests on the desk and can turn one down', async ({ page }) => {
  const made = await page.request.post('/api/account/register', {
    data: { username: 'drive-by', email: 'drive-by@example.org', password: PASSWORD },
  });
  expect(made.status()).toBe(202);
  expect(await made.json()).toMatchObject({ status: 'pending' });

  await unlockAsLibrarian(page);
  await page.goto('/admin');
  // The seeded "newbie" and "drive-by" are both waiting.
  await expect(page.getByRole('link', { name: /Player accounts/ }).locator('.count-badge')).toContainText('2');
  await page.getByRole('link', { name: /Player accounts/ }).click();
  await expect(page.getByRole('heading', { name: /Waiting for approval/ })).toBeVisible();
  await expect(page.getByText('newbie@example.org')).toBeVisible();

  page.once('dialog', (dialog) => void dialog.accept());
  await page.getByRole('button', { name: 'Reject drive-by' }).click();
  await expect(page.getByRole('status')).toContainText("Turned down drive-by's request");
  await expect(page.getByText('drive-by@example.org')).toHaveCount(0);
  await expect(page.getByText('newbie@example.org')).toBeVisible();

  const login = await page.request.post('/api/account/login', {
    data: { login: 'drive-by', password: PASSWORD },
  });
  expect(login.status()).toBe(401);
});

test('a username that is taken is reported on its field', async ({ page }) => {
  await page.goto('/player/account/register');
  await page.getByLabel('Username').fill('kid');
  await page.getByLabel('Email').fill('another@example.org');
  await page.getByLabel('Password').fill(PASSWORD);
  await page.getByRole('button', { name: 'Make account' }).click();
  await expect(page.locator('#username-error')).toBeVisible();
});
