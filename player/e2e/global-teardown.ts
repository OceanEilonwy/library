// Stops the server started by the global setup and deletes its data.

import { existsSync, readFileSync, rmSync } from 'node:fs';
import { STATE_FILE, type E2EState } from './state';

export default async function globalTeardown(): Promise<void> {
  if (!existsSync(STATE_FILE)) return;
  const state = JSON.parse(readFileSync(STATE_FILE, 'utf8')) as E2EState;
  try {
    process.kill(state.pid, 'SIGTERM');
  } catch {
    // Already stopped.
  }
  // Give it a moment to shut down before removing its folder.
  for (let tries = 0; tries < 50; tries++) {
    try {
      process.kill(state.pid, 0);
    } catch {
      break;
    }
    await new Promise((wake) => setTimeout(wake, 100));
  }
  if (!process.env.E2E_KEEP) rmSync(state.root, { recursive: true, force: true });
  rmSync(STATE_FILE, { force: true });
}
