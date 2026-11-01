import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './test',
  testMatch: '**/*.spec.js',
  reporter: 'list',
  use: { browserName: 'chromium' },
});
