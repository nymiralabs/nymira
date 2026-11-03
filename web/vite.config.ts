import { copyFileSync, existsSync, mkdirSync } from 'node:fs';
import { resolve } from 'node:path';
import react from '@vitejs/plugin-react';
import type { Plugin } from 'vite';
import { defineConfig } from 'vitest/config';
import { headersFor } from './headers';
import { profile as mainnet } from './src/profiles/mainnet';
import { profile as mock } from './src/profiles/mock';
import { profile as testnet } from './src/profiles/testnet';
import type { Profile } from './src/profiles/types';

const PROFILES: Record<string, Profile> = { mock, testnet, mainnet };

// Serves `vite preview` with the production header set, so the e2e tests run the app under the
// strict policy. It also copies the test-only wallet executor into mock builds.
function headerPlugin(profile: Profile): Plugin {
  return {
    name: 'nymira-headers',
    configurePreviewServer(server) {
      server.middlewares.use((req, res, next) => {
        const pathname = new URL(req.url ?? '/', 'http://x').pathname;
        for (const [name, value] of Object.entries(headersFor(profile, pathname))) {
          res.setHeader(name, value);
        }
        next();
      });
    },
    closeBundle() {
      if (profile.id !== 'mock') return;
      const out = resolve(__dirname, 'dist/executors');
      mkdirSync(out, { recursive: true });
      const from = resolve(__dirname, 'test-fixtures/executors/fake-wallet.js');
      if (existsSync(from)) copyFileSync(from, resolve(out, 'fake-wallet.js'));
    },
  };
}

export default defineConfig(({ mode }) => {
  // Vitest runs with mode "test": it uses the mock profile.
  const profileId = process.env.VITEST ? 'mock' : mode;
  const profile = PROFILES[profileId];
  if (!profile) {
    throw new Error(`unknown mode "${mode}": expected one of ${Object.keys(PROFILES).join(', ')}`);
  }
  return {
    plugins: [react(), headerPlugin(profile)],
    resolve: {
      alias: { '@profile': resolve(__dirname, `src/profiles/${profileId}.ts`) },
    },
    build: {
      target: 'es2022',
      sourcemap: false,
      // Nothing is inlined: a data: URL would need its own CSP entry.
      assetsInlineLimit: 0,
      rollupOptions: {
        input: {
          index: resolve(__dirname, 'index.html'),
          'wallet-host': resolve(__dirname, 'wallet-host.html'),
        },
      },
    },
    worker: { format: 'es' },
    test: {
      include: ['test/**/*.test.ts'],
      environment: 'node',
      testTimeout: 60_000,
    },
    preview: { host: '127.0.0.1', port: 4173, strictPort: true },
    server: { host: '127.0.0.1', port: 5173 },
  };
});
