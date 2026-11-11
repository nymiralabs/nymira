import { defineConfig, devices } from '@playwright/test';

// The e2e tests run the built app (mock profile, served with the production header set) against
// the fake NEAR RPC and a fake 1Click, with the test wallet of web-testkit (issue #86).
//   cargo and Node are needed; `web/scripts/build-wasm.sh` must have run once.
export default defineConfig({
  testDir: './e2e',
  testMatch: '**/*.spec.ts',
  // One fake chain is shared, so tests run one at a time.
  fullyParallel: false,
  workers: 1,
  reporter: 'list',
  timeout: 180_000,
  expect: { timeout: 60_000 },
  use: {
    baseURL: 'http://localhost:4173',
    trace: 'retain-on-failure',
    acceptDownloads: true,
  },
  projects: [
    { name: 'chromium', use: { ...devices['Desktop Chrome'] } },
    // WebKit runs the funding path and the restore after purged site data.
    { name: 'webkit', use: { ...devices['Desktop Safari'] }, grep: /@webkit/ },
  ],
  webServer: [
    {
      command: 'cargo run --locked -q -p nymira-fake-near-rpc -- --addr 127.0.0.1:18545',
      cwd: '..',
      url: 'http://127.0.0.1:18545/control/state',
      timeout: 600_000,
      reuseExistingServer: !process.env.CI,
    },
    {
      command: 'node e2e/servers/fake-oneclick.mjs',
      url: 'http://127.0.0.1:18547/health',
      reuseExistingServer: !process.env.CI,
    },
    {
      command: 'npm run build:mock && npx vite preview --mode mock',
      url: 'http://localhost:4173/',
      timeout: 300_000,
      reuseExistingServer: !process.env.CI,
    },
  ],
});
