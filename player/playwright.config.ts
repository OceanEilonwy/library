// End-to-end tests: the real Rust server, seeded with demo data in a
// throwaway folder, driven by headless Chromium. See e2e/global-setup.ts.
//
//   npm run build        # the server serves player/dist
//   npm run test:e2e

import { defineConfig, devices } from '@playwright/test';

// Set here (not in the setup) so the test workers inherit it.
process.env.E2E_PORT ??= '18431';

export default defineConfig({
  testDir: './e2e',
  globalSetup: './e2e/global-setup.ts',
  globalTeardown: './e2e/global-teardown.ts',
  // One server, one database: tests that change shared data run one at a time.
  workers: 1,
  fullyParallel: false,
  timeout: 60_000,
  expect: { timeout: 10_000 },
  reporter: process.env.CI ? 'line' : [['list'], ['html', { open: 'never' }]],
  use: {
    baseURL: `http://127.0.0.1:${process.env.E2E_PORT}`,
    trace: 'retain-on-failure',
    serviceWorkers: 'allow',
  },
  projects: [
    {
      name: 'chromium',
      use: {
        ...devices['Desktop Chrome'],
        launchOptions: { args: ['--autoplay-policy=no-user-gesture-required', '--mute-audio'] },
      },
    },
  ],
});
