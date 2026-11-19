// SPDX-License-Identifier: AGPL-3.0-or-later
// Library Rip's side of the htmx site (docs/rip.md): the browser handshake
// on /connect, with this test playing the ripper's loopback listener, and
// the /ripper download page.

import { expect, test } from '@playwright/test';
import { createHash, randomBytes } from 'node:crypto';
import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createServer, type Server } from 'node:http';
import type { AddressInfo } from 'node:net';
import { join } from 'node:path';
import { STATE_FILE, type E2EState } from './state';

const base64url = (bytes: Buffer) => bytes.toString('base64url');

type Callback = { path: string; query: URLSearchParams };

/** A stand-in for the ripper's listener on 127.0.0.1:<random port>. */
async function loopbackListener(): Promise<{ server: Server; port: number; callback: Promise<Callback> }> {
  let deliver: (callback: Callback) => void = () => {};
  const callback = new Promise<Callback>((resolve) => {
    deliver = resolve;
  });
  const server = createServer((request, response) => {
    const url = new URL(request.url ?? '/', 'http://127.0.0.1');
    response.writeHead(200, { 'content-type': 'text/html; charset=utf-8' });
    response.end('<!doctype html><title>Library Rip</title><h1>You can close this tab</h1>');
    deliver({ path: url.pathname, query: url.searchParams });
  });
  await new Promise<void>((ready) => server.listen(0, '127.0.0.1', ready));
  return { server, port: (server.address() as AddressInfo).port, callback };
}

test('connecting Library Rip in the browser sends a code back to its listener', async ({ page }) => {
  const { server, port, callback } = await loopbackListener();
  try {
    const verifier = base64url(randomBytes(48));
    const challenge = base64url(createHash('sha256').update(verifier).digest());
    const state = base64url(randomBytes(16));
    const redirectUri = `http://127.0.0.1:${port}/callback`;
    const connect = new URLSearchParams({
      client: 'library-rip',
      name: 'Test desktop',
      redirect_uri: redirectUri,
      state,
      code_challenge: challenge,
      code_challenge_method: 'S256',
    });

    // Not unlocked yet: the librarian unlocks first and comes back.
    await page.goto(`/connect?${connect}`);
    await expect(page).toHaveURL(/\/unlock\?next=/);
    const password = page.locator('#unlock-password-page');
    await password.fill('librarian');
    await password.press('Enter');

    await expect(page.getByRole('heading', { name: 'Connect Library Rip' })).toBeVisible();
    await expect(page.getByText('Test desktop')).toBeVisible();
    await page.getByRole('button', { name: 'Yes, connect' }).click();

    // The browser lands on the listener with a one-time code and our state.
    const back = await callback;
    expect(back.path).toBe('/callback');
    expect(back.query.get('state')).toBe(state);
    const code = back.query.get('code');
    expect(code).toBeTruthy();
    expect(back.query.get('error')).toBeNull();
    await expect(page.getByRole('heading', { name: 'You can close this tab' })).toBeVisible();

    // The ripper swaps the code (with its verifier) for a key, once.
    const swap = () =>
      page.request.post('/api/rip/token', {
        data: { grantType: 'authorization_code', code, codeVerifier: verifier, redirectUri },
      });
    const token = await swap();
    expect(token.status()).toBe(200);
    const body = (await token.json()) as { apiKey: string; keyName: string };
    expect(body.apiKey).toMatch(/^lrk_/);
    expect(body.keyName).toBe('Test desktop');
    expect((await swap()).status()).toBe(400);

    // The key opens the rip API, and the desk lists the connection.
    const whoami = await page.request.get('/api/rip/whoami', {
      headers: { authorization: `Bearer ${body.apiKey}` },
    });
    expect(whoami.status()).toBe(200);
    await page.goto('/admin');
    await expect(page.getByRole('button', { name: 'Disconnect Test desktop' }).first()).toBeVisible();
  } finally {
    server.close();
  }
});

test('saying No sends access_denied back to the listener', async ({ page }) => {
  const { server, port, callback } = await loopbackListener();
  try {
    const challenge = base64url(
      createHash('sha256')
        .update(base64url(randomBytes(48)))
        .digest(),
    );
    const connect = new URLSearchParams({
      client: 'library-rip',
      name: 'Refused desktop',
      redirect_uri: `http://127.0.0.1:${port}/callback`,
      state: 'refused-state',
      code_challenge: challenge,
      code_challenge_method: 'S256',
    });
    await page.goto(`/connect?${connect}`);
    if (page.url().includes('/unlock')) {
      const password = page.locator('#unlock-password-page');
      await password.fill('librarian');
      await password.press('Enter');
    }
    await page.getByRole('button', { name: 'No', exact: true }).click();
    const back = await callback;
    expect(back.query.get('error')).toBe('access_denied');
    expect(back.query.get('state')).toBe('refused-state');
    expect(back.query.get('code')).toBeNull();
  } finally {
    server.close();
  }
});

test('/ripper lists the builds in the downloads folder', async ({ page }) => {
  const state = JSON.parse(readFileSync(STATE_FILE, 'utf8')) as E2EState;
  const downloads = join(state.root, 'data-e2e', 'downloads', 'library-rip');
  mkdirSync(downloads, { recursive: true });
  const name = 'library-rip-0.1.0-linux-x86_64.tar.gz';
  const contents = Buffer.from('not really a build, just bytes for the test\n');
  const file = join(downloads, name);
  writeFileSync(file, contents);
  // Checksum files are not offered as downloads.
  writeFileSync(join(downloads, 'SHA256SUMS.sha256'), 'ignored\n');
  try {
    await page.goto('/ripper');
    await expect(page.getByRole('heading', { name: 'Get Library Rip' })).toBeVisible();
    const link = page.getByRole('link', { name });
    await expect(link).toBeVisible();
    await expect(link).toHaveAttribute('href', `/ripper/download/${name}`);
    const sha256 = createHash('sha256').update(contents).digest('hex');
    await expect(page.getByText(`SHA-256 ${sha256}`)).toBeVisible();
    await expect(page.getByText('SHA256SUMS.sha256')).toHaveCount(0);
    await expect(page.getByRole('link', { name: 'Source code' }).first()).toBeVisible();

    const download = await page.request.get(`/ripper/download/${name}`);
    expect(download.status()).toBe(200);
    expect(Buffer.from(await download.body()).equals(contents)).toBe(true);
  } finally {
    rmSync(file, { force: true });
  }
});
