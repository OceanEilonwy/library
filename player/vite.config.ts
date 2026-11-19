/// <reference types="vitest/config" />
import { readFileSync } from 'node:fs';
import { defineConfig } from 'vite';
import solid from '@solidjs/vite-plugin';
import { VitePWA } from 'vite-plugin-pwa';
import { mockApi } from './scripts/mock-api.ts';

const pkg = JSON.parse(readFileSync(new URL('./package.json', import.meta.url), 'utf8')) as {
  version: string;
};
// `LIBRARY_BACKEND=http://127.0.0.1:8090 npm run dev` to point at the dev server.
const backend = process.env.LIBRARY_BACKEND ?? 'http://127.0.0.1:8080';

export default defineConfig(({ mode }) => ({
  base: '/player/',
  define: {
    __APP_VERSION__: JSON.stringify(pkg.version),
    __BUILD_TIME__: JSON.stringify(new Date().toISOString()),
  },
  plugins: [
    solid(),
    // `npm run dev:mock`: a pretend server so the Player can be tried on its own.
    mode === 'mock' ? mockApi() : null,
    VitePWA({
      strategies: 'injectManifest',
      srcDir: 'src',
      filename: 'sw.ts',
      injectRegister: false,
      registerType: 'prompt',
      manifest: false,
      injectManifest: {
        globPatterns: ['**/*.{js,css,html,svg,png,webmanifest,woff2}'],
      },
      devOptions: { enabled: false },
    }),
  ],
  build: { outDir: 'dist', emptyOutDir: true, target: 'es2022' },
  server: {
    port: 5173,
    proxy: mode === 'mock' ? undefined : { '/api': backend, '/photos': backend, '/static': backend },
  },
  test: {
    environment: 'jsdom',
    globals: true,
    setupFiles: ['./src/test/setup.ts'],
    include: ['src/**/*.test.{ts,tsx}'],
  },
}));
