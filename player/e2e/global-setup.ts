// Seeds a throwaway data folder with scripts/dev-seed.py (accounts, items,
// files uploaded through the real studio endpoint, playlists) and starts the
// Rust server on it, with its online lookups pointed at a pretend
// MusicBrainz (e2e/mock-musicbrainz.ts) that runs in this process. It builds nothing but the server binary, and only if
// cargo thinks it is out of date; run `npm run build` first for the Player.

import { spawn, spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, openSync, writeFileSync } from 'node:fs';
import { createConnection } from 'node:net';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { startMockMusicBrainz } from './mock-musicbrainz';
import { REPO, STATE_FILE, type E2EState } from './state';

function portOpen(port: number): Promise<boolean> {
  return new Promise((done) => {
    const socket = createConnection({ host: '127.0.0.1', port });
    socket.once('connect', () => {
      socket.destroy();
      done(true);
    });
    socket.once('error', () => done(false));
  });
}

function binary(): string {
  if (process.env.LIBRARY_BIN) return resolve(process.env.LIBRARY_BIN);
  const build = spawnSync('cargo', ['build', '--quiet'], { cwd: REPO, stdio: 'inherit' });
  if (build.status !== 0) throw new Error('cargo build failed');
  const meta = spawnSync('cargo', ['metadata', '--format-version', '1', '--no-deps'], { cwd: REPO, encoding: 'utf8' });
  if (meta.status !== 0) throw new Error('cargo metadata failed');
  const target = (JSON.parse(meta.stdout) as { target_directory: string }).target_directory;
  return join(target, 'debug', 'library');
}

async function waitForHealth(base: string, seconds = 60): Promise<void> {
  const deadline = Date.now() + seconds * 1000;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(`${base}/healthz`);
      if (response.ok) return;
    } catch {
      // Not up yet.
    }
    await new Promise((wake) => setTimeout(wake, 200));
  }
  throw new Error(`the server at ${base} didn't start`);
}

export default async function globalSetup(): Promise<void> {
  if (!existsSync(join(REPO, 'player', 'dist', 'index.html'))) {
    throw new Error('The Player has not been built: run `npm run build` in player/ first.');
  }
  const port = Number(process.env.E2E_PORT);
  if (await portOpen(port)) throw new Error(`port ${port} is in use; set E2E_PORT to a free port`);

  const musicbrainz = await startMockMusicBrainz();
  // Closed by the global teardown, which runs in this same process.
  (globalThis as { mockMusicBrainz?: typeof musicbrainz }).mockMusicBrainz = musicbrainz;

  const root = mkdtempSync(join(tmpdir(), 'library-e2e-'));
  const dataDir = join(root, 'data-e2e');
  const config = join(root, 'library.toml');
  // The admin password is "librarian" (the same hash as library.dev.toml).
  writeFileSync(
    config,
    [
      `listen = "127.0.0.1:${port}"`,
      `data_dir = ${JSON.stringify(dataDir)}`,
      `admin_password_hash = "$argon2id$v=19$m=19456,t=2,p=1$ixuLexE66OMO5deZ18kDOA$zM3BsRH54lPgODZox/FF2pNSsGzB3WmdGrrTbVQ34Xs"`,
      '[semantic]',
      'enabled = false',
      '[lookup]',
      // Every source points at the pretend MusicBrainz: nothing leaves the machine.
      'enabled = true',
      `musicbrainz_url = "${musicbrainz.url}/ws/2"`,
      'musicbrainz_interval_ms = 50',
      `open_library_url = "${musicbrainz.url}/none"`,
      `google_books_url = "${musicbrainz.url}/none"`,
      `tmdb_url = "${musicbrainz.url}/none"`,
      `upc_url = "${musicbrainz.url}/none"`,
      '',
    ].join('\n'),
  );

  const bin = binary();
  const seed = spawnSync('python3', [join(REPO, 'scripts', 'dev-seed.py'), '--config', config, '--bin', bin], {
    cwd: REPO,
    stdio: 'inherit',
  });
  if (seed.status !== 0) throw new Error('seeding failed');

  const log = openSync(join(root, 'server.log'), 'a');
  const server = spawn(bin, ['--config', config, 'serve'], {
    cwd: REPO,
    stdio: ['ignore', log, log],
    env: { ...process.env, RUST_LOG: 'library=info' },
    detached: false,
  });
  if (server.pid === undefined) throw new Error('could not start the server');
  server.unref();
  await waitForHealth(`http://127.0.0.1:${port}`);

  const state: E2EState = { root, pid: server.pid, port };
  writeFileSync(STATE_FILE, JSON.stringify(state));
  process.stdout.write(`e2e: server on port ${port}, data in ${dataDir} (log: ${join(root, 'server.log')})\n`);
}
