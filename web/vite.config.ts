import { copyFileSync, existsSync, mkdirSync, readdirSync, statSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import react from '@vitejs/plugin-react';
import type { Plugin, ResolvedConfig } from 'vite';
import { defineConfig } from 'vitest/config';
import { cloudflareHeaders, headerClasses, headersFor } from './headers';
import { canonicalHeaderSet } from './scripts/lib/integrity.mjs';
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
      // The development setup's request key: the mock profile's `provingKey` names it.
      const key = resolve(__dirname, '../vendor/zkapi/protocol/setup/v2-concession-dev/request.pk');
      mkdirSync(resolve(__dirname, 'dist/setup'), { recursive: true });
      copyFileSync(key, resolve(__dirname, 'dist/setup/request.pk'));
      copyFileSync(
        resolve(__dirname, '../vendor/zkapi/protocol/setup/v2-concession-dev/withdrawal.pk'),
        resolve(__dirname, 'dist/setup/withdrawal.pk'),
      );
    },
  };
}

// Next to a deployable bundle (`<outDir>-meta/`) the build writes the header output that the
// release step signs: the Cloudflare `_headers` file and the canonical header set (INTEGRITY.md).
// The mock bundle is never deployed and gets none.
function metaPlugin(profile: Profile): Plugin {
  let config: ResolvedConfig;
  return {
    name: 'nymira-meta',
    apply: 'build',
    configResolved(c) {
      config = c;
    },
    closeBundle() {
      if (profile.id === 'mock') return;
      const outDir = resolve(config.root, config.build.outDir);
      const rootFiles = readdirSync(outDir).filter((n) => statSync(resolve(outDir, n)).isFile());
      const meta = `${outDir}-meta`;
      mkdirSync(meta, { recursive: true });
      writeFileSync(resolve(meta, '_headers'), cloudflareHeaders(profile, rootFiles));
      writeFileSync(resolve(meta, 'headers.txt'), canonicalHeaderSet(headerClasses(profile)));
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
    plugins: [react(), headerPlugin(profile), metaPlugin(profile)],
    resolve: {
      alias: {
        '@profile': resolve(__dirname, `src/profiles/${profileId}.ts`),
        // The mock profile proves with the development setup; deployed profiles never do.
        '@zkapi': resolve(
          __dirname,
          `src/wasm/${profileId === 'mock' ? 'zkapi-browser-dev' : 'zkapi-browser'}`,
        ),
      },
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
