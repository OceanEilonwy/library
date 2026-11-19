// Small steps the end-to-end tests share.

import { expect, type Page } from '@playwright/test';

export const PASSWORD = 'password123';

/** Signs in from the Player's welcome screen and waits for Home. */
export async function signIn(page: Page, login: string, password = PASSWORD): Promise<void> {
  await page.goto('/player/');
  await page.getByLabel('Username or email').fill(login);
  await page.getByLabel('Password').fill(password);
  await page.getByRole('button', { name: 'Sign in' }).click();
  await expect(page.getByRole('navigation').first()).toBeVisible();
  await expect(page.getByRole('heading', { level: 1 })).toBeVisible();
}

/** Goes to a Player screen with a client-side navigation (keeps the audio playing). */
export async function navigate(page: Page, label: string): Promise<void> {
  await page.getByRole('navigation').first().getByRole('link', { name: label, exact: true }).click();
}

/** The persistent audio element's state. */
export function audio(page: Page) {
  return page.evaluate(() => {
    const element = document.getElementById('player-audio') as HTMLAudioElement;
    return { time: element.currentTime, paused: element.paused, src: element.currentSrc };
  });
}

/** Waits until the audio has played past `seconds`. */
export async function playedPast(page: Page, seconds: number, timeout = 15_000): Promise<void> {
  await expect
    .poll(async () => (await audio(page)).time, { timeout, message: `audio should pass ${seconds}s` })
    .toBeGreaterThan(seconds);
}

/** Opens a track's "More options" menu and picks an item. */
export async function trackMenu(page: Page, title: string, choice: string): Promise<void> {
  await page
    .getByRole('button', { name: `More options for “${title}”` })
    .first()
    .click();
  await page.getByRole('menuitem', { name: choice, exact: true }).click();
}

/** Waits for the service worker to control the page. */
export async function serviceWorkerReady(page: Page): Promise<void> {
  await page.evaluate(async () => {
    await navigator.serviceWorker.ready;
    if (navigator.serviceWorker.controller) return;
    await new Promise<void>((done) =>
      navigator.serviceWorker.addEventListener('controllerchange', () => done(), { once: true }),
    );
  });
}

/** The ids of an item's or playlist's link in the Player. */
export function itemPath(id: number): string {
  return `/player/item/${id}`;
}
