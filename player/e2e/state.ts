// Shared between the global setup, teardown and the tests.

import { readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

export const REPO = resolve(import.meta.dirname, '..', '..');
export const STATE_FILE = join(tmpdir(), `library-e2e-${process.env.E2E_PORT ?? '18431'}.json`);

export type E2EState = { root: string; pid: number; port: number };

/** Ids written by scripts/dev-seed.py. */
export type Seed = {
  items: { sineSongs: number; lateNightTones: number; film: number; book: number };
  media: { songs: number[]; lateTracks: number[]; film: number; pdf: number };
};

export function seed(): Seed {
  const state = JSON.parse(readFileSync(STATE_FILE, 'utf8')) as E2EState;
  return JSON.parse(readFileSync(join(state.root, 'data-e2e', 'seed.json'), 'utf8')) as Seed;
}
